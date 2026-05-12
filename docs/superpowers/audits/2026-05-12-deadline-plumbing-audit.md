# Deadline Plumbing Audit - Issue #149

Date: 2026-05-12

Scope: production Rust runtime, protocol schemas, existing Rust tests, and existing Lean proof surfaces for request, tool-call, admission, trigger, and subagent lifetimes. I treated comments and specs as hints only; the verdicts below come from the Rust paths.

Bottom line: PR #161 closes the cooperative async-tool version of issue #149, but it does not fully close the native `glob` failure from #149. The live deadline path still depends on the active future yielding back to `tokio::select!`; native filesystem traversal is synchronous and has no internal deadline/cancellation point. A stuck `glob` can therefore still hold the request until restart recovery.

## Entities audited

| Entity name | Source file:line | Deadline source | Recovery path | Lean property covering it |
| --- | --- | --- | --- | --- |
| Behavior request deadline policy (`InferenceProfile.deadline_duration_secs`, default 900s) | `crates/defra-agent-protocol/schemas/inference/inference_profile.graphql:9`, `crates/defra-agent/src/config.rs:16`, `crates/defra-agent/src/agent.rs:212` | Inference profile field, default `DEFAULT_DEADLINE_DURATION_SECS` | Feeds `BehaviorConfig.deadline_duration`, then `RequestLifecycle::claim_inner` writes `AgentRequest.deadline` | S4/L1/L2 via request deadline model (`crates/defra-agent/proofs/README.md:362`, `crates/defra-agent/proofs/README.md:407`) |
| Claimed request deadline (`AgentRequest.deadline`) | `crates/defra-agent-protocol/schemas/agent/agent_request.graphql:24`, `crates/defra-agent/src/lifecycle/claim.rs:212`, `crates/defra-agent/src/agent/daemon/inference.rs:89` | Claim time plus behavior deadline duration | Live waits are wrapped by `await_with_request_deadline`; process errors call `lifecycle.fail`; startup `RequestLifecycle::recover_all` sweeps processing rows | S4 `completed_not_deadline_expired` / `deadline_structural_bound`, L1/L2/L3 (`crates/defra-agent/proofs/Proofs/Properties/Safety.lean:139`, `crates/defra-agent/proofs/README.md:407`) |
| Pre-claim request TTL (`AgentRequest.valid_until`) | `crates/defra-agent-protocol/schemas/agent/agent_request.graphql:28`, `crates/defra-agent/src/lifecycle/claim.rs:181` | Submitter-provided RFC3339 TTL | Claim checks `valid_until`; expired pending requests transition to `dead/Stale`; watcher fallback polling re-observes missed pending rows | `claim_requires_ttl_open`, `claim_with_ttl_bounds_time` (`crates/defra-agent/proofs/Proofs/Request/Properties.lean:131`, `crates/defra-agent/proofs/Proofs/Request/Properties.lean:160`) |
| Request interrupt lifetime (`AgentRequest.interrupt_requested_at`) | `crates/defra-agent-protocol/schemas/agent/agent_request.graphql:27`, `crates/defra-agent/src/interrupt.rs:175`, `crates/defra-agent/src/agent/daemon/inference.rs:216` | User/API writes interrupt timestamp | Per-request observer polls every 2s; daemon cancels request token and calls `cancel_in_flight_tool_calls`; startup tool recovery cancels rows whose parent is interrupted | `interrupted_request_cancels_live_linked_tools` (`crates/defra-agent/proofs/Proofs/Composed.lean:220`) |
| Tool-call persisted deadline/lifetime (`AgentToolCall.started_at`, `deadline_at`, `completed_at`) | `crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql:12`, `crates/defra-agent/src/hook/persistence.rs:207`, `crates/defra-agent/src/tool_call_lifecycle/transition.rs:76` | Hook uses active request deadline; fallback is now + default deadline if no active request | Runtime marker path maps timeout/cancel to terminal states; live request-deadline sweep calls `timeout_expired_tool_calls`; startup `ToolCallLifecycle::recover_all` sweeps expired running rows | Tool execution liveness and composed deadline theorem (`crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean:94`, `crates/defra-agent/proofs/Proofs/Composed.lean:272`) |
| Tool runtime wrapper (`scope_request_tool_execution`, `wrap_tool`) | `crates/defra-agent/src/tool_call_lifecycle/runtime.rs:38`, `crates/defra-agent/src/tool_call_lifecycle/runtime.rs:57`, `crates/defra-agent/src/tool_call_lifecycle/runtime.rs:98` | Request task-local deadline and cancellation token | `tokio::select!` returns timeout/cancel marker; hook maps marker to `timedOut`/`cancelled` | Same as tool-call liveness; Rust tests cover pending futures (`crates/defra-agent/src/tool_call_lifecycle/runtime.rs:202`) |
| Native filesystem traversal (`glob`, also `grep`/list traversal class) | `crates/defra-agent/src/toolset/file_tools.rs:306`, `crates/defra-agent/src/toolset/shared/filesystem.rs:226`, `crates/defra-agent/src/toolset/shared/filesystem.rs:359` | Only the outer `AgentToolCall.deadline_at` | No internal recovery while traversal is inside one synchronous poll; startup recovery can terminalize only after process restart | Lean expects live terminalization, but Rust deviates for single-poll blocking native work |
| In-flight hook map (`DefraSessionHook.in_flight_lifecycles`) | `crates/defra-agent/src/hook.rs:230`, `crates/defra-agent/src/hook.rs:374`, `crates/defra-agent/src/hook.rs:475` | Same `ToolCallLifecycle.deadline_at` | Live timeout/cancel/fail drains map; hook `Drop` only clears memory and leaves rows for startup recovery | Tool-call recovery/liveness, but Drop is a restart-only path |
| Stream liveness timeout | `crates/defra-agent/src/config.rs:16`, `crates/defra-agent/src/agent/daemon/inference.rs:199`, `crates/defra-agent/src/agent/daemon/inference.rs:271` | `DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS` = 300 | Inner stream wait timeout calls `fail_in_flight_tool_calls`; request surfaces stream liveness error | No dedicated Lean timeout property; terminal tool/request behavior is covered by tool/request liveness |
| One-shot tool execution path | `crates/defra-agent/src/oneshot.rs:48`, `crates/defra-agent/src/oneshot.rs:123` | Hook fallback deadline only, because no active request deadline is set | No request-scoped `wrap_tool`, no request lifecycle, no live deadline sweep; startup can only sweep persisted rows if there is a persisted running tool row | none - deviation |
| Native command/bash/CLI subprocess timeouts | `crates/defra-agent/src/toolset/shared/command.rs:122`, `crates/defra-agent/src/toolset/shared/command.rs:147`, `crates/defra-agent/src/toolset/cli_tool.rs:118`, `crates/defra-agent/src/toolset/bash_tools.rs:106` | Per-tool `timeout_secs`, capped by config/default | `tokio::time::timeout` around child process; `kill_on_drop(true)` for subprocesses; outer lifecycle completes/fails normally | none for local subprocess timeout; covered operationally by Rust tests (`crates/defra-agent/src/toolset/tests.rs:1097`) |
| MCP `call_tool` and MCP preflight timeout | `crates/defra-agent/src/meta_tools/call.rs:96`, `crates/defra-agent/src/meta_tools/call.rs:115`, `crates/defra-agent/src/meta_tools/call.rs:163` | 120s when service health is stale, else 300s; 30s preflight | Local timeout returns error; outer request/tool deadline also wraps daemon calls | Tool lifecycle terminalization; backend-health cases for health gate (`crates/defra-agent/proofs/README.md:415`) |
| MCP health checker probe/staleness lifetime | `crates/defra-agent/src/health_checker.rs:23`, `crates/defra-agent/src/health_checker.rs:247`, `crates/defra-agent/src/health_checker.rs:250` | 30s health interval, 120s heartbeat staleness, 5s probe timeout | Probe timeout marks service unreachable and evicts MCP pool connection | none - operational health path |
| Admission `InferenceCall` queued/running/terminal lifetime | `crates/defra-agent-protocol/schemas/inference/inference_call.graphql:15`, `crates/defra-agent/src/admission/controller.rs:146`, `crates/defra-agent/src/admission/permit.rs:137` | No own deadline; bounded by surrounding request deadline/cancel token in daemon | Queued guard and permit Drop terminalize live cancelled/dropped calls; no startup sweep found for persisted `queued`/`running` rows | InferenceCall slot accounting and permit-drop terminalization (`crates/defra-agent/proofs/Proofs/InferenceCall/SlotAccounting.lean:171`, `crates/defra-agent/proofs/README.md:426`) |
| Streaming response lifetime (`AgentResponse.status=streaming`) | `crates/defra-agent/src/streaming.rs:101`, `crates/defra-agent/src/streaming.rs:426`, `crates/defra-agent/src/lifecycle/recovery.rs:103` | Stream writer batch interval and request lifecycle terminalization | Finalize writes terminal response; startup recovery marks streaming responses `error` and creates missing error responses | L3 recovery convergence (`crates/defra-agent/proofs/README.md:409`) |
| Watcher fallback poll and processed-request cooldown | `crates/defra-agent/src/watcher/cooldown.rs:7`, `crates/defra-agent/src/watcher/cooldown.rs:8`, `crates/defra-agent/src/watcher.rs:113` | 30s fallback poll, 30s local processed-id cooldown | Missed gossip falls back to full pending-request query; processed-id cache is pruned and capped | none - runtime observation path |
| Interrupt observer poll cadence | `crates/defra-agent/src/interrupt.rs:175`, `crates/defra-agent/src/interrupt.rs:195` | 2s DB poll interval | Observer signals daemon; daemon cancel path reaches request/tool/admission lifecycles | `interrupted_request_cancels_live_linked_tools` |
| Runtime control watcher debounce/settle window | `crates/defra-agent/src/agent/runtime/control_watcher.rs:14`, `crates/defra-agent/src/agent/runtime/control_watcher.rs:18`, `crates/defra-agent/src/agent/runtime/control_watcher.rs:137` | 5s debounce, 60s settle window, 1s settle retry | Dropped events force full reconcile; unresolved references keep retrying and publish error | none - operational reconcile path |
| Schedule trigger lifetime (`Schedule.interval_secs`, `next_run_at`) | `crates/defra-agent-protocol/schemas/agent/schedule.graphql:4`, `crates/defra-agent-protocol/schemas/agent/schedule.graphql:7`, `crates/defra-agent/src/trigger_engine/schedule_source.rs:91` | Persisted `next_run_at` plus `interval_secs` | Tick loop seeds missing `next_run_at`; fired/skipped advances next run; errors do not advance so next tick retries | Trigger dispatch/serial properties, but runtime writeback convergence is outside Lean (`crates/defra-agent/proofs/README.md:491`) |
| Event trigger runtime lifetime (`EventTrigger.last_*`, `seen_docs`) | `crates/defra-agent-protocol/schemas/agent/event_trigger.graphql:11`, `crates/defra-agent/src/trigger_engine/event_source.rs:31`, `crates/defra-agent/src/trigger_engine/event_source.rs:84` | Process-lifetime first-seen cache and best-effort runtime fields | Subscription drives fires; dropped events only log; runtime-field writes are best effort | Trigger dispatch shape is covered; event delivery/resync is explicitly operational, not proven |
| Subagent spawn source (`AgentToolCall.child_request_id`, spawn args deadline) | `crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql:21`, `crates/defra-agent/src/trigger_engine/subagent_source.rs:37`, `crates/defra-agent/src/trigger_engine/subagent_source.rs:282` | Effective deadline is min(parent tool `deadline_at`, spawn args `deadline`) | Event-driven `SubagentSource` materializes child; startup `recover_orphan_subagent_children` materializes missed children after restart | Subagent `bridge_spawn`/link invariants (`crates/defra-agent/proofs/Proofs/Subagent/Transition.lean:39`) |
| Subagent child request deadline | `crates/defra-agent/src/tool_call_lifecycle/subagent_request.rs:130`, `crates/defra-agent/src/tool_call_lifecycle/subagent_request.rs:159`, `crates/defra-agent/src/lifecycle/claim.rs:212` | Caller passes effective deadline into child `AgentRequest.deadline` | No convergent path: watcher does not load `deadline`, claim overwrites it with now + behavior duration | none - deviation from deadline propagation |
| Subagent bridge terminal lifetime (`await_mode`, `cancel_policy`, `child_request_id`) | `crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql:19`, `crates/defra-agent/src/tool_call_lifecycle/transition.rs:306`, `crates/defra-agent/src/tool_call_lifecycle/recovery.rs:224` | Parent tool deadline plus child request terminal state | `bridge_complete`/`bridge_failure` exist but no production caller was found; recovery skips detached subagent tool rows even when an outcome exists | Subagent bridge transition model (`crates/defra-agent/proofs/Proofs/Subagent/Transition.lean:76`, `crates/defra-agent/proofs/Proofs/Subagent/Transition.lean:105`) |
| Subagent lineage/depth (`caused_by_parent_*`, `subagent_depth`) | `crates/defra-agent-protocol/schemas/agent/agent_request.graphql:29`, `crates/defra-agent/src/tool_call_lifecycle/subagent_request.rs:162`, `crates/defra-agent/src/watcher.rs:40` | Not a deadline; it is the recovery/cascade link | Creation validates parent/depth, watcher rejects incoherent rows, recovery uses parent link for orphan children and cascade interrupt | Subagent depth/link invariants (`crates/defra-agent/proofs/Proofs/Subagent/Properties.lean:80`) |
| Trigger lineage/concurrency (`caused_by_trigger_id`, `caused_by_trigger_kind`) | `crates/defra-agent-protocol/schemas/agent/agent_request.graphql:19`, `crates/defra-agent/src/lifecycle/materialize.rs:10`, `crates/defra-agent/src/trigger_engine/production_materializer.rs:156` | Not a deadline; it gates serial/latest-only runtime lifetimes | Trigger materializer writes lineage; serial/latest-only query or supersede active runtime requests by lineage | Trigger dispatch/lineage and serial/latest-only proofs (`crates/defra-agent/proofs/README.md:491`) |
| Health/status observability for expired processing | `crates/defra-agent/src/hook.rs:26`, `crates/defra-agent/src/agent/runtime/startup.rs:334` | Should be active request/tool age and expired-processing counters | Existing counters cover hook persistence successes/failures and startup recovery logs only; no live active-tool/expired-processing counter found | none - deviation from #149 acceptance criteria |

## Per-entity verdict

- ✅ Closed - Behavior request deadline policy: the policy source is wired into `BehaviorConfig` and request claim; it is covered by request deadline properties.
- ⚠️ Partial - Claimed request deadline: request waits are bounded in normal async paths, but a synchronous native tool poll can prevent the deadline future from being observed until restart.
- ✅ Closed - Pre-claim request TTL: stale pending requests are converted to `dead/Stale` during claim, and watcher fallback polling makes missed pending events observable.
- ⚠️ Partial - Request interrupt lifetime: interrupt propagation exists, but it is cooperative and has the same non-preemptive native tool gap as request deadlines.
- ⚠️ Partial - Tool-call persisted deadline/lifetime: live timeout/cancel/recovery exists and is tested for pending async futures, but native synchronous filesystem work can still bypass live convergence.
- ⚠️ Partial - Tool runtime wrapper: the wrapper is correct for futures that yield, but `tokio::select!` cannot interrupt an inner future during a single blocking poll.
- ⚠️ Partial - Native filesystem traversal: `glob` has no internal timeout/cancel point, so it only converges via restart recovery if the traversal blocks indefinitely.
- ⚠️ Partial - In-flight hook map: live drains exist, but hook drop intentionally leaves running rows for startup recovery, which is not live convergence.
- ⚠️ Partial - Stream liveness timeout: it handles idle async streams, but it cannot fire while `stream.next()` is trapped inside a synchronous native tool poll.
- ❌ Open - One-shot tool execution path: tools are not wrapped and no request deadline is installed, so one-shot tool calls can hang with only a fallback persisted deadline.
- ✅ Closed - Native command/bash/CLI subprocess timeouts: subprocesses have local timeouts and `kill_on_drop`, with Rust coverage.
- ✅ Closed - MCP `call_tool` and preflight timeout: the mutating call path has explicit timeouts and daemon request deadlines also wrap it.
- ✅ Closed - MCP health checker probe/staleness lifetime: probes time out, mark unreachable, and evict cached connections.
- ⚠️ Partial - Admission `InferenceCall` lifetime: live Drop paths are covered, but no startup sweep terminalizes stale persisted `queued`/`running` call rows.
- ✅ Closed - Streaming response lifetime: normal finalize and startup recovery both converge streaming/missing response documents.
- ✅ Closed - Watcher fallback poll/cooldown: missed gossip falls back to polling and cooldown state is bounded.
- ⚠️ Partial - Interrupt observer poll cadence: the observer itself is bounded, but the downstream cancellation can still be blocked by a synchronous native tool.
- ✅ Closed - Runtime control watcher debounce/settle window: dropped control events force full reconcile and unresolved references retry inside a bounded settle window.
- ⚠️ Partial - Schedule trigger lifetime: schedule fires advance on success/skip, but post-fire runtime-field writes are spawned best-effort and can duplicate due fires if the write fails.
- ⚠️ Partial - Event trigger runtime lifetime: dropped events only warn and there is no live resync path; runtime-field writes are explicitly best effort.
- ⚠️ Partial - Subagent spawn source: missed `AgentToolCall` update events are repaired only by startup orphan-child recovery, not by a live scan.
- ❌ Open - Subagent child request deadline: the effective child deadline is written to the pending row and then silently overwritten at claim.
- ❌ Open - Subagent bridge terminal lifetime: bridge terminal transitions exist but are not wired to a production child-terminal observer, and detached rows are skipped by recovery.
- ✅ Closed - Subagent lineage/depth: creation and watcher coherence checks match the Lean depth/link invariants.
- ✅ Closed - Trigger lineage/concurrency: trigger lineage is written and used by serial/latest-only gates; stuck requests still inherit the request-deadline gap above.
- ⚠️ Partial - Health/status observability for expired processing: the #149 liveness counters are not present; only persistence counters and startup recovery logs exist.

## #149 specifically

Verdict: #161 did not fully close #149. It added the right persisted lifecycle states and cooperative runtime paths, but the native `glob` implementation still has no preemptible execution boundary.

Trace through the intended #161 path:

1. Runtime construction wraps daemon tools with `runtime::wrap_tool` (`crates/defra-agent/src/agent/runtime/context.rs:84`).
2. A request is claimed with `deadline = now + behavior.deadline_duration` (`crates/defra-agent/src/lifecycle/claim.rs:212`), and the daemon reads that as `lifecycle.claimed_deadline_at()` (`crates/defra-agent/src/agent/daemon/inference.rs:89`).
3. Before streaming, the daemon installs the active request id and request deadline on the hook (`crates/defra-agent/src/agent/daemon/inference.rs:137`, `crates/defra-agent/src/agent/daemon/inference.rs:145`).
4. When the model calls `glob`, the hook persists an `AgentToolCall` row and inserts the lifecycle in the in-flight map (`crates/defra-agent/src/hook/persistence.rs:223`, `crates/defra-agent/src/hook/persistence.rs:235`).
5. Stream polling is scoped with the request deadline and cancellation token (`crates/defra-agent/src/agent/daemon/inference.rs:242`).
6. If the wrapper observes the deadline, it returns a managed timeout marker (`crates/defra-agent/src/tool_call_lifecycle/runtime.rs:115`), and the hook maps that marker to `ToolCallLifecycle::timeout()` (`crates/defra-agent/src/hook/persistence.rs:267`).
7. If the outer request deadline fires while waiting for a stream item, the daemon calls `timeout_expired_tool_calls()` (`crates/defra-agent/src/agent/daemon/inference.rs:253`), which drains expired lifecycles and calls `timeout()` (`crates/defra-agent/src/hook.rs:374`).
8. The request then unwinds through `process_request`, records the failure reason, and fails the request (`crates/defra-agent/src/agent/daemon.rs:320`).

That path works for a pending async tool future; the #161 runtime test uses `PendingTool` and proves the wrapper returns a timeout marker (`crates/defra-agent/src/tool_call_lifecycle/runtime.rs:202`).

The actual `glob` path is different:

1. `GlobTool::call` is an `async fn`, but it immediately calls `collect_glob_matches(...)` without an `.await` inside the traversal (`crates/defra-agent/src/toolset/file_tools.rs:306`).
2. `collect_glob_matches_inner` recursively walks directories synchronously (`crates/defra-agent/src/toolset/shared/filesystem.rs:226`).
3. Directory reads use `std::fs::read_dir` (`crates/defra-agent/src/toolset/shared/filesystem.rs:359`).

Once `tokio::select!` polls the `self.inner.call(args)` branch, this native `glob` future can spend the entire poll inside synchronous filesystem code. During that poll, the deadline sleep and cancellation token cannot be observed by the wrapper, and the outer `await_with_request_deadline` around `stream.next()` cannot resume either. That reconstructs the issue #149 shape: `AgentToolCall` stays `status="called"` / `lifecycle_state="running"`, the request remains `processing`, and the single-worker queue stays occupied until the process restarts.

Restart recovery is better after #161: `ToolCallLifecycle::recover_all` finds persisted running tool calls, marks expired rows `timedOut`, and cascades child interrupts where applicable (`crates/defra-agent/src/tool_call_lifecycle/recovery.rs:192`). Startup also recovers stuck processing requests and streaming responses (`crates/defra-agent/src/lifecycle/recovery.rs:15`, `crates/defra-agent/src/lifecycle/recovery.rs:103`). That is still restart convergence, not the live convergence requested by #149.

Recommendation: do not close #149 yet.

## Follow-up issues to file

### 1. Native filesystem tools need a preemptible deadline boundary

`glob` still executes synchronous recursive filesystem traversal inside a single async poll (`file_tools.rs:306`, `filesystem.rs:226`, `filesystem.rs:359`). PR #161's wrapper is cooperative, so it cannot observe the request deadline until that poll returns. Add a hard runtime boundary for native filesystem tools, especially `glob` and `grep`: the request should terminalize the `AgentToolCall` and free the worker at deadline even if the filesystem traversal continues or blocks. Include a regression test that simulates a single-poll blocking native tool, not only a pending async future.

### 2. One-shot tool runs bypass request-scoped deadline enforcement

`run_openai_oneshot_with_tools` builds tools directly and never maps them through `runtime::wrap_tool` (`oneshot.rs:48`), while the hook is created without an active request id or deadline (`oneshot.rs:123`). The hook fallback persists a default deadline and empty request id, but there is no request lifecycle, cancellation token, or live `timeout_expired_tool_calls` sweep. Either install a one-shot request deadline scope or make one-shot tool persistence explicitly non-lifecycle-managed.

### 3. Preserve inherited subagent deadlines through claim

`create_subagent_request_with_request_id` writes an effective child `deadline` from the parent tool deadline and spawn args (`subagent_request.rs:130`), but `RequestLifecycle::claim_inner` later overwrites `AgentRequest.deadline` with `now + behavior.deadline_duration` (`claim.rs:212`). The watcher also does not load the pending row's existing `deadline` (`watcher/query.rs:5`), so a queued child can start after its inherited deadline. Claim should preserve or min with an existing pending deadline, or the subagent source should encode the inherited bound as `valid_until`.

### 4. Wire subagent bridge completion/failure into production recovery

`bridge_complete` and `bridge_failure` exist as lifecycle transitions (`transition.rs:306`, `transition.rs:364`), but no production caller was found. `recover_stuck_running_tool_calls` also skips detached subagent tool rows (`recovery.rs:224`), even when the parent is terminal or the tool deadline expired. Add a child-terminal observer/recovery path that drives parent bridge tool rows to `completed`, `failed`, or `cancelled`, and define how detached parent bridge rows become terminal.

### 5. Add a live rescan path for missed subagent spawn events

`SubagentSource` only reacts to update events and logs dropped messages as "may have missed child spawns" (`subagent_source.rs:378`). Startup orphan recovery can materialize missing child requests after restart (`recovery.rs:88`), but a live daemon has no periodic scan to repair missed `AgentToolCall.child_request_id` rows. Add a bounded live rescan after dropped events or on a slow cadence.

### 6. Terminalize stale persisted `InferenceCall` rows on startup

Admission guards terminalize queued/running calls on normal drop (`controller.rs:282`, `permit.rs:137`), but startup recovery only calls request and tool-call recovery (`startup.rs:334`, `startup.rs:368`). A crash can leave `InferenceCall.call_state="queued"` or `"running"` indefinitely, diverging from the persisted slot-accounting model and polluting telemetry. Add startup recovery keyed by old `runtime_instance_id`, terminal parent request, or request deadline.

### 7. Make trigger runtime-field writebacks convergent

Schedule and event trigger result callbacks spawn best-effort DefraDB updates and only log failures (`schedule_source.rs:236`, `event_source.rs:581`). A failed schedule fired/skipped update can leave `next_run_at` unchanged and refire the same due schedule; event runtime fields can miss `last_status`/`fire_count`. Add retry, idempotent writeback reconciliation, or a periodic repair pass for trigger runtime-owned fields.

### 8. Add live resync for dropped event-trigger messages

`EventSource` treats dropped subscription messages as a correctness hazard and only warns (`event_source.rs:844`). Because `seen_docs` is process-local first-observation state, a dropped create event can be missed forever in the live process. Add a resync strategy after dropped messages, or persist enough event-source cursor/seen state to make "created" triggers convergent.

### 9. Expose active request/tool liveness counters

Issue #149 asked for `/healthz` or `/status` counters for active request id, active tool name, age since last progress, and expired-processing count. The current exposed hook stats are persistence counters only (`hook.rs:26`), and recovery counts are logged only at startup (`startup.rs:334`). Add live runtime status fields or health counters so expired processing/tool-call stalls are visible before an operator notices queue starvation.
