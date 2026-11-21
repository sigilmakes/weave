#!/usr/bin/env python3
import copy
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest

from test_timings import Children, load_timing_run, reconcile, span_totals, statuses


def target(required=True):
    return {"inventory": ["module::normal", "module::panic", "module::ignored"],
            "ignored": ["module::ignored"], "timing_required": required,
            "statuses": [("module::normal", "ok"), ("module::panic", "ok"),
                         ("module::ignored", "ignored")]}


def record(name, panic=False):
    return {"kind": "test", "test": name, "binary": "fixture", "pid": 123,
            "start_unix_ns": 100, "end_unix_ns": 200, "elapsed_ns": 100,
            "panicking": panic, "outcome": "panic" if panic else "returned", "spans": []}


class ChildOwnership(unittest.TestCase):
    def setUp(self):
        private = tempfile.TemporaryDirectory(prefix="pa-timing-test-")
        self.addCleanup(private.cleanup)
        self.cwd = Path(private.name)
        home = self.cwd / "home"
        temporary = self.cwd / "tmp"
        home.mkdir()
        temporary.mkdir()
        self.env = {"HOME": str(home), "TMPDIR": str(temporary),
                    "TMP": str(temporary), "TEMP": str(temporary)}

    def test_main_thread_child_inherits_unblocked_interrupt_and_term(self):
        children = Children()
        child = children.spawn(
            [sys.executable, "-I", "-c", "import json, signal; print(json.dumps([int(s) for s in signal.pthread_sigmask(signal.SIG_BLOCK, [])]))"],
            cwd=self.cwd, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            children.resume_signals()
            stdout, stderr = child.communicate()
        finally:
            child.wait()
            children.complete(child)
        self.assertEqual((child.returncode, stderr), (0, ""))
        self.assertFalse({signal.SIGINT, signal.SIGTERM} & set(json.loads(stdout)))
        self.assertFalse(children.active)

    def test_launcher_child_can_create_its_own_session(self):
        children = Children()
        pid_file = self.cwd / "binary.pid"
        launcher = Path(__file__).with_name("test_timing_launcher.py")
        code = "import json, os; os.setsid(); print(json.dumps({'pid': os.getpid(), 'sid': os.getsid(0)}))"
        child = children.spawn(
            [sys.executable, "-I", str(launcher), "--pid-file", str(pid_file), "--",
             sys.executable, "-I", "-c", code], cwd=self.cwd, env=self.env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            children.resume_signals()
            stdout, stderr = child.communicate()
        finally:
            child.wait()
            children.complete(child)
        self.assertEqual((child.returncode, stderr), (0, ""))
        actual_pid = int(pid_file.read_text())
        self.assertNotEqual(actual_pid, child.pid)
        self.assertEqual(json.loads(stdout), {"pid": actual_pid, "sid": actual_pid})

    def test_launcher_forwards_term_to_child_in_separate_session(self):
        children = Children()
        pid_file = self.cwd / "binary.pid"
        launcher = Path(__file__).with_name("test_timing_launcher.py")
        child = children.spawn(
            [sys.executable, "-I", str(launcher), "--pid-file", str(pid_file), "--",
             sys.executable, "-I", "-c", "import os, signal; os.setsid(); print('ready', flush=True); signal.pause()"],
            cwd=self.cwd, env=self.env, stdout=subprocess.PIPE, text=True)

        def reap():
            try:
                return child.wait()
            finally:
                children.complete(child)

        try:
            with ThreadPoolExecutor(max_workers=1) as workers:
                future = workers.submit(reap)
                try:
                    children.resume_signals()
                    self.assertEqual(child.stdout.readline(), "ready\n")
                    actual_pid = int(pid_file.read_text())
                    self.assertEqual(os.getsid(actual_pid), actual_pid)
                    self.assertNotEqual(actual_pid, child.pid)
                finally:
                    children.cancel()
                self.assertEqual(future.result(), -signal.SIGTERM)
            self.assertFalse(children.active)
        finally:
            child.stdout.close()

    def test_cancel_signals_only_tracked_group_and_owner_reaps(self):
        children = Children()
        child = children.spawn(
            [sys.executable, "-I", "-c", "import signal; print('ready', flush=True); signal.pause()"],
            cwd=self.cwd, env=self.env, stdout=subprocess.PIPE, text=True)

        def reap():
            try:
                _, status, usage = os.wait4(child.pid, 0)
                child.returncode = os.waitstatus_to_exitcode(status)
                return child.returncode, usage
            finally:
                children.complete(child)

        try:
            with ThreadPoolExecutor(max_workers=1) as workers:
                future = workers.submit(reap)
                try:
                    children.resume_signals()
                    self.assertEqual(child.stdout.readline(), "ready\n")
                    self.assertEqual(os.getpgid(child.pid), child.pid)
                finally:
                    children.cancel()
                code, usage = future.result()
            self.assertEqual(code, -signal.SIGTERM)
            self.assertGreaterEqual(usage.ru_utime, 0)
            self.assertFalse(children.active)
            with self.assertRaises(RuntimeError):
                children.spawn([sys.executable, "-I", "-c", "pass"], cwd=self.cwd, env=self.env)
        finally:
            child.stdout.close()


class TimingReports(unittest.TestCase):
    def test_harness_status_keeps_expected_panic_success_separate_from_guard(self):
        fixture = target()
        fixture["statuses"] = statuses(
            "running 3 tests\n"
            "test module::normal ... ok\n"
            "test module::panic - should panic ... ok\n"
            "test module::ignored ... ignored, fixture\n"
            "failures:\n"
            "test bogus::captured_stdout ... FAILED\n"
            "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
            fixture["inventory"], fixture["ignored"])
        records = [record("module::normal"), record("module::panic", panic=True)]
        self.assertEqual(reconcile(fixture, records, disabled=False), [])
        self.assertEqual(dict(fixture["statuses"])[records[1]["test"]], "ok")
        self.assertEqual(records[1]["outcome"], "panic")

    def test_nested_failed_child_does_not_override_passing_parent_summary(self):
        fixture = target()
        log = (
            "running 3 tests\n"
            "running 1 test\n"
            "test module::normal ... FAILED\n"
            "failures:\n\n    module::normal\n\n"
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.01s\n"
            "test module::normal ... ok\n"
            "test module::normal ... ok\n"
            "test module::panic - should panic ... ok\n"
            "test module::ignored ... ignored\n"
            "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.02s\n")
        self.assertEqual(dict(statuses(log, fixture["inventory"], fixture["ignored"])),
                         dict(fixture["statuses"]))

    def test_failed_parent_uses_only_final_failure_name_list(self):
        fixture = target()
        log = (
            "running 3 tests\n"
            "failures:\n\n---- module::panic stdout ----\n"
            "failures:\n\n    module::panic\n\n"
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.01s\n"
            "failures:\n\n    module::normal\n\n"
            "test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.02s\n")
        self.assertEqual(dict(statuses(log, fixture["inventory"], fixture["ignored"])),
                         {"module::normal": "FAILED", "module::panic": "ok", "module::ignored": "ignored"})

    def test_missing_filtered_or_mismatched_summary_cannot_claim_completion(self):
        fixture = target()
        for log in (
                "test module::normal ... ok\n",
                "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.01s\n",
                "test result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
                "failures:\n\n    module::panic\n\n    module::normal\n\n"
                "test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n"):
            with self.subTest(log=log), self.assertRaises(ValueError):
                statuses(log, fixture["inventory"], fixture["ignored"])

    def test_integrations_require_one_record_per_completed_test(self):
        records = [record("module::normal"), record("module::panic", panic=True)]
        self.assertEqual(reconcile(target(), records, disabled=False), [])
        for invalid in (records[:1], records + records[:1], records + [record("unexpected")]):
            with self.subTest(records=invalid):
                self.assertTrue(reconcile(target(), invalid, disabled=False))

    def test_units_report_uncovered_but_strict_mode_rejects(self):
        fixture = target(required=False)
        self.assertEqual(reconcile(fixture, [], disabled=False), [])
        self.assertEqual(fixture["uncovered_tests"], ["module::normal", "module::panic"])
        strict = copy.deepcopy(fixture)
        strict["timing_required"] = True
        self.assertTrue(reconcile(strict, [], disabled=False))

    def test_raw_sidecars_group_by_binary_and_pid_without_inventing_status(self):
        first = record("module::normal")
        second = record("module::panic", panic=True)
        second["pid"] = 456
        with tempfile.TemporaryDirectory() as state:
            directory = Path(state)
            (directory / "fixture-123.jsonl").write_text(json.dumps(first) + "\n")
            (directory / "fixture-456.jsonl").write_text(json.dumps(second) + "\n")
            manifest = load_timing_run(directory)
        self.assertTrue(manifest["coverage_unverified"])
        self.assertEqual({row["pid"] for row in manifest["targets"]}, {123, 456})
        self.assertEqual([row["statuses"] for row in manifest["targets"]], [[], []])
        self.assertEqual(sum(len(row["records"]) for row in manifest["targets"]), 2)
        self.assertTrue(all(row["coverage_unverified"] for row in manifest["targets"]))

    def test_completion_is_independent_of_passing_and_detects_early_abort(self):
        fixture = target(required=False)
        fixture["statuses"][0] = ("module::normal", "FAILED")
        reconcile(fixture, [], disabled=False)
        self.assertTrue(fixture["whole_test_set_completed"])
        self.assertFalse(fixture["aborted_early"])
        self.assertEqual(fixture["failed_phases"], {"module::normal": ["unknown"]})
        fixture["statuses"].pop(1)
        reconcile(fixture, [], disabled=False)
        self.assertFalse(fixture["whole_test_set_completed"])
        self.assertTrue(fixture["aborted_early"])
        self.assertEqual((fixture["completed_count"], fixture["expected_nonignored_count"]), (1, 2))

    def test_nested_and_overlapping_spans_do_not_double_count(self):
        timing = record("module::normal")
        timing["spans"] = [
            {"name": "outer", "start_unix_ns": 110, "end_unix_ns": 180, "elapsed_ns": 70},
            {"name": "inner", "start_unix_ns": 120, "end_unix_ns": 140, "elapsed_ns": 20},
            {"name": "inner", "start_unix_ns": 130, "end_unix_ns": 150, "elapsed_ns": 20},
            {"name": "outer", "start_unix_ns": 185, "end_unix_ns": 190, "elapsed_ns": 5},
        ]
        totals, unattributed = span_totals(timing)
        self.assertEqual((totals, unattributed), ({"inner": 30, "outer": 75}, 25))
        self.assertEqual(reconcile({"inventory": ["module::normal"], "ignored": [],
                                    "statuses": [("module::normal", "ok")], "timing_required": True},
                                   [timing], disabled=False), [])


if __name__ == "__main__":
    unittest.main()
