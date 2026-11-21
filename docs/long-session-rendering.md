# Long-session TUI rendering: measurement and design

Design for ticket #20, inspected at `57cd83b17` (2026-11-21). This is not a
performance result or an implementation proposal ready to merge. Week 5 depends
on #10 measurements. The suite's Week 2.5 remains open: this document changes no
product code, harness, timing budget, or CI configuration.

## Decision before measurements

**None of the four candidate fixes is justified as a new optimization yet.**
Viewport rendering, entry memoisation, and append bookkeeping already exist.
Reimplementing them could add work or break their invalidation rules. A cheaper
boundary query is plausible, but must preserve context-strategy and usage-drift
semantics. Reading/profiling the existing paths and collecting baselines is safe;
changing their behavior is not part of this ticket.

Unknown today: frame-time distributions at 100, 1,000 and 10,000 turns, formatted
bytes per frame, allocations per frame, cache hit rates for representative
sessions, and the share attributable to context assembly. No speedup, frame-time
target, or cache-size limit is inferred from those unknowns.

The 100/1,000/10,000-turn matrix cannot run until its driver exists. No number
in this document is a measured result; numbers describe proposed workloads or
existing test configurations and assertion limits.

## Current delivery policy

Under the current policy, the measurement campaign is deferred to the end.
Candidate fixes may land on correctness evidence, with performance numbers
recorded during that later campaign. This does not supply the missing evidence
or change the conclusion that no optimisation is justified yet.

## Current path and ownership

`pa-tui` consumes wire snapshots/events, not the session engine. Keep transcript
paint, the `/agents` roster, and provider-context assembly separate in profiles.
A session with many turns is not the same workload as a roster with many sessions.

### Session transcript

1. `session_ui/` folds daemon snapshots and events into `AgentView.chat`.
   `AgentView::push` can complete a tool result in place; `push_entry` appends;
   `prepend_entries` shifts cached indices for backfill; `clear_chat` discards
   transcript caches for an authoritative rebuild. Snapshot conversion and
   incremental event handling must be measured separately from painting.
2. `interactive/` schedules dirty frames. `interactive/render.rs` owns the
   terminal/headless sinks. Terminal paint reaches `app::draw`, then
   `AgentView::render_frame` in `view/frame.rs`. Headless paint uses
   `app::render_frame_text`, which calls the same frame composer before stripping
   OSC markers/links and joining text. It is not a terminal-backend benchmark.
3. Each composition rebuilds the dock/pickers and top bar, visible transcript
   rows, padding, click geometry, hover/toast overlays and hyperlink ranges.
   `app::draw` converts the frame to ratatui lines and draws it in a synchronized
   update bracket. Ratatui's previous-buffer diff limits terminal writes, not
   the CPU work that produced the frame.
4. `view/lazy.rs::visible_transcript_window` normally walks a sparse top/tail
   anchor and nearby cursor. It does not format every historical turn for every
   ordinary frame. Splash and live tail sections are built only when reached.
   Selection, resize and absolute-geometry requests can leave that fast path.
5. `view/layout.rs::layout_pass` is the exact-geometry path: it scans all entries,
   builds an offsets vector and obtains heights, reusing cached heights when
   valid. `transcript_window` then materializes intersecting rows. Thus a full
   geometry walk is not automatically a full text-formatting pass.
6. `view/flush.rs` streams explicit restored-transcript replay and exit/suspend
   output to native scrollback. `interactive/render.rs` keeps replay bytes
   separate from normal viewport frames. These full-history operations are not
   recurring-frame costs and must not be optimized away as redundant paints.

### Caches already present

- `AgentView.entry_layout`: rendered rows per entry and three detail modes,
  packed as `RowPack` behind `Arc`. Only requested row ranges expand to owned
  strings. Visited entries can retain packed rows; measure retained memory as
  well as allocations. `entry_heights` separately caches exact counts/spacing.
- `entry_cacheable` excludes streaming assistant bodies and running tool/bash
  cards. Stable entries reuse layouts; the streaming path still formats its
  changing content. Per-assistant `MarkdownBlockCache` reuses settled markdown
  blocks inside a streaming message.
- Width and `LayoutOptions` (theme, code indent, Mermaid/image modes and link
  base) invalidate layout data. `mark_entry_stale` also invalidates later
  spacing-dependent entries. Prepend, pop, card expansion and detail changes
  have their own bookkeeping. A turn is not an immutable cache key.
- Sparse cursor/anchor state avoids global geometry on ordinary scroll/follow.
  `view/handoff.rs` can seed matching layouts across view switches.
  `selection_restyle` updates rows touched by selection changes.
- `flushed_frame` tracks previously flushed text to avoid duplicate native
  scrollback. It is not a per-frame formatting cache.

### `/agents` roster

`agents_view/` owns search, selection, expansion and actions over session rows.
`agents_view/render.rs::render_frame` recounts sections, builds splash/prompt,
notice and hints, and delegates to `render_list`. That method calls
`agents_view_state::build_layout` over the roster, builds the display-item list
and selects a centered slice. Only sliced rows call `render_row`, but layout,
section counts, display-list construction and selection lookup still visit the
roster. Row formatting also looks up heartbeat jobs. This path is not formatting
the open session's transcript.

Terminal draw converts those lines to ratatui text; headless draw joins plain
text and retains only distinct frames. Roster state survives draws, but there is
no formatted-row cache in this composer. Distinct-frame capture is output
deduplication **after** formatting, not skipped rendering.

### Boundary view snapshot and #83 seam

`pa-core/src/session_engine/turn_boundary.rs` defines `LiveViewSnapshot` as
messages plus `UsageDrift`. `assemble_live_view` calls an installed async
`LiveView`. `engine.rs` installs a closure through
`context_strategy::assemble_session_view` and publishes the accepted archive.
`compact.status` assembles that view before computing usage; its fallback clones
retained entries and builds session context. `history_host.rs` also consumes the
seam. `take_refresh` itself only consumes an atomic stale marker.

This is a **model-facing context** view, not `AgentView` or a terminal frame.
Treat the #83 seam as a candidate for measuring repeated full assembly by
boundary/status consumers, not evidence that every frame assembles history.
Before changing it, record callers, invocation count, assembled message/byte
count, duration, lock wait and whether the accepted archive changes. A proposed
usage-only or revision-keyed snapshot must preserve active strategy selection,
canonical history edits, model switches, compaction, drift and archive publication.
No new `pa-tui` dependency on `pa-core` is needed or allowed.

## What to measure first on your own machine

Start with existing isolated fake-daemon checks, not a live personal session.
From the repository's Rust development environment, with the project's isolated
HOME/agent/session/kernel/socket setup, run:

```sh
cargo test -p pa-tui --release --test drag_select_perf -- --nocapture --test-threads=1
cargo test -p pa-tui --release --lib view::layout::tests -- --nocapture --test-threads=1
cargo test -p pa-tui --release --lib view::lazy::tests -- --nocapture --test-threads=1
```

The first is the immediate local performance sanity check: it brackets a
100-drag burst after initial rendering, counts render invocations and verifies
copy output at 40 and 4,000 **messages**, not turns. It takes multiple samples
and compares their minima. Passing produces no general frame-time distribution;
assertion budgets in that test are existing regression limits, not measurements
or proposed #20 targets. The library tests pin bounded work and sparse-vs-exact
render equivalence, including a 100,000-entry case. They are correctness/work
checks, not the requested 100/1,000/10,000-turn baseline.

For an optional Linux CPU profile with `perf` already available:

```sh
perf record -g -o /tmp/t20-drag-perf.data -- cargo test -p pa-tui --release --test drag_select_perf -- --nocapture --test-threads=1
perf report -i /tmp/t20-drag-perf.data --stdio
```

Build once before recording. This command includes Cargo/test startup and mock
attach work; inspect call stacks for layout, markdown, row unpacking, selection
and frame conversion, and do not divide whole-command time by frame count.
`perf` may require host permission; report that blocker rather than changing
system settings. No existing command in this revision reports all three requested
metrics at all three turn counts. `hyperfine` around these tests would measure
whole-test latency, not frame latency or allocation counts.

## First full baseline (#10 follow-up, not added here)

Use deterministic synthetic wire data and a local fake daemon, no provider,
network or personal transcripts. Define one turn as one user submission plus its
assistant reply; record tool entries separately. At **100, 1,000 and 10,000
turns**, keep bytes per turn and content mix fixed. Include a plain-text series
and a separate markdown/code/table/tool-output series. Record fixture seed/hash,
total UTF-8 bytes, entry count, terminal size, detail/theme/options, machine,
revision, Rust build profile and tool versions. Proposed initial geometry is
100 columns by 30 rows, matching `drag_select_perf`; also test narrower resize.
This is a workload choice, not an observed optimum.

Separate cold attach/first paint from warmed steady-state frames. For each size,
collect idle forced repaint, live-tail append/stream delta, paused middle scroll
while appending, drag/copy, detail toggle and resize. Measure full replay/exit
separately. Do not equate retained distinct headless frames with draw invocations.
For the roster, vary session count independently and include search/expansion.

A future opt-in driver/counter mode should report:

| Metric | Boundary and interpretation |
| --- | --- |
| Frame time (microseconds) | Wall time around `AgentView::render_frame`; separate `app::draw` inclusive terminal cost and headless text serialization. Report samples, p50/p95/p99/max per scenario, plus cold time. Record scheduling/event-to-paint separately. |
| Bytes formatted/frame | In-repo counter: sum UTF-8 input bytes actually sent to text/markdown/tool formatters on cache misses; count repeated work, including hidden work. Separately count row-unpack/copied bytes and emitted frame bytes. Do not call terminal bytes or cached bytes “formatted bytes”. |
| Allocations/frame | In-repo scoped counting allocator around the same synchronous compose boundary: allocation/reallocation calls and requested bytes, excluding fixture setup and capture retention. Measure cold and warm paths; state thread scope so daemon/task allocations are not silently mixed in. |
| Diagnostic work | Cache hits/misses, entry visits/renders/count-only visits, boundary assembly calls/time/bytes, retained cache bytes and RSS. |

Existing test-only `ENTRY_VISITS`/`ENTRY_RENDERS` in `view/layout.rs`, and
`SPLASH_RENDERS`/`TAIL_RENDERS` in `view/lazy.rs`, are useful precedents. They do
not count allocated or formatted bytes. Headless `renders` and timestamp markers
witness actual invocations, while frames are deduplicated and style-blind.
`tui_rss_stability.rs` samples RSS; RSS is not an allocation count.

For reproducibility, reserve this **proposed, not currently runnable** command
contract for the follow-up driver (no example or flags are added by #20):

```sh
cargo run -p pa-tui --release --example long_session_render -- --turns 100 --width 100 --height 30 --samples 1000 --metrics frame,formatted-bytes,allocations
```

Repeat with `--turns 1000` and `--turns 10000`. The proposed 1,000 samples is a
collection starting point, not a performance threshold; retain raw samples and
repeat runs to expose noise. Run uninstrumented timing separately from allocation
counting and CPU profiling so counter overhead cannot masquerade as product cost.
If a driver is not approved while Week 2.5 is open, record these cells as unknown;
do not claim the existing tests fill them.

## Candidate fixes and evidence gates

| Candidate | Shape if supported | Evidence required before implementation |
| --- | --- | --- |
| Viewport-only formatting | Extend the existing sparse window/count-only geometry, especially inside one huge visible entry; avoid formatting hidden rows. Do not replace it with a second viewport system. | Per-scenario formatter bytes and entry/count visits show hidden work scaling with turn count or entry size; stacks identify eager formatting rather than backend cost. Compare cold/warm, middle/tail and resize. Preserve exact heights and spacing. |
| Memoising formatted turns | Extend existing entry/block caches, keyed by content revision plus all layout options; keep transient streaming state separate. Consider retention only with memory evidence. | Cache-miss and invalidation traces show repeated formatting of unchanged content. Quantify saved CPU/allocations against retained packed rows. Demonstrate edits, tool completion, detail/expansion, prepend and width/theme changes never return stale rows. |
| Incremental transcript append | Reuse `push_entry`, in-place updates and sparse tail bookkeeping; replace an identified avoidable clear/rebuild with append or delta handling only where event/snapshot identity proves continuity. | Event/attach traces show full snapshot conversion or `clear_chat` repeats for unchanged prefixes, with meaningful cost. Prove reconnect, backfill, replacement snapshots, retries, edits and compaction cannot duplicate, omit or reorder entries. Canonical history is not an append-only rendering cache. |
| Avoid full view assembly on boundary checks (#83) | Separate cheap flags/revision checks or reuse an authoritative usage snapshot from required context assembly. Leave actual send/history-edit assembly authoritative. | Caller traces show repeated assembly without relevant state changes and its measured share of latency/allocations. Prove strategy, model, drift and archive invalidation, concurrency and error behavior; do not cache by message count alone or assume boundary assembly causes a slow frame. |

Pick the smallest change for the measured dominant path. If warm formatting is
already bounded, investigate row expansion/copying, frame conversion or scheduling
instead. A faster formatter cannot fix a backend write stall. Measure before/after
with identical fixtures and report absolute values as well as ratios.

## Non-regression contract

- Transcript content, order, tool-result association, wrapping, styles, links,
  thinking/detail visibility and card expansion must match the existing reference.
  Compare sparse and exact paths, including hidden/empty and wide Unicode rows.
- Scrollback must preserve paused position while appending/backfilling, follow
  transitions, top/bottom/page navigation and resize behavior. Explicit restored
  native replay and exit/suspend flush must remain complete and nonduplicating.
- Copy/drag must return the displayed text across wrapped rows, not stale cached
  text or off-screen overlays; selection coordinates must survive anchor changes.
- Keep `agents_view_*_headless.rs` assertions unchanged, including click, hover
  and heartbeat-badge frames, and the roster render-barrier checks. Text equality
  alone misses style-only and clickable-region regressions.
- Later implementation verification must include existing `drag_select_perf`,
  `mouse_selection_headless`, `mouse_scroll_headless`, `windowed_backfill_headless`,
  `chat_open_first_frame_headless`, layout/lazy tests and native replay checks.
  Run affected `agents_view_*_headless` targets explicitly (Cargo's `--test` takes
  a target name, not a shell filename glob). Preserve existing budgets and waits;
  do not loosen tests to make an optimization appear successful.
- A user-visible implementation needs the repository's TypeScript parity-diff
  evidence for frames and inputs, plus normal merge gates. This document neither
  supplies that evidence nor authorizes a harness/CI change.
