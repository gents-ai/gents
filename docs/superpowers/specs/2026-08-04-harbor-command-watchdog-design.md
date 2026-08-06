# Harbor foreground command watchdog (#1018)

## Problem

The Harbor Terminal-Bench profile sets `GENTS_COMMAND_TIMEOUT_SECS=86400`,
equal to the whole-request deadline. That turns the #985 foreground command
ceiling into a no-op: in the full Terminal-Bench 2.1 run, five trials sat
19–22 hours on pathological foreground commands (`./enc`, recursive `grep`
over `/`, QEMU probes) with zero inference work running.

Under #985 semantics the operator ceiling doubles as both the
omitted-`timeout_secs` default and the hard cap on explicit requests. A single
knob therefore cannot express "return control quickly by default, but let the
model opt into a longer run for a known-long build."

## Decision

Decouple the foreground default from the model-requestable maximum
(approach A from brainstorming):

- `--command-timeout-secs` keeps meaning "what an omitted `timeout_secs`
  gets" — today's reading, unchanged.
- New optional `--command-timeout-max-secs` raises the explicit-request
  ceiling above the default. Unset ⇒ max = default, which is byte-for-byte
  the current #985 behavior, so existing operators migrate by doing nothing.
- Harbor configures 600s default / 3600s max against the unchanged 86400s
  request deadline.

Rejected alternatives: inverted flag naming (existing flag becomes the cap —
equally compatible, less intuitive), and building #899's general budget
derivation now (correct end state, but a design-scale effort that would stall
the concrete Harbor fix; this knob pair is shaped so #899 can subsume it).

## Runtime changes (`crates/gents`)

- `resolve_command_timeout` (toolset/shared.rs) gains `foreground_max`
  alongside `foreground_default`:
  - omitted `timeout_secs` → `foreground_default`;
  - explicit `timeout_secs` → clamped to `[1, foreground_max]`;
  - background path unchanged (`BACKGROUND_COMMAND_TIMEOUT_SECS` = 10h);
  - `run_command` still takes `min(command deadline, request deadline)`.
- `ToolCeiling` (tool_surface/modes.rs) carries `command_timeout_max` with a
  `with_command_timeout_max_secs` builder. The effective max is
  `max(command_timeout, command_timeout_max)`, so a misconfigured pair can
  never push the cap below the default.
- Bash tool constructors and `timeout_secs_schema` (toolset/bash_tools.rs)
  take both values. The model-visible schema becomes
  `default: <default>, maximum: <max>` with a description stating the max is
  the foreground cap and that backgrounded runs get the background lifetime
  budget instead.

Not Lean-first: this is pure configuration resolution. No request/tool-call
lifecycle transition, invariant, or provider-input change. The fence is the
unit/conformance tests in `toolset/tests.rs`.

## CLI changes (`crates/gents-cli`)

- `gents server --command-timeout-max-secs <secs>` (optional). Threads into
  `ToolCeiling` in serve.rs.
- The existing startup `tracing` line that records `command_timeout_secs`
  also records the effective max, so the pair lands in `gents-server.log`,
  which Harbor retains as runtime evidence. Per-call attribution already
  exists: every persisted command result carries `timeout_ms`, `timed_out`,
  and `status: "timeout"`.

## Harbor adapter changes (`scripts/harbor`)

- `run_gents.sh`: `GENTS_COMMAND_TIMEOUT_SECS` default 86400 → **600**; new
  `GENTS_COMMAND_TIMEOUT_MAX_SECS` default **3600**; both passed to
  `gents server`. `GENTS_REQUEST_TIMEOUT_SECS` stays 86400.
- `README.md`: overrides table documents the two command values separately
  from the request deadline; the example invocation drops its explicit
  `GENTS_COMMAND_TIMEOUT_SECS=86400` line.

## Acceptance criteria mapping

| Criterion (#1018) | How it is met |
|---|---|
| Harbor ceiling materially below request deadline, documented separately | 600/3600 vs 86400; README table split |
| Timed-out command → normal tool outcome | Existing #985 machinery (`status: "timeout"`, partial stdout/stderr) |
| Cancellation/teardown kills process tree | Existing `managed_exec` setsid + group SIGTERM→SIGKILL |
| Effective deadline in runtime evidence | Per-call `timeout_ms` in persisted results; default+max in server startup log |
| Runaway process cannot hold a slot 24h | Foreground bounded at ≤3600s/command; turn ceiling bounds retries |
| Coordinate with #899/#729, no unexplained constant | Knob pair documented; shaped for later #899 derivation; no new global |

## Testing

- Extend the `resolve_command_timeout` table tests: omitted → default;
  explicit under max; explicit over max clamped; max unset ⇒ behaves as
  today (max = default).
- Schema test asserting `default` and `maximum` diverge when configured.
- Serve-args test for `--command-timeout-max-secs`.
- Shell-level sanity on `run_gents.sh` (defaults present, both flags passed).
- Gates: `cargo test -p gents`, `cargo check --workspace --all-targets`.
