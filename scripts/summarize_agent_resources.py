#!/usr/bin/env python3
"""Summarize a private daemon resource experiment as JSON.

CPU is attributed to the phase at each sample's ending boundary. Sample zero
establishes the baseline; later first-seen identities contribute their full CPU.
This assumes every post-baseline first observation is a newly born process.
The last observed CPU of an exited process remains counted, but CPU after its
last observation and processes never observed are missing. Endpoint receipt CPU
and sampled CPU have different boundaries. Neither is an exact turn measurement.
RSS sums include shared-page duplication. Worker subtrees use each snapshot's
recorded parent edges, excluding sibling workers. No command arguments are read.
"""

import argparse
import csv
from itertools import groupby
import json
from pathlib import Path
import statistics


def subtree_pids(processes, worker_pid):
    if worker_pid not in processes:
        return set()
    children = {}
    for pid, process in processes.items():
        children.setdefault(int(process["ppid"]), []).append(pid)
    pending = [worker_pid]
    found = set()
    while pending:
        pid = pending.pop()
        if pid not in found:
            found.add(pid)
            pending.extend(children.get(pid, []))
    return found


def summarize(rows, receipt):
    workers = {row["sessionName"]: int(row["workerPid"]) for row in receipt["sessions"]
               if row.get("sessionName") and row.get("workerPid") is not None}
    turns_by_phase = {}
    for turn in receipt["turns"]:
        phase = turn["phase"]
        name = ("measure-one" if phase.startswith("child_one") else
                "measure-two" if phase.startswith("child_two") else "measure-root")
        turns_by_phase[phase] = workers[name]
    phases = {}
    previous = {}
    ordered = sorted(rows, key=lambda row: int(row["sample"]))
    for sample_index, (_, grouped) in enumerate(groupby(ordered, key=lambda row: row["sample"])):
        sample_rows = list(grouped)
        tree, = [row for row in sample_rows if row["row_type"] == "tree"]
        phase = tree["phase"]
        values = phases.setdefault(phase, {
            "counts": [], "rss": [], "tree_cpu": 0.0, "agent_cpu": 0.0, "agent_rss": [],
        })
        values["counts"].append(int(tree["process_count"]))
        values["rss"].append(int(tree["rss_kib"]) / 1024)
        processes = {int(row["pid"]): row for row in sample_rows if row["row_type"] == "process"}
        agent = subtree_pids(processes, turns_by_phase.get(phase))
        for pid, process in processes.items():
            identity = (pid, process["start_id"])
            cpu = float(process["cpu_seconds"])
            baseline = previous.get(identity, cpu if sample_index == 0 else 0.0)
            delta = cpu - baseline
            if delta < 0:
                raise ValueError(f"CPU counter decreased for {identity}")
            previous[identity] = cpu
            values["tree_cpu"] += delta
            if pid in agent:
                values["agent_cpu"] += delta
        values["agent_rss"].append(sum(int(processes[pid]["rss_kib"]) for pid in agent) / 1024)
    phase_summary = {
        phase: {
            "samples": len(values["counts"]),
            "process_count_min": min(values["counts"]),
            "process_count_max": max(values["counts"]),
            "tree_rss_median_mib": statistics.median(values["rss"]),
            "tree_rss_max_mib": max(values["rss"]),
        }
        for phase, values in phases.items()
    }
    turns = []
    for turn in receipt["turns"]:
        phase = turn["phase"]
        values = phases.get(phase)
        turns.append({
            "phase": phase, "worker_pid": turns_by_phase[phase],
            "samples": 0 if values is None else len(values["counts"]),
            "wall_seconds": turn["wall_seconds"],
            "boundary_tree_cpu_seconds": turn["tree_cpu_seconds"],
            "sampled_tree_cpu_seconds": None if values is None else values["tree_cpu"],
            "sampled_agent_subtree_cpu_seconds": None if values is None else values["agent_cpu"],
            "agent_subtree_peak_rss_mib": None if values is None else max(values["agent_rss"]),
        })
    return {"phases": phase_summary, "turns": turns}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--samples", required=True, type=Path)
    parser.add_argument("--receipt", required=True, type=Path)
    args = parser.parse_args()
    with args.samples.open(newline="", encoding="utf-8") as handle:
        rows = list(csv.DictReader(handle))
    receipt = json.loads(args.receipt.read_text(encoding="utf-8"))
    print(json.dumps(summarize(rows, receipt), indent=2, allow_nan=False))


if __name__ == "__main__":
    main()
