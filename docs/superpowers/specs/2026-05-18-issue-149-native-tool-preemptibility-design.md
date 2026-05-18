# Issue #149 Native Tool Preemptibility Design

Status: design draft
Date: 2026-05-18
Tracking: #149, #159 R3
Related audit: `docs/superpowers/audits/2026-05-12-deadline-plumbing-audit.md`
Filed follow-ups: #230, #231, #232, #233, #234, #235

## Goal

Define the final runtime boundary for native filesystem tools so request
deadlines and request cancellation cannot be defeated by filesystem traversal
that does not yield to Tokio.

The design target is the operational theorem described in #159 R3:

> When the daemon advances time past a request's deadline and a tool call is in
> Running state, the daemon takes a transition that lands the tool in TimedOut
> within bounded steps.

PR #161 closed the cooperative async-tool version of this property. The
remaining question is what boundary is strong enough for native work that can
block inside synchronous filesystem calls.

## Current Runtime Shape

The intended #161 path is:

- `runtime::wrap_tool` wraps every daemon tool
  (`crates/defra-agent/src/tool_call_lifecycle/runtime.rs:57`).
- `RuntimeManagedTool::call` uses `tokio::select!` over cancellation,
  deadline, and `self.inner.call(args)` (`runtime.rs:98`).
- The daemon scopes stream polling with `scope_request_tool_execution` inside
  `await_with_request_deadline` (`crates/defra-agent/src/agent/daemon/inference.rs:243`).
- If the wrapper observes the deadline, the hook maps the managed timeout
  marker to `ToolCallLifecycle::timeout()` (`crates/defra-agent/src/hook/persistence.rs:267`).

That path is sufficient for futures that yield. The test in
`runtime.rs:202` proves a pending async tool reaches the managed timeout marker.

Native filesystem traversal is different. `GlobTool::call` enters
`collect_glob_matches` (`crates/defra-agent/src/toolset/file_tools.rs:322`),
which recurses through `collect_glob_matches_inner`
(`crates/defra-agent/src/toolset/shared/filesystem.rs:226`). The directory
boundary is `sorted_children`, which calls `std::fs::read_dir`
(`filesystem.rs:359`). `grep` has the same traversal class and also performs
`std::fs::read_to_string` (`filesystem.rs:330`). `list_files` shares the
recursive `sorted_children` path.

As of post-#228 main at `79e6f64`, the file tools already have an important
mitigation: `run_filesystem_boundary` wraps `list_files`, `glob`, and `grep`
in `tokio::task::spawn_blocking`
(`crates/defra-agent/src/toolset/file_tools.rs:22`). The regression test
`native_filesystem_deadline_preempts_single_poll_blocker_and_advances_queue`
pins that behavior (`crates/defra-agent/src/toolset/tests.rs:287`), and the
Lean conformance witness family records the current boundary as
`spawnBlockingRuntimeBoundary`
(`crates/defra-agent/proofs/Proofs/Conformance/ContractCases/BoundaryRuntime.lean:237`).

That mitigation changes the problem from "the runtime cannot observe the
deadline at all" to "the runtime can stop awaiting the blocking work, but it
cannot stop the work." Once a `spawn_blocking` closure starts, dropping the
join handle does not abort a blocked `read_dir` or recursive walk. The worker
thread may continue after the tool row has timed out and the queue has moved
on. That is acceptable as a short-term queue-liveness fix, but it is not true
preemptibility.

## Design Options

### Option A: Subprocess Migration

Move native filesystem tools behind a managed subprocess boundary, modeled on
Codex `unified_exec`.

Reference inspected:

- `/Users/johnzampolin/go/src/github.com/openai/codex/codex-rs/core/src/unified_exec/`
- `mod.rs` defines the process manager, bounded process store, yield-time caps,
  and output caps.
- `process.rs` owns a process handle, output buffer, cancellation token, and
  `Drop` path that terminates the child.
- `process_manager.rs` centralizes spawn, output collection until deadline,
  process pruning, exit watching, and background polling.

Defra does not need the full interactive Codex surface for native filesystem
tools. The recommended subset is a single-shot managed-exec layer:

1. Parent serializes a filesystem-tool request as JSON.
2. Parent spawns a small native runner binary with stdin/stdout pipes, a clean
   environment, and cwd rooted at the configured tool root.
3. The runner executes the existing filesystem algorithms and emits one JSON
   response envelope.
4. Parent waits on child exit, request deadline, or cancellation.
5. On timeout/cancel, parent marks the `AgentToolCall` terminal immediately,
   sends a process-group termination signal, escalates if needed, and reaps in
   the background.

The runner should be a Defra binary rather than shelling out to `find`, `grep`,
or platform tools. That preserves the current ignore rules, path display,
structured output, and test fixtures while avoiding quoting/platform drift.

Process ownership should be group-scoped on Unix: spawn the runner into its own
process group/session, terminate the group on timeout, then escalate after a
short grace interval. Windows can initially be out of scope or use a Job Object
when Windows support becomes required. Existing bash tools already demonstrate
the shape of subprocess timeouts (`crates/defra-agent/src/toolset/shared/command.rs:122`,
`command.rs:147`, `crates/defra-agent/src/toolset/bash_tools.rs:106`), but the
native-tool boundary needs stronger ownership than `kill_on_drop(true)` alone.

Pros:

- True preemption of native tool work: the daemon can release the request and
  stop the traversal process.
- No unbounded accumulation of blocked Tokio blocking threads.
- Better operational evidence: child pid/process group, kill time, exit status,
  and reap status can be surfaced in `/healthz` or `/status`.
- Aligns with future sandbox tiers because native tools become ordinary
  managed execution.
- Easier to state the Lean theorem: timeout is a daemon transition that marks
  the tool `TimedOut` and signals the managed executor.

Cons:

- Higher implementation cost than the current `spawn_blocking` boundary.
- Requires a runner protocol, runner binary packaging, and process ownership
  tests.
- Adds platform-specific process-group behavior.
- File-tool code must be split so the runner can reuse the traversal logic
  without pulling daemon-only dependencies.
- Needs careful output caps and partial-output handling to avoid replacing one
  liveness problem with an output-memory problem.

### Option B: Cooperative Pre-Yield

Keep native tools in-process, but make the synchronous body run on the blocking
pool and check deadline/cancellation at every directory boundary.

The current codebase already implements the first half of this option through
`run_filesystem_boundary`. A complete Option B would add a small cancellation
context to `collect_entries_inner`, `collect_glob_matches_inner`, and
`collect_grep_matches_inner`, checking it:

- before each `sorted_children(dir)` call;
- after each `sorted_children(dir)` return;
- before recursing into a subdirectory;
- before each `read_to_string` in grep;
- before rendering final output.

Pros:

- Smallest delta from the current implementation.
- Preserves current tool functions, output, and tests.
- Gives best-effort early exit between syscalls and directory boundaries.
- Works on every platform Tokio supports.
- Useful even if Option A is chosen, because the runner can reuse the same
  cooperative checks internally.

Cons:

- It cannot interrupt an in-flight `std::fs::read_dir`, metadata call, or file
  read.
- A blocked `spawn_blocking` worker continues after the parent future returns.
- The strongest true theorem is about the awaiting request, not the native work
  itself.
- Thread-pool pressure can become the next soak failure mode if many requests
  timeout while their blocking closures keep running.
- Health reporting can detect aged blocking work but cannot stop it.

### Option C: Soft-Cancel-Only Recovery and Observability

Accept the synchronous gap. Strengthen startup recovery so stale running tool
rows become `timedOut` more aggressively, and add `/healthz` or `/status`
fields that report:

- active request id;
- active tool name;
- active tool started-at;
- age since last progress;
- count of expired processing requests/tool calls.

Pros:

- Lowest implementation risk.
- Improves operator diagnosis for the exact blind spot in #149.
- Compatible with either Option A or B later.
- No runner packaging or process-management work.

Cons:

- Does not fix live convergence.
- Does not free the single-worker queue while the native tool is blocking.
- Still depends on operator restart or daemon recovery for terminalization.
- Cannot satisfy #159 R3's runtime liveness theorem.

## Recommendation

Choose Option A as the target design, with the existing Option B
`spawn_blocking` boundary retained as the interim mitigation.

The reason is simple: #149 is a liveness bug, not only a bookkeeping bug. The
runtime must be able to stop the unit of work that can block. `spawn_blocking`
lets the daemon stop waiting and frees the request path in the common case, but
it does not create ownership over the native work after the timeout branch wins.
A managed subprocess does. It gives the daemon a concrete handle to signal,
observe, reap, and report, which maps cleanly to both the operational acceptance
criteria and the Lean liveness statement.

The design should not discard the current mitigation. It is a useful bridge and
a guardrail while the subprocess boundary lands. It also lowers risk during the
migration because the file traversal code can keep cooperative checks inside
the runner. But the closure criterion for "hard timeout" should be process
ownership, not merely dropping a join handle.

## Lean State Machine Implications

Option A should introduce a small executor machine, likely under
`crates/defra-agent/proofs/Proofs/ManagedExec/`.

Suggested states:

- `PendingSpawn`
- `Running`
- `Exited`
- `KillSignaled`
- `Killed`
- `SpawnFailed`
- `ReapFailed`

Suggested events/transitions:

- `spawn`: request/tool call creates a child executor.
- `observe_exit_success`: child exits zero and the tool can complete.
- `observe_exit_failure`: child exits non-zero and the tool can fail.
- `deadline_elapsed`: daemon marks the linked tool `TimedOut` and moves the
  executor to `KillSignaled`.
- `cancel_requested`: daemon marks the linked tool `Cancelled` and moves the
  executor to `KillSignaled`.
- `kill_observed`: child is reaped after signal.
- `reap_failed`: signal/reap failed, but the tool row remains terminal and the
  daemon records an executor cleanup failure.

The key theorem should be stated over the composed request/tool/executor state:

```lean
theorem running_tool_times_out_after_deadline_bounded
    (pre : ManagedExecComposedState)
    (h_running : pre.tool.state = .running)
    (h_exec : pre.exec.state = .running)
    (h_deadline : pre.request.deadline < pre.now) :
    exists post,
      BoundedTrace pre post maxTimeoutSteps
      /\ post.tool.state = .timedOut
      /\ post.exec.state = .killSignaled
```

The theorem should not require the OS process to have exited within the same
bound. The operational requirement is that the daemon releases the request and
terminalizes the tool within bounded steps. Reaping can be a second liveness
property with a boundary assumption such as "the OS eventually reports process
exit after a kill signal, or the daemon records `ReapFailed`."

If Option B remains as the only runtime boundary, the existing
`NativeFilesystemBoundaryCase` witnesses should be retained but renamed in the
spec narrative as awaiter-liveness witnesses, not executor-liveness witnesses.
They prove "queue advances before blocker returns"; they do not prove "the
blocking work is killed."

## Conformance Contract Additions

For Option A, add a `ManagedExec` contract family rather than overloading the
current native filesystem boundary rows.

Proposed additions:

- `ManagedExecState` vocabulary: `pendingSpawn`, `running`, `exited`,
  `killSignaled`, `killed`, `spawnFailed`, `reapFailed`.
- `ManagedExecTransitionCase` rows for legal and illegal state transitions.
- `ManagedExecLivenessCase` rows for:
  - running child plus expired request deadline -> tool `timedOut` and executor
    `killSignaled` within the bound;
  - running child plus request interrupt -> tool `cancelled` and executor
    `killSignaled` within the bound;
  - fast child exit -> tool `completed`, no kill signal;
  - child non-zero exit -> tool `failed`, no timeout;
  - timeout with partial stdout -> terminal timeout envelope preserves captured
    output metadata.
- Coverage ledger entry tying the rows to Rust tests in the managed-exec crate
  and the migrated file-tool tests.

The existing `native_filesystem_boundary_cases` can stay as the current
`spawnBlockingRuntimeBoundary` witness family until migration completes. After
Option A lands, either replace those rows with
`managedExecProcessGroupBoundary` rows or keep both families so tests prove the
old boundary is no longer used by migrated tools.

Runtime observability likely does not require a persisted schema change. If the
implementation stores executor metadata on `AgentToolCall`, add optional fields
only after a separate schema decision:

- `executor_kind`
- `executor_pid`
- `executor_started_at`
- `executor_kill_signaled_at`
- `executor_reaped_at`
- `executor_exit_code`

The lighter default is to keep those in daemon memory and expose them through
health/status, while the lifecycle row remains the durable source of truth for
`timedOut`, `cancelled`, `failed`, and `completed`.

## Scope Split

This work should be multi-PR. Natural boundaries:

1. **Lean ManagedExec spec skeleton.** Add `Proofs/ManagedExec/` state,
   transition, executable step, and first liveness theorem shape. No Rust.
2. **Conformance contract rows.** Emit `ManagedExec` vocabulary, transition,
   and liveness witness rows; add coverage-ledger entries and Rust consumers
   that initially assert pending implementation status.
3. **Managed-exec Rust crate/module.** Add the single-shot process manager with
   process-group spawn, timeout, cancel, output caps, and reap reporting.
4. **Native filesystem runner protocol.** Add the runner request/response
   schema and runner binary, reusing current filesystem traversal behavior.
5. **Migrate `glob` and `list_files`.** Route the highest-risk traversal tools
   through managed exec; keep output snapshots stable.
6. **Migrate `grep` and decide `read_file`.** Move grep; decide whether
   `read_file` follows for uniformity or remains `tokio::fs::read`.
7. **Lifecycle integration.** Wire managed-exec timeout/cancel outcomes into
   `ToolCallLifecycle` and request cancellation paths.
8. **Observability.** Add `/healthz` or `/status` active request/tool/executor
   age fields and expired-processing counts.
9. **Recovery and orphan cleanup.** Startup sweep handles persisted running
   tool rows and any recorded managed-exec orphan metadata.
10. **Soak closeout.** Add the #149 soak replay gate and document the closure
    criteria.

## Deferred Decisions

The following decisions and cross-cutting work are tracked as filed sub-issues:

| Issue | Title | Scope |
| --- | --- | --- |
| #230 | Issue #149 R3: model ManagedExec liveness in Lean | Decide standalone machine vs ToolExecution extension; add state, transition, and liveness theorem shape. |
| #231 | Issue #149 R3: choose ManagedExec Rust crate boundary and process ownership | Decide crate/module boundary, Unix process-group API, and first-platform support. |
| #232 | Issue #149 R3: define native filesystem runner protocol | Decide shared runner vs per-tool binary, JSON vs streaming protocol, shared traversal library shape, and `read_file` migration. |
| #233 | Issue #149 R3: add ManagedExec conformance witness rows | Emit ManagedExec vocabulary, transition rows, liveness rows, and Rust coverage consumers. |
| #234 | Issue #149 R3: expose native tool executor liveness in health/status | Decide memory-only vs persisted executor metadata and expose active tool/executor age counters. |
| #235 | Issue #149 R3: define soak closeout gate for native tool preemptibility | Define the replay/soak evidence required before treating #149/R3 as operationally closed. |
