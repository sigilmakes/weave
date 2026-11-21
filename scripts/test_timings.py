#!/usr/bin/env python3
"""Developer-only whole-binary timing runs. Doctests run last, timing uncovered.

Requires a Unix host with os.wait4 (the supported developer host is Linux).
Use a qualified, synced kernel and private CARGO_HOME/CARGO_TARGET_DIR, as for
check_workspace_tests.py. This runner never filters, retries, or changes native
libtest concurrency. run.json retains commands, isolated environments and logs.
"""
import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor, as_completed
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from threading import Event, RLock, current_thread, main_thread

from check_workspace_tests import CLEANUP_SECONDS, EXCLUSIVE, target_environment, target_groups


class Children:
    def __init__(self):
        self.lock = RLock()
        self.active = {}
        self.cancelled = False
        self.launching_main = False
        self.pending_signal = None

    def spawn(self, command, **kwargs):
        if current_thread() is main_thread():
            self.launching_main = True
        try:
            with self.lock:
                if self.cancelled:
                    raise RuntimeError("run cancelled before child launch")
                child = subprocess.Popen(command, start_new_session=True, **kwargs)
                self.active[child.pid] = Event()
            return child
        except BaseException:
            self.resume_signals()
            raise

    def resume_signals(self):
        if current_thread() is main_thread():
            self.launching_main = False
            pending, self.pending_signal = self.pending_signal, None
            if pending is not None:
                signal.raise_signal(pending)

    def complete(self, child):
        with self.lock:
            event = self.active.pop(child.pid)
            event.set()

    def cancel(self):
        previous = None
        if current_thread() is main_thread():
            previous = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
        try:
            with self.lock:
                if self.cancelled:
                    return
                self.cancelled = True
                children = list(self.active.items())
            for pid, _ in children:
                try:
                    os.killpg(pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            deadline = time.monotonic() + CLEANUP_SECONDS
            for _, done in children:
                done.wait(max(0, deadline - time.monotonic()))
            for pid, _ in children:
                try:
                    os.killpg(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        finally:
            if previous is not None:
                signal.pthread_sigmask(signal.SIG_SETMASK, previous)


def snapshot():
    return {"unix_ns": time.time_ns(), "load_average": os.getloadavg(),
            "cpu_count": os.cpu_count()}


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def release_state(state, record, cancelled):
    record["state_retained"] = cancelled
    if cancelled:
        return
    try:
        shutil.rmtree(state)
    except OSError as error:
        record["cleanup_error"] = f"{type(error).__name__}: {error}"
        record["state_retained"] = True


def environment(repo, state, ambient):
    env = target_environment(repo, state, ambient)
    env.pop("RUST_TEST_THREADS", None)
    if "RUST_TEST_THREADS" in ambient:
        env["RUST_TEST_THREADS"] = ambient["RUST_TEST_THREADS"]
    env["PA_CORE_KERNEL_PYTHON"] = ambient["PA_CORE_KERNEL_PYTHON"]
    return env


def configure_timing(env, directory, disabled):
    env.pop("PA_TEST_TIMING", None)
    env.pop("PA_TEST_TIMING_DIR", None)
    if not disabled:
        directory.mkdir()
        env["PA_TEST_TIMING_DIR"] = str(directory)


def checked(command, cwd, env, log, commands, children):
    commands.append({"command": command, "cwd": str(cwd), "environment": env})
    child = children.spawn(command, cwd=cwd, env=env, stdout=subprocess.PIPE,
                           stderr=subprocess.PIPE, text=True)
    try:
        try:
            children.resume_signals()
            stdout, stderr = child.communicate()
        except BaseException:
            children.cancel()
            stdout, stderr = child.communicate()
            log.write_text(stdout + stderr)
            raise
        log.write_text(stdout + stderr)
    finally:
        children.complete(child)
    if child.returncode:
        raise RuntimeError(f"command failed ({child.returncode}); see {log}")
    return stdout


def inventory(binary, cwd, env, directory, commands, children):
    inventories = []
    for ignored in (False, True):
        command = [binary, "--list", "--format", "terse"]
        if ignored:
            command.append("--ignored")
        output = checked(command, cwd, env,
                         directory / ("ignored.log" if ignored else "inventory.log"), commands, children)
        names = [line[:-6] for line in output.splitlines() if line.endswith(": test")]
        if len(names) != len(set(names)):
            raise ValueError(f"duplicate inventory entries for {binary}")
        inventories.append(set(names))
    all_names, ignored = inventories
    if not ignored <= all_names:
        raise ValueError(f"ignored inventory not a subset for {binary}")
    return sorted(all_names), sorted(ignored)


def statuses(log, inventory, ignored):
    pattern = re.compile(
        r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; "
        r"(\d+) measured; (\d+) filtered out; finished in [^\n]*$", re.MULTILINE)
    summaries = list(pattern.finditer(log))
    if not summaries:
        raise ValueError("parent harness final summary missing; completion unknown")
    summary = summaries[-1]
    outcome = summary[1]
    passed, failed, ignored_count, measured, filtered = map(int, summary.groups()[1:])
    expected = set(inventory) - set(ignored)
    if (passed + failed != len(expected) or ignored_count != len(ignored)
            or measured or filtered or (outcome == "ok") != (failed == 0)):
        raise ValueError("parent harness final summary does not match unfiltered inventory")
    failed_names = []
    if failed:
        preceding = log[:summary.start()].rstrip()
        marker = preceding.rfind("\nfailures:\n")
        block = preceding[marker + 1:] if marker >= 0 else preceding
        lines = block.splitlines()
        if not lines or lines[0] != "failures:" or any(
                line and not line[0].isspace() for line in lines[1:]):
            raise ValueError("parent harness final failure-name list missing")
        failed_names = [line.strip() for line in lines[1:] if line.strip()]
        if (len(failed_names) != failed or len(set(failed_names)) != failed
                or not set(failed_names) <= expected):
            raise ValueError("parent harness final failure-name list disagrees with summary/inventory")
    return [(name, "ignored" if name in ignored else "FAILED" if name in failed_names else "ok")
            for name in sorted(inventory)]


def read_records(directory, binary, pid):
    path = directory / f"{Path(binary).name}-{pid}.jsonl"
    records = []
    if path.exists():
        for line in path.read_text().splitlines():
            record = json.loads(line)
            if record.get("pid") != pid:
                raise ValueError(f"foreign pid in {path}")
            records.append(record)
    return records


def reconcile(target, records, disabled):
    errors = []
    expected = set(target["inventory"]) - set(target["ignored"])
    completed = target["statuses"]
    all_counts = Counter(name for name, _ in completed)
    if set(all_counts) - set(target["inventory"]):
        errors.append("unexpected harness status names")
    if any(count != 1 for count in all_counts.values()):
        errors.append("duplicate harness status names")
    counts = Counter(name for name, status in completed if status != "ignored")
    target["expected_nonignored_count"] = len(expected)
    target["completed_count"] = sum(counts.values())
    target["whole_test_set_completed"] = counts == Counter({name: 1 for name in expected})
    target["aborted_early"] = not target["whole_test_set_completed"]
    target["failed_phases"] = {name: ["unknown"] for name, status in completed if status == "FAILED"}
    if set(counts) != expected:
        errors.append(f"completion mismatch: missing={sorted(expected - counts.keys())}, "
                      f"unexpected={sorted(counts.keys() - expected)}")
    if any(count != 1 for count in counts.values()):
        errors.append("duplicate harness completions")
    recorded = Counter(record.get("test") for record in records)
    uncovered = sorted(expected - recorded.keys())
    target["uncovered_tests"] = uncovered
    required = target["timing_required"]
    if not disabled and ((required and uncovered) or recorded.keys() - expected):
        errors.append(f"record mismatch: missing={sorted(expected - recorded.keys())}, "
                      f"unexpected={sorted(recorded.keys() - expected, key=str)}")
    if disabled and records:
        errors.append("timing disabled but records were emitted")
    if any(count != 1 for count in recorded.values()):
        errors.append("duplicate timing records")
    for record in records:
        start, end = record["start_unix_ns"], record["end_unix_ns"]
        if (record.get("kind") != "test" or end < start
                or record["elapsed_ns"] < 0
                or not isinstance(record.get("binary"), str)
                or record.get("outcome") not in ("panic", "returned")
                or record.get("panicking") != (record.get("outcome") == "panic")):
            errors.append(f"invalid record: {record.get('test')}")
        failed_spans = [span["name"] for span in record["spans"] if span.get("panicking") is True]
        if record.get("test") in target["failed_phases"] and failed_spans:
            target["failed_phases"][record["test"]] = failed_spans
        for span in record["spans"]:
            if (not isinstance(span["name"], str)
                    or not start <= span["start_unix_ns"] <= span["end_unix_ns"] <= end
                    or span["elapsed_ns"] < 0):
                errors.append(f"invalid span: {record.get('test')}")
    return errors


def execute(target, repo, ambient, run_dir, disabled, children):
    directory = run_dir / target["id"]
    directory.mkdir()
    private = Path(tempfile.mkdtemp(prefix="pa-t-", dir="/tmp"))
    target.update(state_root=str(private), commands=[], load_before=snapshot(),
                  exit_code=None, expected_nonignored_count=None, completed_count=0,
                  whole_test_set_completed=False, aborted_early=True, failed_phases={}, errors=[])
    try:
        env = environment(repo, private, ambient)
        timing_dir = directory / "records"
        configure_timing(env, timing_dir, disabled)
        deps = str(Path(target["binary"]).parent)
        loader = "DYLD_FALLBACK_LIBRARY_PATH" if os.uname().sysname == "Darwin" else "LD_LIBRARY_PATH"
        env[loader] = os.pathsep.join(filter(None, [deps, env.get(loader)]))
        cwd = Path(target["cwd"])
        target["environment"] = env
        target["inventory"], target["ignored"] = inventory(
            target["binary"], cwd, env, directory, target["commands"], children)
        target["expected_nonignored_count"] = len(set(target["inventory"]) - set(target["ignored"]))
        pid_file = private / "binary.pid"
        command = [sys.executable, "-I", str(Path(__file__).with_name("test_timing_launcher.py")),
                   "--pid-file", str(pid_file), "--", target["binary"]]
        target["pid_file"] = str(pid_file)
        target["commands"].append({"command": command, "cwd": str(cwd), "environment": env})
        with (directory / "harness.log").open("w") as output:
            started = time.monotonic_ns()
            target["start_unix_ns"] = time.time_ns()
            target["start_monotonic_ns"] = started
            child = children.spawn(command, cwd=cwd, env=env, stdout=output,
                                   stderr=subprocess.STDOUT)
            target["launcher_pid"] = child.pid
            try:
                children.resume_signals()
                _, status, usage = os.wait4(child.pid, 0)
            except BaseException:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                _, status, usage = os.wait4(child.pid, 0)
                child.returncode = os.waitstatus_to_exitcode(status)
                raise
            finally:
                children.complete(child)
            child.returncode = os.waitstatus_to_exitcode(status)
            target.update(exit_code=child.returncode, elapsed_ns=time.monotonic_ns() - started,
                          end_unix_ns=time.time_ns(), cpu_user_seconds=usage.ru_utime,
                          cpu_system_seconds=usage.ru_stime)
        target["pid"] = int(pid_file.read_text().strip())
        try:
            target["statuses"] = statuses((directory / "harness.log").read_text(),
                                          target["inventory"], target["ignored"])
            target["harness_status_source"] = "final unfiltered libtest summary and final failure-name list"
        except ValueError as error:
            target["statuses"] = []
            target["errors"].append(str(error))
        target["records"] = read_records(timing_dir, target["binary"], target["pid"])
        target["errors"].extend(reconcile(target, target["records"], disabled))
        if child.returncode:
            target["errors"].append(f"binary exited {child.returncode}")
    except Exception as error:
        target.setdefault("errors", []).append(f"{type(error).__name__}: {error}")
    finally:
        target["load_after"] = snapshot()
        target["cancelled"] = children.cancelled
        release_state(private, target, children.cancelled)
        save(directory / "target.json", target)
    return target


def execute_doctests(repo, ambient, run_dir, selection, children):
    target = {"id": "doctests", "kinds": ["doctest"], "records": [], "statuses": [],
              "errors": [], "load_before": snapshot(), "timing_uncovered": True,
              "exit_code": None, "expected_nonignored_count": None, "completed_count": None,
              "whole_test_set_completed": None, "aborted_early": None,
              "completion_basis": "native Cargo doctest aggregate; inventory not collected"}
    directory = run_dir / "doctests"
    directory.mkdir()
    private = Path(tempfile.mkdtemp(prefix="pa-t-", dir="/tmp"))
    try:
        env = environment(repo, private, ambient)
        env.pop("PA_TEST_TIMING_DIR", None)
        env.pop("PA_TEST_TIMING", None)
        command = ["cargo", "test", "--locked", "--offline", *selection, "--doc"]
        target.update(command=command, cwd=str(repo), environment=env, state_root=str(private))
        with (directory / "harness.log").open("w") as output:
            started = time.monotonic_ns()
            target["start_unix_ns"] = time.time_ns()
            target["start_monotonic_ns"] = started
            child = children.spawn(command, cwd=repo, env=env, stdout=output,
                                   stderr=subprocess.STDOUT)
            target["pid"] = child.pid
            try:
                children.resume_signals()
                _, status, usage = os.wait4(child.pid, 0)
            except BaseException:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                _, status, usage = os.wait4(child.pid, 0)
                child.returncode = os.waitstatus_to_exitcode(status)
                raise
            finally:
                children.complete(child)
            child.returncode = os.waitstatus_to_exitcode(status)
            target.update(exit_code=child.returncode, elapsed_ns=time.monotonic_ns() - started,
                          end_unix_ns=time.time_ns(), cpu_user_seconds=usage.ru_utime,
                          cpu_system_seconds=usage.ru_stime)
        if child.returncode:
            target["errors"].append(f"doctests exited {child.returncode}")
    except Exception as error:
        target["errors"].append(f"{type(error).__name__}: {error}")
    finally:
        target["load_after"] = snapshot()
        target["cancelled"] = children.cancelled
        release_state(private, target, children.cancelled)
        save(directory / "target.json", target)
    return target


def union_ns(intervals):
    total = 0
    end = None
    for start, stop in sorted(intervals):
        total += max(0, stop - max(start, end if end is not None else start))
        end = max(stop, end if end is not None else stop)
    return total


def span_totals(record):
    spans = record["spans"]
    names = sorted({span["name"] for span in spans})
    totals = {name: union_ns([(span["start_unix_ns"], span["end_unix_ns"])
                             for span in spans if span["name"] == name]) for name in names}
    covered = union_ns([(span["start_unix_ns"], span["end_unix_ns"]) for span in spans])
    return totals, max(0, record["elapsed_ns"] - covered)


def load_timing_run(directory):
    manifest_path = directory / "run.json"
    if manifest_path.exists():
        return json.loads(manifest_path.read_text())
    groups = {}
    for path in sorted(directory.glob("*.jsonl")):
        for line_number, line in enumerate(path.read_text().splitlines(), 1):
            record = json.loads(line)
            if record.get("kind") != "test":
                raise ValueError(f"{path}:{line_number}: expected a test timing record")
            key = (record["binary"], record["pid"])
            target = groups.setdefault(key, {"id": f"{key[0]}-{key[1]}", "binary": key[0],
                                             "pid": key[1], "records": [], "statuses": [],
                                             "coverage_unverified": True, "errors": []})
            target["records"].append(record)
    if not groups:
        raise ValueError(f"no run.json or timing sidecars in {directory}")
    return {"targets": list(groups.values()), "coverage_unverified": True}


def table(directory):
    manifest = load_timing_run(directory)
    unverified = manifest.get("coverage_unverified", False)
    if manifest.get("cleanup_error"):
        print(f"BUILD STATE RETAINED: {manifest.get('build_state_root')}: {manifest['cleanup_error']}")
    if unverified:
        print("COVERAGE UNVERIFIED: raw sidecars have no runner inventory or harness status.")
    print("TARGET  WALL(s)  USER(s)  SYS(s)  EXIT  INVENTORY_COMPLETED  RESULT")
    for target in manifest["targets"]:
        if unverified:
            print(f"{target['id']}  records={len(target['records'])}  "
                  "wall/CPU/exit=unknown  INVENTORY=UNVERIFIED")
            continue
        print(f"{target['id']}  {target.get('elapsed_ns', 0) / 1e9:.3f}  "
              f"{target.get('cpu_user_seconds', 0):.3f}  "
              f"{target.get('cpu_system_seconds', 0):.3f}  "
              f"{target.get('exit_code')}  "
              f"{target.get('completed_count')}/{target.get('expected_nonignored_count')} "
              f"(whole={target.get('whole_test_set_completed')}, aborted={target.get('aborted_early')})  "
              f"{'FAIL' if target.get('errors') else 'ok'}")
        if target.get("cleanup_error"):
            print(f"  STATE RETAINED: {target.get('state_root')}: {target['cleanup_error']}")
        for name, phases in target.get("failed_phases", {}).items():
            print(f"  FAILED PHASE: {name}: {', '.join(phases)}")
        if target.get("timing_uncovered"):
            print("  UNCOVERED TIMING: doctest aggregate (executed natively; inventory completion unknown)")
        if target.get("uncovered_tests"):
            print(f"  UNCOVERED TIMING: crate={target.get('package', 'unknown')} "
                  f"{len(target['uncovered_tests'])} tests "
                  f"({', '.join(target.get('kinds', []))}); names in run.json")
        for error in target.get("errors", []):
            print(f"  {error}")
    if unverified:
        print("TOTAL wall/CPU unknown (raw records are not a measured run).")
    else:
        print(f"TOTAL wall_with_build={manifest['elapsed_ns'] / 1e9:.3f}s "
              f"execution={manifest.get('execution_elapsed_ns', 0) / 1e9:.3f}s "
              f"user={manifest['cpu_user_seconds']:.3f}s sys={manifest['cpu_system_seconds']:.3f}s")
    rows = [(target, record) for target in manifest["targets"] for record in target.get("records", [])]
    print("\nTEST  WALL(s)  HARNESS  GUARD  SPANS(s)  UNATTRIBUTED(s)")
    for target, record in sorted(rows, key=lambda row: row[1]["elapsed_ns"], reverse=True):
        totals, unattributed = span_totals(record)
        outcome = dict(target["statuses"]).get(record["test"], "unknown" if unverified else "missing")
        details = ", ".join(f"{name}={total / 1e9:.6f}" for name, total in totals.items())
        print(f"{target['id']}::{record['test']}  {record['elapsed_ns'] / 1e9:.6f}  "
              f"{outcome}  {record['outcome']}  {details or '-'}  {unattributed / 1e9:.6f}")
    uncovered_crates = sorted({target["package"] for target in manifest["targets"]
                               if target.get("uncovered_tests") and target.get("package")})
    if unverified:
        print("\nCOVERAGE UNVERIFIED: missing tests, ignored tests and fixture subprocess provenance unknown.")
    else:
        print("\nUNCOVERED UNIT CRATES: " + (", ".join(uncovered_crates) or "none"))
        print("UNCOVERED TIMING: doctests; uninstrumented lib/bin unit tests. Ignored tests are not run.")
    return int(any(target.get("errors") for target in manifest["targets"]))


def run(args):
    repo = Path(__file__).resolve().parents[1]
    audit = None
    if args.audit:
        from instrument_test_timings import instrument, inventory as source_inventory
        missing = {}
        for path in source_inventory(repo):
            _, count = instrument(path.read_text())
            if count:
                missing[str(path.relative_to(repo))] = count
        audit = {"scope": "integration tests plus pa-core/pa-daemon src unit tests", "missing": missing}
        if missing:
            raise ValueError(f"source audit: {sum(missing.values())} uninstrumented scoped tests: {missing}")
    ambient = dict(os.environ)
    required = ("CARGO_HOME", "CARGO_TARGET_DIR", "PRIME_AGENT_KERNEL_PYTHON",
                "PA_E2E_KERNEL_PYTHON", "PA_CORE_KERNEL_PYTHON")
    for key in required:
        if not ambient.get(key) or not Path(ambient[key]).is_absolute():
            raise ValueError(f"set {key} to an absolute isolated/qualified path first")
    if len({ambient[key] for key in ("PRIME_AGENT_KERNEL_PYTHON", "PA_E2E_KERNEL_PYTHON",
                                         "PA_CORE_KERNEL_PYTHON")}) != 1:
        raise ValueError("all three kernel Python overrides must name the same synced interpreter")
    directory = args.output.resolve() if args.output else Path(tempfile.mkdtemp(prefix="pa-timings-"))
    if args.output:
        directory.mkdir(parents=True, exist_ok=False)
    manifest = {"schema_version": 1, "slots": args.slots, "timing_disabled": args.disable_timing,
                "load_before": snapshot(), "commands": [], "targets": [],
                "source_audit": audit,
                "uncovered": ["doctests", "ignored tests", "uninstrumented lib/bin unit tests"]}
    started = time.monotonic_ns()
    private = Path(tempfile.mkdtemp(prefix="pa-t-", dir="/tmp"))
    children = Children()
    previous_handlers = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}

    def interrupt(signum, frame):
        if children.launching_main:
            children.pending_signal = signum
            return
        for sig in previous_handlers:
            signal.signal(sig, signal.SIG_IGN)
        children.cancel()
        raise KeyboardInterrupt(f"timing run interrupted by signal {signum}")

    for sig in previous_handlers:
        signal.signal(sig, interrupt)
    try:
        env = environment(repo, private, ambient)
        selection = ["-p", args.package] if args.package else ["--workspace"]
        selectors = [item for name in (args.target or []) for item in ("--test", name)]
        command = ["cargo", "test", "--locked", "--offline", *selection, *selectors,
                   "--no-run", "--message-format=json"]
        output = checked(command, repo, env, directory / "build.log", manifest["commands"], children)
        metadata = json.loads(checked(["cargo", "metadata", "--locked", "--offline", "--no-deps",
                                       "--format-version", "1"], repo, env,
                                      directory / "metadata.log", manifest["commands"], children))
        scheduling_metadata = json.loads(json.dumps(metadata))
        for package in scheduling_metadata["packages"]:
            for target in package["targets"]:
                if target["kind"] == ["proc-macro"]:
                    target["kind"] = ["lib"]
        target_groups(scheduling_metadata)
        packages = {package["id"]: package for package in metadata["packages"]}
        seen = set()
        for line in output.splitlines():
            artifact = json.loads(line)
            if (artifact.get("reason") != "compiler-artifact"
                    or not artifact.get("profile", {}).get("test") or not artifact.get("executable")):
                continue
            target = artifact["target"]
            if not set(target["kind"]) & {"lib", "bin", "test", "proc-macro"}:
                raise ValueError(f"unsupported default harness: {target['kind']}")
            binary = artifact["executable"]
            if binary in seen:
                continue
            seen.add(binary)
            if args.target and target["name"] not in args.target:
                continue
            package = packages[artifact["package_id"]]
            manifest["targets"].append({"id": f"{len(seen):03d}-{package['name']}-{target['name']}",
                                        "name": target["name"], "binary": binary,
                                        "package": package["name"], "kinds": target["kind"],
                                        "timing_required": (args.require_all_timing or "test" in target["kind"]
                                                            or package["name"] in {"pa-core", "pa-daemon"}),
                                        "cwd": str(Path(package["manifest_path"]).parent)})
        if not manifest["targets"]:
            raise ValueError("no matching default test binaries")
        ordinary = [target for target in manifest["targets"] if target["name"] not in EXCLUSIVE]
        exclusive = [target for target in manifest["targets"] if target["name"] in EXCLUSIVE]
        for targets, slots in ((ordinary, args.slots), (exclusive, 1)):
            with ThreadPoolExecutor(max_workers=slots) as workers:
                try:
                    futures = [workers.submit(execute, target, repo, ambient, directory,
                                              args.disable_timing, children) for target in targets]
                    for future in as_completed(futures):
                        target = future.result()
                        print(f"{target['id']}: {'FAIL' if target['errors'] else 'ok'}", flush=True)
                except BaseException:
                    children.cancel()
                    raise
        if not args.target:
            manifest["targets"].append(execute_doctests(repo, ambient, directory, selection, children))
        else:
            manifest["uncovered"].append("doctests not run for --target selection")
        completed = [target for target in manifest["targets"] if "end_unix_ns" in target]
        manifest["execution_elapsed_ns"] = (
            max(target["start_monotonic_ns"] + target["elapsed_ns"] for target in completed)
            - min(target["start_monotonic_ns"] for target in completed)) if completed else 0
    except BaseException:
        children.cancel()
        manifest["cancelled"] = True
        raise
    finally:
        for sig, handler in previous_handlers.items():
            signal.signal(sig, handler)
        release_state(private, manifest, children.cancelled)
        manifest["build_state_root"] = str(private)
        manifest.update(elapsed_ns=time.monotonic_ns() - started, load_after=snapshot())
        for field in ("cpu_user_seconds", "cpu_system_seconds"):
            manifest[field] = sum(target.get(field, 0) for target in manifest["targets"])
        save(directory / "run.json", manifest)
        print(f"Timing run: {directory}", flush=True)
    return table(directory)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    runner = commands.add_parser("run")
    runner.add_argument("--slots", type=int, choices=(1, 8, 16), required=True)
    runner.add_argument("--package")
    runner.add_argument("--audit", action="store_true",
                        help="audit integration and pa-core/pa-daemon source instrumentation")
    runner.add_argument("--require-all-timing", action="store_true",
                        help="require lib/bin/proc-macro unit coverage too; doctest timing remains uncovered")
    runner.add_argument("--target", action="append", help="integration target name; repeat to select whole binaries")
    runner.add_argument("--output", type=Path)
    runner.add_argument("--disable-timing", action="store_true",
                        help="exercise timing off path: unset PA_TEST_TIMING_DIR; do not create a records directory")
    viewer = commands.add_parser("table")
    viewer.add_argument("run_dir", type=Path, help="runner run directory or raw JSONL sidecar directory")
    args = parser.parse_args()
    try:
        return run(args) if args.command == "run" else table(args.run_dir)
    except KeyboardInterrupt as error:
        parser.exit(130, f"{error}\n")
    except (OSError, ValueError, RuntimeError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
