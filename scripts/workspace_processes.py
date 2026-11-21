import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import time


TERM_SECONDS = 1.0
KILL_SECONDS = 1.0


class ProcessLeaks(RuntimeError):
    pass


def interrupted(signum, frame):
    raise KeyboardInterrupt(f"workspace tests interrupted by signal {signum}")


def root_processes(roots):
    patterns = [re.compile(re.escape(os.fsencode(root.absolute())) + rb"(?=/|$|[\x00\s:\'\"])")
                for root in roots]
    found = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit() or int(entry.name) == os.getpid():
            continue
        try:
            if entry.stat().st_uid != os.getuid():
                continue
            stat = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            if stat[0] == "Z":
                continue
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
        fields = {}
        for name in ("cmdline", "environ", "cwd"):
            try:
                fields[name] = (os.fsencode(os.readlink(entry / name)) if name == "cwd"
                                else (entry / name).read_bytes())
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
        if any(pattern.search(field) for pattern in patterns for field in fields.values()):
            found[int(entry.name)] = (stat[19], fields.get("cmdline", b"<unreadable>"))
    return found


def live_processes(processes):
    live = {}
    for pid, (started, argv) in processes.items():
        try:
            stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
            if stat[19] == started and stat[0] != "Z":
                live[pid] = (started, argv)
        except (FileNotFoundError, ProcessLookupError):
            pass
    return live


def signal_processes(processes, signum):
    for pid in live_processes(processes):
        try:
            os.kill(pid, signum)
        except ProcessLookupError:
            pass


def finish_roots(roots, *, remove=False):
    roots = list(roots)
    if not roots:
        return
    leaked = root_processes(roots)
    if leaked:
        print("Leaked test processes:", flush=True)
        for pid, (_, argv) in sorted(leaked.items()):
            command = shlex.join(os.fsdecode(arg) for arg in argv.split(b"\0") if arg)
            print(f"  pid={pid} argv={command}", flush=True)
        remaining = leaked
        for signum, seconds in ((signal.SIGTERM, TERM_SECONDS),
                                (signal.SIGKILL, KILL_SECONDS)):
            deadline = time.monotonic() + seconds
            while remaining:
                signal_processes(remaining, signum)
                remaining = live_processes(remaining) | root_processes(roots)
                if not remaining or time.monotonic() >= deadline:
                    break
                time.sleep(0.02)
    else:
        remaining = {}
    if remaining:
        print("Cleanup incomplete; retained test roots: " + ", ".join(map(str, roots)),
              flush=True)
        for pid, (_, argv) in sorted(remaining.items()):
            print(f"  surviving pid={pid} argv={argv!r}", flush=True)
    elif remove:
        for root in roots:
            shutil.rmtree(root)
    if leaked:
        raise ProcessLeaks("test process leak detected" +
                           ("; cleanup incomplete" if remaining else "; cleaned up"))
