# Agent resource baseline (#9, Week 4)

This is measurement, not an optimization or a capacity limit. #10 can use it to
choose larger experiments. No production, harness, timing, crate, or CI code changed.

## Recorded run

- Source: `origin/main` at `9e7f13530`; sampler commit `017793586`.
- Linux 7.3.4, x86_64 NixOS; AMD RYZEN AI MAX+ 395, 32 logical CPUs,
  130,492,060 KiB RAM. One shared host, not a dedicated benchmark machine.
- `nix develop`: rustc 1.98.1; unoptimized dev build, debug info disabled by
  the dev shell. Python 3.13.15; repository runtime lockfile and all dependency groups.
- Selected run: `/tmp/t9-measure-run6`, 2026-11-21 UTC. Binary SHA-256 is in
  [the receipt](agent-resource-baseline.json).
- One daemon, one root session, two actual `rlm.spawn` children, ten explicit
  sequential prompts plus each child's initial turn. No TUI. The loopback fake
  SSE provider emits real Python tool calls and verifies their output markers.
  It has no external inference or network latency.
- Root Python startup is measured before adding children. Each CPU tool touches
  a 32 MiB `bytearray`, runs about 0.6 process-CPU seconds, then releases it.
  Idle observation windows are deliberately 0.5 seconds, not readiness waits.
- Sampler requested interval: 50 ms. Actual gaps: minimum 50.03 ms, median
  50.16 ms, maximum 312.25 ms. The command-shape observer requests 25 ms.
  The workload caps capture at 180 seconds; the selected run finished in 26 seconds.
- Daemon exited 0; no owned processes remained after shutdown.

## What is measured

`scripts/sample_agent_resources.py` reads Linux `/proc/<pid>/stat`. Every CSV
sample contains a row for each visible process in the daemon's current tree,
including grandchildren, and one `tree` total. Every row carries `root_pid`.
`pid` plus boot-relative `start_id` distinguishes PID reuse; reuse of the daemon
PID ends collection. CPU is own user + system time, not child CPU counters;
Linux clock precision here is 10 ms. RSS is resident pages in KiB (4 KiB pages
here). MiB below means KiB / 1024.

Tree memory is the sum of simultaneous process RSS. An agent's memory/CPU is its
worker subtree, including its interpreter/probes; the daemon is separate shared
cost. Child workers are daemon children, not descendants of the root worker, so
root-worker attribution does not accidentally include their work.

The workload's separate observer reads command shapes only inside its private
daemon tree. `argv[1:3] == ["-m", "rlm.repl"]` identifies a kernel;
`argv[1] == "-c"` identifies a Python probe in this workload. Probe source and
other arguments are not retained. An unrecognized shape is reported as its
process name, without assuming it is a kernel. There were 3 positively identified
kernels, alongside observed `python -c` probes and unclassified `python` shapes.
With a 312 ms maximum gap, short-lived processes are an incompletely observed
class, not a census of births. The raw process CSV does not read arguments; the receipt holds this separate
identity-to-shape mapping. The root kernel was visible for 19.84 seconds; the two
child kernels for 5.00 and 1.73 seconds. These are observed spans, not exact
lifetimes. Child kernels disappeared after their completed work; they were not
assumed to persist simply because their workers remained.

For CPU, the summary groups process rows by identity and credits each increase
to the ending sample's phase. For a new post-baseline identity it also credits
its first observed cumulative CPU. The first sample is a baseline, not credited.
This birth rule is valid for this freshly launched private tree; it must not be
applied blindly to an arbitrary existing daemon with reparented processes.
The CSV's generic `cpu_delta_seconds` instead leaves first observations blank
and flags incomplete tree deltas; it does not assume when a process was born.

The receipt also retains a weaker before/after CPU measure around each
`prompt_and_wait`: surviving-process differences plus cumulative CPU of new
processes still visible at the end. It misses processes that already exited.
The cold root tool illustrates why process rows matter: sampled tree CPU was
5.62 seconds, while endpoint snapshots saw only 1.22 seconds. Endpoint-only
sampling understated this cold-root CPU cost by **at least 4.6×**. The sampled
estimate is itself a lower bound because it loses exit tails and unseen processes.
Native import threads can make CPU seconds exceed elapsed wall seconds.

## Results

Stable-window medians, not startup peaks:

| Phase | Processes | Tree RSS MiB | Growth MiB |
| --- | ---: | ---: | ---: |
| Root after first tool, before children | 3 | 188.31 | — |
| One child, no child kernel yet | 4 | 253.43 | 65.12 |
| Two children, no child kernels yet | 5 | 317.56 | 64.13 |
| Final idle, child kernels gone | 5 | 320.15 | — |

These increments include small changes in daemon/root state; they are not
exclusive allocation counts. At the two-child idle point, worker-subtree RSS was
133.45 MiB for the root (including its kernel), 63.88 MiB for child one, and
62.83 MiB for child two. Daemon RSS was 57.41 MiB. During their own tool calls,
subtree peaks were 165.70, 163.11, and 161.63 MiB respectively. Whole-tree tool
peaks were 350.06 MiB (warm root), 417.79 MiB (child one), and 417.16 MiB (child
two). The largest sampled tree RSS was **417.79 MiB**, not a guaranteed peak.

CPU seconds are sampled estimates/lower bounds; see the limits below. “Agent”
excludes the shared daemon and other workers. Spawn tree cost includes the newly
created child's initial work. Endpoint tree CPU is retained for comparison only.

| Prompt phase | Wall s | Agent CPU s | Tree CPU s | Endpoint tree CPU s |
| --- | ---: | ---: | ---: | ---: |
| root_plain | 0.230 | 0.34 | 0.36 | 0.30 |
| root_tool_cold | 4.416 | 5.60 | 5.62 | 1.22 |
| spawn_one | 0.382 | 0.07 | 0.32 | 0.32 |
| spawn_two | 0.500 | 0.09 | 0.31 | 0.30 |
| root_tool | 0.759 | 0.68 | 0.69 | 0.71 |
| root_followup | 0.078 | 0.05 | 0.07 | 0.07 |
| child_one_tool | 7.198 | 6.93 | 6.95 | 1.42 |
| child_one_followup | 0.269 | 0.18 | 0.20 | 0.07 |
| child_two_tool | 4.524 | 6.40 | 6.41 | 1.35 |
| child_two_followup | 0.129 | 0.06 | 0.08 | 0.08 |

## Reproduce

From the worktree, with one Cargo process at a time:

```sh
nix develop --command bash -c 'rustc --version; export CARGO_TARGET_DIR=/tmp/t9-target CARGO_BUILD_JOBS=4; nice -n 10 cargo build -p pa-cli --bin prime-agent'
nix develop --command uv sync --project prime-agent-runtime --all-groups --locked --no-editable
nix develop --command nice -n 10 uv run --no-project scripts/measure_agent_tree.py --binary /tmp/t9-target/debug/prime-agent --root /tmp/t9-reproduce --kernel-python "$PWD/prime-agent-runtime/.venv/bin/python"
nix develop --command uv run --no-project scripts/summarize_agent_resources.py --samples /tmp/t9-reproduce/samples.csv --receipt /tmp/t9-reproduce/receipt.json
nix develop --command uv run --no-project scripts/test_sample_agent_resources.py
nix develop --command uv run --no-project scripts/test_summarize_agent_resources.py
```

Choose a new `--root` each time; existing roots are refused. The kernel interpreter
must keep its venv symlink path, not resolve to the base interpreter. The workload
allowlists environment variables and creates private HOME, XDG, agent/session,
TMPDIR, socket and `PA_TEST_SCRATCH_ROOT` paths. Only local fixture model credentials
exist. Its `PYTHONPATH` contains this repository's skill source directories;
no user configuration or live provider credentials are loaded.

The binary target on this source commit is still `prime-agent`; a rename branch
exists but is not merged. The user-facing product is Weave. The build succeeded
with existing vendored crossterm warnings; they were not changed. No full fleet or
workspace suite was run for these standalone scripts. Sampler unit tests passed;
a Linux `--backend ps` smoke capture also passed (BSD/macOS was not tested).

For an already running daemon, use the sampler directly with `--pid PID
--interval 0.05 --duration 60 --output new.csv`. `--phase-file PATH` labels rows.
On non-Linux systems `--backend auto` uses `ps -e -o
pid=,ppid=,time=,rss=,lstart=,comm=`; explicit `--backend ps` also works on Linux.
This requires a compatible system `ps`, not an extra Python dependency. Fallback
CPU and start identities have whole-second precision; same-second PID reuse
cannot be distinguished. The Linux-only workload is separate from that fallback.

## Limits and discarded trials

- A process born and finished between samples is invisible. A before/after pair
  also cannot see one born and finished entirely inside the turn. Even a sampled
  process can accrue CPU after its last row and before exit; that tail is lost.
  RSS peaks shorter than an actual sample gap are missed. No kernel in this
  selected run is claimed to have a sub-interval lifetime; short-lived probes
  are a particular reason endpoint snapshots undercount CPU.
- Phase CPU can move across a boundary by an actual sample interval or collection
  delay. Endpoint and sampled totals are different windows, so small reversals
  between their values are not contradictions. Reads are not atomic; disappearing
  or inaccessible processes are skipped. Reparented descendants leave the tree.
- RSS is **not PSS**, USS, an allocation total, or cgroup memory. Shared libraries
  and shared pages are counted repeatedly. Swap, GPU memory, disk/cache footprint,
  inference-provider memory and this external provider/sampler/observer's own
  resources are not measured.
- This is one selected run on one shared host with a fake provider and no external
  network latency. It does not establish variance, production latency, release-build
  behavior, long-conversation growth, deep trees, concurrent tool peaks, or limits
  beyond two children. Do not extrapolate a production capacity from it.
- Trial 1 was discarded: resolving the venv interpreter symlink selected its base
  Python, without the runtime; tool failures had initially looked like completion.
  Successful output-marker validation now prevents this.
- Trial 2 was calibration only (root startup was mixed with child addition).
  Trials 3–4 failed because missing skill source paths queued a
  `python-skills-unavailable` notice instead of the expected next workload turn;
  trial 3 also overlapped the end of trial 2. All are excluded from these tables.
- Trial 5 completed but reused an idle phase label before and after adding children;
  it was discarded rather than combining unlike medians. Trial 6, with distinct
  phases, skill imports and command-shape verification, is the recorded evidence.

## Raw evidence

[CSV](agent-resource-baseline.csv): all 2,382 raw rows (425 samples, including
shutdown); no thinning or numeric normalization (CSV line endings are LF). [Receipt](agent-resource-baseline.json):
turn boundaries, session-to-worker mapping, successful tool results, safe command
shapes and shutdown result. PIDs and paths are local observations, not stable IDs
for future runs. The summarizer produces phase and turn JSON directly from these
files; it does not read the table values above.
