# Deadline Audit Execution Plan

## 1. Priority + dependency matrix

| Item | Priority | Hard dependencies | Blocking relationship to R4 |
| --- | --- | --- | --- |
| Issue #149 | P0 | Audit follow-up #1 is required for live correctness; follow-up #9 is required if we keep the original health/status acceptance criteria. Current timeout rows write `lifecycle_state: "timedOut"` but not `tool_failure_class` (`crates/defra-agent/src/tool_call_lifecycle/transition.rs:471`, `crates/defra-agent/src/tool_call_lifecycle/recovery.rs:479`), so the #149 closure PR must either add the class or explicitly accept `timedOut` as equivalent. | Blocks any R4 soak that uses native filesystem tools because request deadline observation is still cooperative in `RuntimeManagedTool::call` (`crates/defra-agent/src/tool_call_lifecycle/runtime.rs:115`) and a blocked request remains the active daemon work item until its future returns. |
| Follow-up #1: native filesystem preemptible boundary | P0 | None. It should not depend on R4 or new R4 primitives; the existing daemon tool wrapping point is already present (`crates/defra-agent/src/agent/runtime/context.rs:84`). | Blocks R4 validation runs that depend on deadline guarantees. R4 must not assume native file tools are deadline-preemptible until this lands. |
| Follow-up #2: one-shot deadline enforcement | P1 | None hard. It can reuse existing `wrap_tool` / `scope_request_tool_execution` (`crates/defra-agent/src/tool_call_lifecycle/runtime.rs:38`, `crates/defra-agent/src/tool_call_lifecycle/runtime.rs:57`). | Not a design blocker for R4 unless R4 introduces one-shot subagent test harnesses through `run_openai_oneshot_with_tools` (`crates/defra-agent/src/oneshot.rs:33`). |
| Follow-up #3: preserve inherited subagent deadlines through claim | P0 | None hard. Current child creation already accepts a deadline (`crates/defra-agent/src/tool_call_lifecycle/subagent_request.rs:88`); claim is the overwrite point (`crates/defra-agent/src/lifecycle/claim.rs:212`). | Blocks R4 if R4 needs inherited child deadlines or detach semantics to be truthful. R4 must not pretend the pending child `deadline` survives claim today. |
| Follow-up #4: wire bridge completion/failure in production | P0 | Follow-up #3 is not a strict code dependency, but should land first if R4 wants bridge terminal behavior and child deadline behavior in the same release train. The transition methods already exist (`crates/defra-agent/src/tool_call_lifecycle/transition.rs:306`, `crates/defra-agent/src/tool_call_lifecycle/transition.rs:364`). | Direct R4 blocker. R4 cannot assume parent bridge rows terminalize when children finish unless it wires this itself or waits for this PR. |
| Follow-up #5: live rescan for missed subagent spawn events | P1 | None hard. It should reuse the running child-linked row query shape from recovery (`crates/defra-agent/src/tool_call_lifecycle/recovery.rs:88`) rather than inventing a second parser. | R4-adjacent. It is not a design prerequisite, but R4 should document that live missed-spawn recovery is restart-only until this lands. |
| Follow-up #6: startup sweep for stale `InferenceCall` rows | P1 | None hard. Startup recovery currently invokes request and tool-call recovery only (`crates/defra-agent/src/agent/runtime/startup.rs:334`, `crates/defra-agent/src/agent/runtime/startup.rs:368`). | Not an R4 design blocker, but it affects post-crash scheduler capacity telemetry if R4 stress tests restart mid-run. |
| Follow-up #7: trigger runtime-field writeback convergence | P1 | None hard. Schedule and event writebacks are independent code paths (`crates/defra-agent/src/trigger_engine/schedule_source.rs:236`, `crates/defra-agent/src/trigger_engine/event_source.rs:585`). | Not an R4 blocker unless R4 depends on scheduled/event-trigger subagent orchestration. |
| Follow-up #8: event-trigger dropped-message resync | P1 | None hard, but it should reuse any durable writeback/error-reporting helpers introduced by follow-up #7 if they land first. Current dropped-event behavior is warn-only (`crates/defra-agent/src/trigger_engine/event_source.rs:844`). | Not an R4 blocker for bridge design. It is a production reliability gap for event-triggered R4 scenarios. |
| Follow-up #9: active request/tool liveness counters | P1 | None hard for observability; if used as #149 close evidence, it should land after follow-up #1 so counters report the final active-tool model. Existing hook stats are persistence-only (`crates/defra-agent/src/hook.rs:26`). | Does not block R4 design. It should be available before R4 soak sign-off so stalls are visible without DB spelunking. |

Planning correction to the audit: no finding is reversed. The plan adds one #149 nuance: the audit treated `lifecycle_state="timedOut"` as the timeout classification, but #149 also asked for `tool_failure_class` or equivalent. Current timeout transitions do not populate `tool_failure_class` (`crates/defra-agent/src/tool_call_lifecycle/transition.rs:471`, `crates/defra-agent/src/tool_call_lifecycle/recovery.rs:479`), so the #149 PR should make that equivalence explicit in code or issue-closing notes.

## 2. Sequencing

| PR | Scope | Independently mergeable? | One-sentence title | Smallest first commit |
| --- | --- | --- | --- | --- |
| PR A | Issue #149 + follow-up #1 | Yes | Make native filesystem tool deadlines preemptible and close the live `glob` stall. | Add a test-only single-poll blocking native tool harness that proves the existing cooperative wrapper cannot meet a short request deadline without a preemptible boundary. |
| PR B | Follow-up #3 | Yes | Preserve inherited subagent request deadlines through claim. | Add a claim-path regression that creates a pending subagent request with an earlier deadline and proves claim does not extend it past the inherited bound. |
| PR C | Follow-up #4 | Yes, but should read R4 design before implementation | Bridge child terminal states back to parent subagent tool rows. | Add the production child-terminal scanner/observer skeleton and a DB fixture showing a completed child maps to `bridge_complete`. |
| PR D | Follow-up #5 | Yes | Add live orphan-subagent spawn rescan after missed tool-call events. | Extract the recovery query/parsing path for running child-linked tool rows into a reusable scanner used first by startup recovery. |
| PR E | Follow-up #6 | Yes | Recover stale admission calls on startup. | Add an `InferenceCall` startup-recovery query for old `queued`/`running` rows without mutating request/tool logic. |
| PR F | Follow-up #2 | Yes | Enforce request-scoped deadlines for one-shot tool runs. | Wrap one-shot tools with `runtime::wrap_tool` and add a one-shot short-deadline regression before changing public behavior. |
| PR G | Follow-up #7 | Yes | Make trigger runtime writebacks retryable and idempotent. | Introduce a small writeback worker abstraction for `Schedule` runtime fields and move the current fire-and-forget schedule update onto it. |
| PR H | Follow-up #8 | Yes, but can reuse PR G helpers | Resync event-trigger sources after dropped subscription messages. | Add a deterministic resync entry point that can rebuild `seen_docs`/missed candidates without depending on live subscription delivery. |
| PR I | Follow-up #9 | Yes | Expose active request/tool liveness counters in runtime status. | Add an internal `RuntimeLivenessSnapshot` query/model with active request id, active tool id/name, active ages, and expired-processing count. |

Recommended merge order:

1. PR A first. It is the only issue #149 correctness fix and the only one that can prove the single-worker queue no longer stalls behind native `glob`.
2. PR B before PR C if R4 wants inherited child deadlines in the same milestone. PR C can still be developed independently, but its R4 semantics should cite whether PR B is already merged.
3. PR C next for R4. It is the largest R4-coupled production path because the Lean bridge transitions already exist, but production currently only defines the methods.
4. PR D can land before or after PR C. It improves missed spawn convergence but does not define bridge terminal semantics.
5. PR E, PR F, PR G, PR H, and PR I are independently mergeable after the P0 path is moving. PR I should land before any "soak clean" claim for R4, but it does not need to block PR A's code review.

## 3. R4 coupling

### Follow-up #3: inherited subagent deadlines

R4's design needs to assume the current runtime computes an effective child deadline before materialization (`crates/defra-agent/src/trigger_engine/subagent_source.rs:282`) and writes it at subagent request creation (`crates/defra-agent/src/tool_call_lifecycle/subagent_request.rs:130`), but claim overwrites `AgentRequest.deadline` with `now + behavior.deadline_duration` (`crates/defra-agent/src/lifecycle/claim.rs:212`). The watcher request projection also omits `deadline`, so claim has no loaded pending-deadline value to preserve (`crates/defra-agent/src/watcher/query.rs:5`).

R4 should not pretend a detached child is bounded by the parent/tool deadline today. If R4's model depends on that, R4 must either block on PR B or explicitly include deadline preservation in the R4 implementation scope.

Recommendation: block R4's "deadline semantics complete" claim on PR B, but R4 design can continue with the current gap called out.

### Follow-up #4: bridge completion/failure wiring

R4's design needs to assume only the transition methods exist today: `bridge_complete` trusts its caller to verify child completion (`crates/defra-agent/src/tool_call_lifecycle/transition.rs:306`), and `bridge_failure` projects failed/dead/interrupted/superseded child states (`crates/defra-agent/src/tool_call_lifecycle/transition.rs:364`). The production source currently materializes child requests from `AgentToolCall.child_request_id` (`crates/defra-agent/src/trigger_engine/subagent_source.rs:284`), but the audit found no production caller that observes child terminal state and invokes those bridge transitions.

R4 should not pretend parent bridge tool rows become terminal when child requests finish. It must wire that observer itself or list bridge terminalization as an explicit gap.

Recommendation: R4 should block on PR C if it wants production bridge semantics; otherwise it can layer on top only if its design names the unwired bridge as a carried risk.

### R4-adjacent: missed subagent spawn events

Follow-up #5 is not a core R4 design primitive, but R4 should not assume missed `AgentToolCall` update events are live-repaired. Current `SubagentSource` warns on dropped messages (`crates/defra-agent/src/trigger_engine/subagent_source.rs:378`), while the repair path is startup-only (`crates/defra-agent/src/tool_call_lifecycle/recovery.rs:88`).

Recommendation: do not block R4 design on PR D, but block production-readiness language for event-loss convergence until PR D lands.

## 4. Lean obligations

| Item | Obligation type | File/module | Blocks Rust fix? |
| --- | --- | --- | --- |
| Issue #149 / follow-up #1 | New conformance boundary for an existing property. Lean already says live calls can reach terminal and deadline-exceeded linked tools time out; the missing part is Rust's runtime assumption that tool execution is preemptible (`crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean:94`, `crates/defra-agent/proofs/Proofs/Composed.lean:272`). | Add generated contract/conformance coverage under `Proofs/Conformance/Boundaries.lean` or `Proofs/Conformance/ContractCases/BoundaryRuntime.lean`. | Rust P0 fix can land first with a Rust regression; Lean conformance should be in the same PR if small, otherwise queued immediately. |
| Follow-up #2 | Conformance test over existing tool lifecycle behavior if one-shot remains lifecycle-managed. If one-shot is declared outside the lifecycle model, record that deviation. | `Proofs/Conformance/Deviations.lean` for an explicit exclusion, or boundary contract cases if wrapped. | Does not block Rust; Rust should define whether one-shot participates in lifecycle first. |
| Follow-up #3 | New property: child claim must not extend an inherited subagent deadline, or inherited deadline must be encoded as TTL. | `Proofs/Subagent/Properties.lean` for bridge/deadline invariant plus request claim case in `Proofs/Request/Properties.lean`. | Rust can land first with DB tests if it preserves/mins existing deadline; Lean should follow before R4 claims proof coverage. |
| Follow-up #4 | Conformance test for existing Lean bridge transitions, not a new transition. Lean already models `bridge_complete` and `bridge_failure` (`crates/defra-agent/proofs/Proofs/Subagent/Transition.lean:76`, `crates/defra-agent/proofs/Proofs/Subagent/Transition.lean:105`). | Add contract cases under a new `Proofs/Conformance/Subagent` module or extend `Proofs/Conformance/ContractCases/LifecycleTransitions.lean`. | Blocks R4 proof/conformance claim, but the Rust observer can land with Rust tests first if the Lean case is queued in the same milestone. |
| Follow-up #5 | Conformance test for startup/live materialization convergence; new property only if the model wants live missed-event repair. | `Proofs/Conformance/Boundaries.lean` for DefraDB event-delivery boundary; `Proofs/Subagent/Properties.lean` only if live rescan becomes modeled. | Rust can land first; Lean should document the event-delivery boundary. |
| Follow-up #6 | New property or conformance case: startup recovery terminalizes stale persisted `InferenceCall` rows so reconstructed slot counts do not include dead-runtime rows. Existing slot model says running rows hold slots (`crates/defra-agent/proofs/Proofs/InferenceCall/SlotAccounting.lean:97`). | `Proofs/InferenceCall/SlotAccounting.lean` and generated cases in `Proofs/Conformance/ContractCases/SlotAccounting.lean`. | Rust can land first with recovery tests; Lean should be same PR if the sweep affects slot-accounting invariants. |
| Follow-up #7 | New conformance/liveness case if writeback retry semantics become part of runtime contract; otherwise Rust operational tests only. | `Proofs/Conformance/Triggers/Trace.lean` or `Proofs/Triggers` if modeled. | Does not block Rust. |
| Follow-up #8 | No new Lean property unless we decide to model DefraDB event delivery. The existing README treats subscription delivery/control-source behavior as operational Rust coverage (`crates/defra-agent/proofs/README.md:514`). | Rust tests plus optional note in `Proofs/Conformance/Boundaries.lean`. | Does not block Rust. |
| Follow-up #9 | No Lean property. This is observability over runtime state, not a state-machine transition. | none | Does not block Rust. |

## 5. Verification strategy per follow-up

| Item | Concrete test/repro | CI stability plan | Soak needed? |
| --- | --- | --- | --- |
| Issue #149 | End-to-end daemon regression: model invokes a native `glob`-class tool that blocks longer than a short request deadline; assert request terminalizes and a second queued request advances without restart. | Use a finite single-poll blocker, e.g. `std::thread::sleep(200ms)` with a 10-20ms deadline, not an infinite hang. Gate wall-clock assertions with generous bounds. | Yes. Re-run the original 70-case stress shape before closing #149. |
| Follow-up #1 | Unit test the native execution boundary with a single-poll blocking tool; integration test that `AgentToolCall` ends as `timedOut` and has `completed_at`/`latency_ms`. | Use deterministic short sleeps and `tokio::time::timeout` around the test itself. Avoid OS/filesystem pathological hangs in CI. | Yes, same soak as #149 because the production failure involved native filesystem traversal. |
| Follow-up #2 | One-shot regression using an extra test tool that never yields or blocks longer than a one-shot deadline; assert `run_openai_oneshot_with_tools` returns and persisted lifecycle is terminal when persistence is enabled. | Prefer a fake completion model/tool harness over networked OpenAI-compatible clients. Keep timeout under one second. | No. Unit/integration coverage is enough. |
| Follow-up #3 | DB test: create subagent pending request with inherited deadline earlier than behavior duration, then claim; assert deadline is preserved/minned or stale request is rejected if deadline is past. | Use fixed RFC3339 values and an isolated EmbeddedNode fixture. No sleeps except unavoidable DB polling. | No. |
| Follow-up #4 | DB integration tests for child `completed`, `failed`, `dead`, `interrupted`, and `superseded`: parent bridge row must transition via `bridge_complete` or `bridge_failure`; cascade interrupt must still call `interrupt_request` for cascade policy (`crates/defra-agent/src/hook.rs:396`). | Use direct fixture rows and call the observer/sweep once. Do not depend on real model/tool streaming. | R4 soak should cover this after unit tests, because ordering with live child requests is the risk. |
| Follow-up #5 | Start `SubagentSource` after inserting a running child-linked tool row or simulate dropped event, then run live rescan; assert child request materializes once. | Test the scanner directly and one source-level path. Avoid relying on actual subscription buffer overflow in CI. | No dedicated soak, but include in R4 subagent stress. |
| Follow-up #6 | Seed `InferenceCall` rows in `queued` and `running` for an old runtime or terminal parent request; run startup recovery; assert terminal states and reconstructed slot count zero. | Pure DB fixture plus recovery function. No provider/backend network calls. | No. |
| Follow-up #7 | Fake first writeback failure and second success for schedule/event runtime fields; assert `next_run_at` advances once and fire count is idempotent. | Use an injectable writeback helper/fake or paused-time retry loop. Avoid sleeping real retry intervals. | No, unless the implementation uses background retry tasks that need fleet-level confidence. |
| Follow-up #8 | Unit/integration test resync: seed source docs and trigger config, mark subscription drop, run resync, assert missed created doc produces one fire and already-seen docs do not duplicate. | Drive resync as a direct method; do not try to force actual mpsc dropped-message behavior. | Maybe. Event-trigger soak is useful after implementation because lost-event bugs are ordering-sensitive. |
| Follow-up #9 | Seed one active request past deadline and one running tool; query status/liveness endpoint/model; assert active ids, ages, active tool name, and expired-processing count. | Avoid wall-clock exactness; assert nonzero/greater-than thresholds with fixed inserted timestamps. | No, but use it during #149/R4 soak as operator-facing evidence. |

## 6. Out-of-scope and deferred items

Do soon:

- Follow-up #9 is not the live correctness fix, but it is worth doing soon because #149's operational failure was silent while health stayed OK. It should not block PR A implementation, but it should block declaring the soak operationally observable.
- The `tool_failure_class` nuance for timeout rows should be resolved in PR A or in the #149 close comment. Current timeout code sets `lifecycle_state="timedOut"` and omits `tool_failure_class` (`crates/defra-agent/src/tool_call_lifecycle/transition.rs:471`, `crates/defra-agent/src/tool_call_lifecycle/recovery.rs:479`).
- Follow-up #6 should land before any restart-heavy stress campaign, because persisted `InferenceCall` rows are the scheduler slot-accounting source (`crates/defra-agent/src/admission/slot_accounting.rs:16`).

Defer behind P0 correctness:

- Follow-up #7 writeback convergence and follow-up #8 event resync are silent operational bugs, but neither should delay PR A, PR B, or PR C.
- Follow-up #2 is a real bug for public one-shot APIs, but it does not block R4 unless R4 starts using one-shot as a subagent execution harness.
- Deep health/status UI work beyond the counters in follow-up #9 is out of scope. The first observability PR should expose facts already derivable from `AgentRequest` and `AgentToolCall` (`crates/defra-agent-protocol/schemas/agent/agent_request.graphql:24`, `crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql:12`); dashboards can follow later.

Do not include in this plan's implementation scope:

- No production code changes in the audit/plan PR.
- No issue filing until the plan is reviewed.
- No assumptions about R4-only primitives. Every PR above is expressed in terms of code that already exists in this worktree.
