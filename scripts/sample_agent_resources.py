#!/usr/bin/env python3
"""Sample a running daemon and its current descendants without reading arguments.

Examples:
    uv run scripts/sample_agent_resources.py --pid 1234 --duration 60 --output run.csv
    uv run scripts/sample_agent_resources.py --pid 1234 --backend ps --output run.csv

Linux /proc is preferred. The dependency-free ps fallback requires a ps supporting
-e and -o pid=,ppid=,time=,rss=,lstart=,comm= (procps and BSD/macOS ps).
It has whole-second CPU and start-time precision, so rapid same-PID reuse within
one second cannot be distinguished. Neither backend reads command arguments.
RSS is KiB; summing RSS counts shared pages more than once. CPU seconds are own
user + system CPU, not child CPU counters. Tree totals sum currently visible
members, not exited processes. Short-lived children between samples are missed.
Snapshots are not atomic. Reparented processes no longer under the root are not
included. Linux start_id is boot-relative clock ticks; ps start_id is lstart.

Each sample has process rows and one tree row, all attributed to root_pid.
CPU deltas compare consecutive observations of the same (pid, start_id).
A first observation has a blank delta; tree delta sums known deltas only and
cpu_delta_complete is 0 if any member lacks a baseline. Tree accumulated CPU
may decrease as processes exit. CPU delta is not CPU percent. Sampling times
are taken before collection; sample_elapsed_seconds is the monotonic interval.
An optional UTF-8 phase file is reread each sample (missing means empty).
Use non-sensitive phase labels. The delta is labeled with the phase read at its
ending sample: boundary attribution can shift by one sampling interval (longer
if collection is delayed). Wall times are UTC ISO 8601.
The output must not exist. Each sample is flushed. SIGTERM finishes the current
sample and closes CSV; Ctrl-C also closes CSV. Sampling stops
if the root disappears or changes identity, or the optional duration expires.
"""

import argparse
import csv
from dataclasses import dataclass
from datetime import datetime, timezone
import math
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


@dataclass(frozen=True)
class Process:
    pid: int
    ppid: int
    start_id: str
    name: str
    cpu_seconds: float
    rss_kib: int

    @property
    def identity(self):
        return self.pid, self.start_id


def parse_proc_stat(text, ticks, page_size):
    prefix, tail = text.rsplit(")", 1)
    pid_text, name = prefix.split("(", 1)
    fields = tail.split()
    return Process(
        int(pid_text), int(fields[1]), fields[19], name,
        (int(fields[11]) + int(fields[12])) / ticks,
        int(fields[21]) * page_size // 1024,
    )


def read_proc(proc_root=Path("/proc")):
    ticks = os.sysconf("SC_CLK_TCK")
    page_size = os.sysconf("SC_PAGE_SIZE")
    processes = {}
    for entry in proc_root.iterdir():
        if entry.name.isdecimal():
            try:
                process = parse_proc_stat((entry / "stat").read_text(), ticks, page_size)
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
            processes[process.pid] = process
    return processes


def parse_cpu_time(value):
    days, separator, clock = value.partition("-")
    if not separator:
        clock, days = days, "0"
    parts = [float(part) for part in clock.split(":")]
    seconds = 0.0
    for part in parts:
        seconds = seconds * 60 + part
    return int(days) * 86400 + seconds


def parse_ps(text):
    processes = {}
    for line in text.splitlines():
        fields = line.split(None, 9)
        if not fields:
            continue
        pid, ppid, cpu, rss = fields[:4]
        process = Process(
            int(pid), int(ppid), " ".join(fields[4:9]),
            fields[9] if len(fields) > 9 else "", parse_cpu_time(cpu), int(rss),
        )
        processes[process.pid] = process
    return processes


def read_ps():
    result = subprocess.run(
        ["ps", "-e", "-o", "pid=,ppid=,time=,rss=,lstart=,comm="],
        check=True, capture_output=True, text=True,
        env={**os.environ, "LC_ALL": "C"},
    )
    return parse_ps(result.stdout)


def descendants(processes, root_pid):
    if root_pid not in processes:
        return []
    children = {}
    for process in processes.values():
        children.setdefault(process.ppid, []).append(process.pid)
    pending = [root_pid]
    seen = set()
    while pending:
        pid = pending.pop()
        if pid not in seen:
            seen.add(pid)
            pending.extend(children.get(pid, []))
    return [processes[pid] for pid in sorted(seen)]


FIELDS = (
    "sample", "wall_time_utc", "monotonic_seconds", "sample_elapsed_seconds",
    "phase", "backend", "root_pid", "row_type", "pid", "ppid", "start_id",
    "name", "process_count", "cpu_seconds", "cpu_delta_seconds",
    "cpu_delta_complete", "rss_kib",
)


def sample_rows(members, previous, common):
    rows = []
    known_delta = 0.0
    complete = True
    for process in members:
        baseline = previous.get(process.identity)
        delta = None if baseline is None else process.cpu_seconds - baseline
        if delta is not None and delta < 0:
            delta = None
        complete = complete and delta is not None
        known_delta += delta if delta is not None else 0.0
        rows.append({
            **common, "row_type": "process", "pid": process.pid,
            "ppid": process.ppid, "start_id": process.start_id,
            "name": "".join(c for c in process.name if c.isprintable()),
            "process_count": 1, "cpu_seconds": process.cpu_seconds,
            "cpu_delta_seconds": "" if delta is None else delta,
            "cpu_delta_complete": int(delta is not None), "rss_kib": process.rss_kib,
        })
    rows.append({
        **common, "row_type": "tree", "process_count": len(members),
        "cpu_seconds": sum(p.cpu_seconds for p in members),
        "cpu_delta_seconds": known_delta, "cpu_delta_complete": int(complete),
        "rss_kib": sum(p.rss_kib for p in members),
    })
    return rows


def positive(value):
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise argparse.ArgumentTypeError("must be a finite positive number")
    return number


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--pid", type=int, required=True, help="running daemon root PID")
    parser.add_argument("--interval", type=positive, default=1.0, help="seconds between samples (default: 1)")
    parser.add_argument("--duration", type=positive, help="seconds to sample; omit to run until interrupted")
    parser.add_argument("--output", type=Path, required=True, help="new CSV path (never overwritten)")
    parser.add_argument("--phase-file", type=Path, help="UTF-8 phase label file, reread every sample")
    parser.add_argument("--backend", choices=("auto", "proc", "ps"), default="auto")
    args = parser.parse_args(argv)
    if args.pid <= 0:
        parser.error("--pid must be positive")
    backend = args.backend
    if backend == "auto":
        backend = "proc" if sys.platform.startswith("linux") and Path("/proc/self/stat").exists() else "ps"
    collect = read_proc if backend == "proc" else read_ps
    initial = collect()
    if args.pid not in initial:
        parser.error("root PID is not visible")
    root_identity = initial[args.pid].identity
    previous = {}
    started = time.monotonic()
    last_sample = None
    sample = 0
    stopping = False

    def request_stop(signum, frame):
        nonlocal stopping
        stopping = True

    previous_sigterm = signal.signal(signal.SIGTERM, request_stop)
    try:
        with args.output.open("x", newline="", encoding="utf-8") as output:
            writer = csv.DictWriter(output, fieldnames=FIELDS)
            writer.writeheader()
            while not stopping:
                now = time.monotonic()
                if sample and args.duration is not None and now - started >= args.duration:
                    break
                wall_time = datetime.now(timezone.utc).isoformat()
                processes = collect()
                root = processes.get(args.pid)
                if root is None or root.identity != root_identity:
                    break
                members = descendants(processes, args.pid)
                phase = ""
                if args.phase_file is not None:
                    try:
                        phase = args.phase_file.read_text(encoding="utf-8").strip()
                    except FileNotFoundError:
                        pass
                common = {
                    "sample": sample, "wall_time_utc": wall_time,
                    "monotonic_seconds": now,
                    "sample_elapsed_seconds": "" if last_sample is None else now - last_sample,
                    "phase": phase, "backend": backend, "root_pid": args.pid,
                }
                writer.writerows(sample_rows(members, previous, common))
                output.flush()
                previous = {p.identity: p.cpu_seconds for p in members}
                last_sample = now
                sample += 1
                delay = max(0.0, args.interval - (time.monotonic() - now))
                if args.duration is not None:
                    delay = min(delay, max(0.0, started + args.duration - time.monotonic()))
                if not stopping:
                    time.sleep(delay)
    except KeyboardInterrupt:
        pass
    finally:
        signal.signal(signal.SIGTERM, previous_sigterm)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
