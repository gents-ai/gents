# Issue #149 R3 Soak Closeout Gate

Status: proposed closeout gate
Date: 2026-05-19
Tracking: #149, #235
Design reference: `docs/superpowers/specs/2026-05-18-issue-149-native-tool-preemptibility-design.md`

This gate defines the operational evidence required before #149/R3 is treated
as closed in production. It does not replace the design spec; it is the replay
and evidence checklist for the native filesystem preemptibility fix.

## Replay Shape

Run the replay on a Unix deployment. Windows process-tree termination remains
out of scope for this gate and is tracked by #236.

The blocking request must use a native filesystem traversal tool routed through
the native runner: `glob`, `list_files`, or `grep`. `glob` is the preferred
replay because it matches the original #149 stall shape. The target must be a
directory tree that takes longer than the request deadline to traverse. In local
regression tests, the deterministic substitute is
`DEFRA_NATIVE_FS_RUNNER_BLOCK_DIR` plus `DEFRA_NATIVE_FS_RUNNER_BLOCK_MS`, with
the block duration longer than the request deadline.

The runtime must be configured as a single-worker deployment:
`max_concurrent = 1`. Submit:

1. A first `AgentRequest` that invokes the blocking native filesystem tool with
   a tight deadline.
2. A second `AgentRequest` queued behind it, using any quick tool or prompt that
   proves the worker advances after the first request times out.

While the blocker is in flight, poll `/status`, `/healthz`, and Prometheus often
enough to capture the active request/tool/executor window. If the timeout path
is too fast to sample manually, rerun with a deterministic blocker that survives
long enough to capture the degraded sample; missing observability evidence is
not closure evidence.

## Required Assertions

- No `AgentToolCall` row remains `running` or legacy `called` after its
  deadline plus the bounded timeout/kill window.
- No `AgentRequest` row remains `processing` after its deadline plus the bounded
  timeout/kill window.
- The single-worker queue picks up the second request without a daemon restart.
- `/healthz` reports `status = degraded` while
  `expired_processing_count > 0`, then returns to `ok` after timeout recovery.
- `/status` exposes the active request, active tool call, active native
  executor, and executor age while the blocker is in flight.
- Prometheus exposes the same liveness shape through
  `defra_agent_expired_processing_count`,
  `defra_agent_active_tool_calls`, and
  `defra_agent_active_native_executors`.

The row assertions are the closure boundary. Health/status/metrics are the
operator visibility boundary that prevents a future #149-style stall from being
silent.

## Automated Regression Coverage

The checked-in CI gate is intentionally unit-level plus deterministic native
runner replay, not a full live DefraDB/HTTP soak harness.

- `crates/defra-agent/src/toolset/tests.rs`:
  `native_filesystem_deadline_preempts_single_poll_blocker_and_advances_queue`
  runs `glob` through `defra-native-fs-runner`, blocks traversal longer than
  the request deadline, asserts the managed timeout marker, and proves the next
  queued tool can run before the blocker duration elapses.
- `crates/defra-agent/src/managed_exec/tests.rs`:
  `managed_exec_deadline_kills_process_group` proves the process-group kill
  path, active native executor snapshot fields (`pid`, tool name, `argv0`,
  `started_at`, age), snapshot cleanup after reap, and successful execution of
  a second managed process after the timeout.
- `crates/defra-agent-cli/src/http/liveness.rs` and `healthz.rs` cover
  `expired_processing_count`, active tool-call liveness, and `/healthz`
  degradation from expired processing rows.

This split is sufficient for CI because it pins the native runner boundary, the
managed-exec preemption boundary, and the HTTP liveness projection separately.
The full DB/HTTP queue proof is required as soak evidence below rather than as a
slow ignored integration test in this repository.

Local verification command:

```bash
cargo test -p defra-agent native_filesystem_deadline_preempts_single_poll_blocker_and_advances_queue
cargo test -p defra-agent managed_exec_deadline_kills_process_group
```

When changing the HTTP liveness surface, also run:

```bash
cargo test -p defra-agent-cli healthz_reports_degraded_when_expired_processing_count_positive
```

## Evidence Required For Closure

Before the controller treats #149/R3 as operationally closed, attach these
artifacts to the closeout thread or PR:

- A green CI run that includes `cd crates/defra-agent/proofs && lake build`,
  `cargo check -p defra-agent`, and `cargo test -p defra-agent`.
- A documented Unix soak replay with the exact request IDs, tool call IDs,
  deadlines, and the single-worker backend configuration.
- DefraDB row snapshots before, during, and after the blocker showing the
  blocking `AgentToolCall` and `AgentRequest` become terminal and do not remain
  `running`/`called` or `processing` past the bounded timeout window.
- `/healthz` payloads showing degraded while
  `expired_processing_count > 0` and recovery to `ok`.
- `/status` payloads showing the active request, active tool call, active native
  executor, and executor age while the blocker is in flight.
- A Prometheus snapshot covering
  `defra_agent_expired_processing_count`,
  `defra_agent_active_tool_calls`, and
  `defra_agent_active_native_executors`.
- Daemon logs, if available, showing timeout classification and managed-exec
  kill/reap completion without a daemon restart.

## Acceptance: What Re-Opens #149

Re-open #149, or file a direct regression child linked to #149, if any later
soak run shows one of these failures on Unix:

- A native filesystem `glob`, `list_files`, or `grep` call remains
  `running`/`called` past the bounded timeout window.
- An `AgentRequest` remains `processing` past the bounded timeout window.
- A single-worker queue requires daemon restart before the next request runs.
- `/healthz` stays `ok` while expired processing work is visible.
- `/status` or Prometheus no longer expose the active native executor and age
  during the blocking window.

Operational closure means this replay is covered and observable. It does not
mean the bug class can never regress.
