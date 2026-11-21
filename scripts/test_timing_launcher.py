#!/usr/bin/env python3
import argparse
import os
from pathlib import Path
import signal
import subprocess


def main():
    parser = argparse.ArgumentParser(description="Launch a test as a non-session-leader child.")
    parser.add_argument("--pid-file", type=Path, required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("a test binary is required")
    child = None
    pending = []

    def forward(signum, frame):
        if child is None:
            pending.append(signum)
        else:
            try:
                child.send_signal(signum)
            except ProcessLookupError:
                pass

    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, forward)
    child = subprocess.Popen(command)
    for signum in pending:
        forward(signum, None)
    try:
        args.pid_file.write_text(f"{child.pid}\n")
        code = child.wait()
    except BaseException:
        child.kill()
        child.wait()
        raise
    if code < 0:
        signum = -code
        if signum != signal.SIGKILL:
            signal.signal(signum, signal.SIG_DFL)
        os.kill(os.getpid(), signum)
    return code


if __name__ == "__main__":
    raise SystemExit(main())
