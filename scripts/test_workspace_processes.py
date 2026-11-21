import contextlib
import io
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import workspace_processes as audit


CHILD = (
    "import signal; "
    "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
    "print('ready', flush=True); signal.pause()"
)


@unittest.skipUnless(sys.platform == "linux", "ownership audit uses Linux procfs")
class WorkspaceProcessAudit(unittest.TestCase):
    def test_detects_argv_cwd_and_environment_and_kills_stubborn_process(self):
        for source in ("argv", "cwd", "environment"):
            with self.subTest(source=source), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp) / "run"
                root.mkdir()
                command = [sys.executable, "-c", CHILD]
                env = dict(os.environ)
                cwd = None
                if source == "argv":
                    command.append(str(root / "fixture"))
                elif source == "cwd":
                    cwd = root
                else:
                    env["PA_TEST_SCRATCH_ROOT"] = str(root)
                child = subprocess.Popen(command, cwd=cwd, env=env,
                                         stdout=subprocess.PIPE, text=True,
                                         start_new_session=True)
                try:
                    self.assertEqual(child.stdout.readline().strip(), "ready")
                    output = io.StringIO()
                    with contextlib.redirect_stdout(output):
                        with self.assertRaisesRegex(audit.ProcessLeaks, "cleaned up"):
                            audit.finish_roots([root], remove=True)
                    self.assertEqual(child.wait(timeout=2), -signal.SIGKILL)
                    self.assertIn(f"pid={child.pid} argv=", output.getvalue())
                    self.assertFalse(root.exists())
                finally:
                    if child.poll() is None:
                        child.kill()
                        child.wait(timeout=2)
                    child.stdout.close()

    def test_killed_test_parent_does_not_hide_detached_fixture(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            code = (
                "import os, signal, subprocess, sys; "
                f"child=subprocess.Popen([sys.executable, '-c', {CHILD!r}], "
                "stdout=subprocess.PIPE, text=True, start_new_session=True); "
                "assert child.stdout.readline().strip() == 'ready'; "
                "print(child.pid, flush=True); signal.pause()"
            )
            parent = subprocess.Popen([sys.executable, "-c", code],
                env=dict(os.environ, PA_TEST_SCRATCH_ROOT=str(root)),
                stdout=subprocess.PIPE, text=True, start_new_session=True)
            orphan_identity = {}
            try:
                orphan = int(parent.stdout.readline())
                orphan_identity = {orphan: audit.root_processes([root])[orphan]}
                parent.kill()
                parent.wait(timeout=2)
                output = io.StringIO()
                with contextlib.redirect_stdout(output):
                    with self.assertRaisesRegex(audit.ProcessLeaks, "cleaned up"):
                        audit.finish_roots([root])
                self.assertIn(f"pid={orphan} argv=", output.getvalue())
                self.assertEqual(audit.root_processes([root]), {})
            finally:
                if parent.poll() is None:
                    parent.kill()
                    parent.wait(timeout=2)
                parent.stdout.close()
                audit.signal_processes(orphan_identity, signal.SIGKILL)

    def test_unreadable_fields_do_not_hide_other_root_references(self):
        from types import SimpleNamespace
        from unittest.mock import Mock

        process = Mock(name="process")
        process.name = "123"
        process.stat.return_value = SimpleNamespace(st_uid=os.getuid())
        stat = "123 (fixture) S " + " ".join(["0"] * 18 + ["42"])
        for unreadable in ("cmdline", "environ", "cwd"):
            for matched in {"cmdline", "environ", "cwd"} - {unreadable}:
                with self.subTest(unreadable=unreadable, matched=matched):
                    entries = {name: Mock(name=name) for name in
                               ("stat", "cmdline", "environ", "cwd")}
                    entries["stat"].read_text.return_value = stat
                    for name in ("cmdline", "environ"):
                        entries[name].read_bytes.return_value = (
                            b"/tmp/run/fixture" if name == matched else b"fixture")
                    if unreadable != "cwd":
                        entries[unreadable].read_bytes.side_effect = PermissionError()
                    process.__truediv__ = Mock(side_effect=entries.__getitem__)
                    cwd = (PermissionError() if unreadable == "cwd" else
                           "/tmp/run/fixture" if matched == "cwd" else "/elsewhere")
                    with patch.object(Path, "iterdir", return_value=[process]), \
                            patch.object(audit.os, "readlink", side_effect=[cwd]):
                        found = audit.root_processes([Path("/tmp/run")])
                    self.assertIn(123, found)

    def test_incomplete_cleanup_fails_and_retains_root(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            observed = {123: ("42", b"stubborn\0fixture\0")}
            output = io.StringIO()
            with patch.object(audit, "root_processes", return_value=observed), \
                    patch.object(audit, "signal_processes") as send, \
                    patch.object(audit, "TERM_SECONDS", 0), \
                    patch.object(audit, "KILL_SECONDS", 0), \
                    contextlib.redirect_stdout(output):
                with self.assertRaisesRegex(audit.ProcessLeaks, "cleanup incomplete"):
                    audit.finish_roots([root], remove=True)
            self.assertTrue(root.exists())
            self.assertEqual([call.args[1] for call in send.call_args_list],
                             [signal.SIGTERM, signal.SIGKILL])
            self.assertIn("surviving pid=123 argv=", output.getvalue())
            self.assertIn(str(root), output.getvalue())

    def test_root_name_prefix_does_not_claim_another_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "run"
            sibling = Path(tmp) / "run-other"
            sibling.mkdir()
            child = subprocess.Popen([sys.executable, "-c", CHILD], cwd=sibling,
                                     stdout=subprocess.PIPE, text=True)
            try:
                self.assertEqual(child.stdout.readline().strip(), "ready")
                self.assertEqual(audit.root_processes([root]), {})
            finally:
                child.kill()
                child.wait(timeout=2)
                child.stdout.close()


if __name__ == "__main__":
    unittest.main()
