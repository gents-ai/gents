# Request Lifecycle Single Owner Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `AgentRequest` lifecycle state becomes one fact with one typed owner: the `status` column is deleted, `lifecycle_state` is the only persisted state, `gents_protocol::request_lifecycle::RequestLifecycleState` is the only Rust type that parses or classifies it, and every hand-rolled string predicate is deleted.

**Architecture:** The Lean `RequestState` model gains a `workspaceBindingPending` pre-claim state (today encoded only in `status`), with one transition `bindWorkspace : workspaceBindingPending → pending`. The protocol crate's existing `RequestLifecycleState` enum becomes the single Rust owner (parse, `as_str`, `is_terminal`, GraphQL list helpers); the runtime's private `PersistedLifecycleState` and the Lean conformance `GentsLifecycleState` are deleted. The `status` column is removed from the SDL as a clean cutover (no migration). Every writer stops writing it; every reader filters and classifies on `lifecycle_state` through the typed owner.

**Tech Stack:** Lean 4 / Mathlib proofs (`lake build`), Rust workspace (`cargo`), DefraDB embedded node, ts-rs generated TypeScript views.

**Spec:** GitHub issue #1330 (milestone "Single owner"). Reference examples of the pattern: #1326 (compaction budgets) and `docs/superpowers/specs/2026-08-17-mobile-session-sync-design.md`.

## Global Constraints

- Clean cutover: no `gents-migration` step, no serde default, no compat alias that accepts the old `status` column or the old vocabulary (`complete`, `error`, `streaming`, `workspace_binding_pending`) on an `AgentRequest`. `AgentResponse.status` (`streaming`/`complete`/`error`) is a different document and stays.
- Zero `sorry` in `crates/gents/proofs`. `cd crates/gents/proofs && lake build` must succeed.
- Never write `[]` in a DefraDB mutation; emit `null`. Always `graphql::escape_graphql_string()` for interpolated strings.
- Gate with `cargo test -p gents` (full package), then `cargo check --workspace --all-targets`.
- `tracing`, never `println`.
- The only Rust type that names request lifecycle strings is `gents_protocol::request_lifecycle::RequestLifecycleState`. A grep for `"inputRequired"` outside that module and its tests must return nothing in non-test code.
- Net code deletion is the success criterion. Do not add abstraction beyond what this plan names.

## Vocabulary after this change

`lifecycle_state` values (Lean `RequestState.toDefraDB`):

| state | terminal | claimable | notes |
|---|---|---|---|
| `workspaceBindingPending` | no | no | created bound to a workspace; awaits binding materialization |
| `pending` | no | yes | |
| `claimed` | no | no | |
| `processing` | no | no | |
| `inputRequired` | no | no | reserved, product-unreachable |
| `completed` | yes | | |
| `failed` | yes | | replaces `status="error"` |
| `superseded` | yes | | |
| `dead` | yes | | |
| `interrupted` | yes | | |

Old `status` → new `lifecycle_state` filter equivalents used throughout the sweep tasks:

| old filter | replacement |
|---|---|
| `status: { _eq: "pending" }` | `lifecycle_state: { _eq: "pending" }` |
| `status: { _eq: "workspace_binding_pending" }` | `lifecycle_state: { _eq: "workspaceBindingPending" }` |
| `status: { _eq: "processing" }` | `lifecycle_state: { _in: ["claimed", "processing"] }` (or the narrower one the caller already ANDs with) |
| `status: { _eq: "error" }` | `lifecycle_state: { _eq: "failed" }` |
| `status: { _eq: "completed" }` | `lifecycle_state: { _eq: "completed" }` |
| `status: { _nin: [terminal list] }` | `lifecycle_state: { _in: <RequestLifecycleState::active_graphql_list()> }` |

---

### Task 1: Lean model — add `workspaceBindingPending`, delete `GentsLifecycleState`

**Files:**
- Modify: `crates/gents/proofs/Proofs/Request/State.lean`
- Modify: `crates/gents/proofs/Proofs/Request/Transition.lean`
- Modify: `crates/gents/proofs/Proofs/Request/Executable.lean`
- Modify: `crates/gents/proofs/Proofs/Properties/Decidable.lean`
- Modify: `crates/gents/proofs/Proofs/Properties/Liveness.lean`
- Modify: `crates/gents/proofs/Proofs/Properties/Safety.lean` (only if `lake build` demands a case)
- Modify: `crates/gents/proofs/Proofs/Conformance/Gents.lean` (delete `GentsLifecycleState`)
- Modify: `crates/gents/proofs/Proofs/Conformance/Contracts/Machines/Request.lean`
- Modify: `crates/gents/proofs/Proofs/Conformance/ContractCases/LifecycleTransitions.lean`
- Modify: `crates/gents/proofs/Proofs/Conformance/ContractCases/LiveOverlay.lean`
- Modify: `crates/gents/proofs/Proofs/Conformance/Triggers/Lifecycle.lean`
- Modify: `crates/gents/proofs/Proofs/Client/Types.lean`, `Client/Lifecycle.lean`, `Client/Terminal.lean`, `ClientShell/Projection.lean`, `CodexShim/LocalInterrupt.lean`, `Conformance/Contracts/Json/CodexShim.lean` (add the new state wherever an exhaustive match or a nonterminal disjunction enumerates request states)
- Modify: `crates/gents/proofs/README.md` (Layer 2 states + delete the `status` bridging paragraph)

**Interfaces:**
- Produces: `RequestState.workspaceBindingPending` with `toDefraDB = "workspaceBindingPending"`; `RequestContext.Action.bindWorkspace`; `RequestContext.Transition.bind_workspace`. The emitted contract (Contracts.lean JSON) gains state `workspaceBindingPending` and action `bindWorkspace`; the pair `workspaceBindingPending -> pending` classifies `legal`.

- [ ] **Step 1: Add the state to `RequestState`**

In `Proofs/Request/State.lean`, insert `| workspaceBindingPending` as the first constructor, and add it to `toDefraDB` and `fromDefraDB?`:

```lean
inductive RequestState where
  | workspaceBindingPending
  | pending
  ...

def toDefraDB : RequestState → String
  | .workspaceBindingPending => "workspaceBindingPending"
  | .pending => "pending"
  ...

def fromDefraDB? : String → Option RequestState
  | "workspaceBindingPending" => some .workspaceBindingPending
  | "pending" => some .pending
  ...
```

In the `HasTerminal RequestState` instance, add an arm for `.workspaceBindingPending` that is a verbatim copy of the `.pending` arm (an `isFalse` proof). In `RequestContext.coherentStateAdmission` add `| .workspaceBindingPending, a => a = .released`.

- [ ] **Step 2: Add the transition and action**

`Proofs/Request/Transition.lean`, first constructor:

```lean
  | bind_workspace {pre post : RequestContext} :
      pre.state = .workspaceBindingPending →
      pre.admission = .released →
      post = { pre with state := .pending } →
      Transition pre post
```

`Proofs/Request/Executable.lean`: add `| bindWorkspace` as the first `Action` constructor and

```lean
  | .bindWorkspace =>
      if pre.state = .workspaceBindingPending ∧ pre.admission = .released then
        some { pre with state := .pending }
      else
        none
```

Extend `step_sound` (and `step_complete` / any `cases action` proof in that file) with a `bindWorkspace` arm modeled on the `dedupLose` arm:

```lean
  | bindWorkspace =>
      simp [step?] at h_step
      rcases h_step with ⟨⟨h_state, h_admission⟩, h_post⟩
      exact Transition.bind_workspace h_state h_admission h_post.symm
```

- [ ] **Step 3: Extend enumerations and measures**

- `Properties/Decidable.lean`: add `.workspaceBindingPending` to the `Fintype RequestState` list; add `| workspaceBindingPending => exact ⟨.pending, by decide⟩` to `active_request_no_deadlocks` if `activeCoreRequestState` is extended, otherwise the `| _ => False` arm covers it and `cases h` closes it.
- `Properties/Liveness.lean`: the measure function gets `| .workspaceBindingPending => r.maxRetries + 5` (strictly above `pending`'s `+ 4` so `bind_workspace` decreases the measure). If `phase_change_decreases_measure` case-splits on the transition, add a `bind_workspace` arm proving `r.maxRetries + 4 < r.maxRetries + 5` by `omega`.
- `Conformance/Contracts/Machines/Request.lean` and `ContractCases/LifecycleTransitions.lean`: add `.workspaceBindingPending` to `requestStates` / `requestTransitionStates`, `("bindWorkspace", .bindWorkspace)` to both action lists, and `requestContext .workspaceBindingPending .released` / `requestTransitionContext .workspaceBindingPending .released` to both sample lists.
- `Conformance/Triggers/Lifecycle.lean`: `| .workspaceBindingPending => false`.
- `ContractCases/LiveOverlay.lean`: add `requestProgressCase "workspace_binding_pending_is_queued" .workspaceBindingPending`.
- `ClientShell/Projection.lean`: `| .workspaceBindingPending => .queued`.
- `Client/Types.lean`: include `.workspaceBindingPending` in the nonterminal arm alongside `.pending | .claimed | .processing | .inputRequired`. `Client/Lifecycle.lean` and `Client/Terminal.lean`: extend the nonterminal disjunction hypotheses and their `cases` proofs with the new constructor (one more `Or.inl` / `Or.inr` layer, mirroring the existing pattern).
- `CodexShim/LocalInterrupt.lean`: covered by `| _ => False`.
- `Conformance/Contracts/Json/CodexShim.lean`: add `codexShimSubagentStatusCase "codex_shim.subagent_status.workspace_binding_pending" .workspaceBindingPending none` next to the pending case, and if a thread-status list enumerates all request states add the analogous case with the same expectation as `.pending`.
- `Conformance/Gents.lean`: delete the whole `GentsLifecycleState` inductive, namespace and theorem (nothing else references it).

- [ ] **Step 4: Build**

Run: `cd crates/gents/proofs && lake build 2>&1 | tail -40`
Expected: build succeeds, no `sorry`, no errors. Fix every "missing cases" error by adding the new arm with the same shape as the `pending` arm.

- [ ] **Step 5: Update `crates/gents/proofs/README.md`**

Layer 2 states: add `workspaceBindingPending` first with the operational meaning "created bound to a workspace and not yet claimable; `bindWorkspace` moves it to `pending` once the WorkspaceBinding document is materialized". Delete the paragraph beginning "Lean `AdmissionState` is not persisted as its own `AgentRequest` column" and replace with: "`AgentRequest.lifecycle_state` is the only persisted request state column; `RequestState.toDefraDB` is its vocabulary. Lean `AdmissionState` is bridged through runtime-owned `InferenceCall` rows, not a request column."

- [ ] **Step 6: Commit**

```bash
git add crates/gents/proofs
git commit -m "spec(request): model workspaceBindingPending; delete GentsLifecycleState"
```

---

### Task 2: Protocol crate — `RequestLifecycleState` becomes the single owner

**Files:**
- Create: `crates/gents-protocol/src/request_lifecycle.rs`
- Modify: `crates/gents-protocol/src/client_protocol.rs` (remove the enum + `TryFrom` + `InvalidRequestLifecycleState`; `pub use crate::request_lifecycle::{RequestLifecycleState, InvalidRequestLifecycleState};` to keep the path working)
- Modify: `crates/gents-protocol/src/lib.rs` (`pub mod request_lifecycle;`)
- Modify: `crates/gents-protocol/src/row.rs` (`AgentRequestRow`: delete `status`)
- Modify: `crates/gents-protocol/src/graphql.rs:735-775` (drop `status` from the two AgentRequest selection sets)
- Modify: `crates/gents-protocol/src/request_admission.rs:739,795,854,1028` (`initial_status: String` → `initial_lifecycle_state: RequestLifecycleState`)
- Test: `crates/gents-protocol/src/request_lifecycle.rs` (unit tests inline)

**Interfaces:**
- Produces:

```rust
// gents_protocol::request_lifecycle
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestLifecycleState {
    WorkspaceBindingPending, Pending, Claimed, Processing, InputRequired,
    Completed, Failed, Superseded, Dead, Interrupted,
}
impl RequestLifecycleState {
    pub const ALL: [Self; 10];
    pub const fn as_str(self) -> &'static str;
    pub const fn is_terminal(self) -> bool;          // Completed|Failed|Superseded|Dead|Interrupted
    pub const fn is_claimable(self) -> bool;         // Pending only
    pub const fn is_active_runtime(self) -> bool;    // Pending|Claimed|Processing (matches today's active_runtime list)
    pub fn parse(value: &str) -> Result<Self, InvalidRequestLifecycleState>;
    pub fn parse_opt(value: Option<&str>) -> Option<Self>;  // None on missing or invalid
    pub fn is_terminal_str(value: Option<&str>) -> bool;    // parse_opt(..).is_some_and(is_terminal)
    pub fn graphql_list(states: impl IntoIterator<Item = Self>) -> String;  // `["a", "b"]`
    pub fn terminal_graphql_list() -> String;
    pub fn active_runtime_graphql_list() -> String;
    pub fn nonterminal_graphql_list() -> String;
}
impl TryFrom<&str> for RequestLifecycleState;  // delegates to parse
impl std::fmt::Display for RequestLifecycleState;  // as_str
```

- `AgentRequestCreate.initial_lifecycle_state: RequestLifecycleState`, default `Pending`; `graphql_input_fields` rejects anything other than `Pending | WorkspaceBindingPending` with the existing error text and emits `lifecycle_state: "<as_str>"` (the existing `lifecycle_state` line stays; the `status` line is deleted).

- [ ] **Step 1: Write the failing tests**

In the new module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_round_trips_for_every_state() {
        for state in RequestLifecycleState::ALL {
            assert_eq!(RequestLifecycleState::parse(state.as_str()), Ok(state));
        }
    }

    #[test]
    fn terminal_partition_matches_lean() {
        use RequestLifecycleState::*;
        let terminal: Vec<_> = ALL.iter().copied().filter(|s| s.is_terminal()).collect();
        assert_eq!(terminal, vec![Completed, Failed, Superseded, Dead, Interrupted]);
        assert!(!WorkspaceBindingPending.is_terminal());
        assert!(!WorkspaceBindingPending.is_claimable());
        assert!(Pending.is_claimable());
    }

    #[test]
    fn legacy_status_vocabulary_is_rejected() {
        for legacy in ["complete", "error", "streaming", "workspace_binding_pending", "timedOut", "cancelled"] {
            assert!(RequestLifecycleState::parse(legacy).is_err(), "{legacy}");
        }
        assert!(!RequestLifecycleState::is_terminal_str(Some("error")));
        assert!(RequestLifecycleState::is_terminal_str(Some("failed")));
        assert!(!RequestLifecycleState::is_terminal_str(None));
    }

    #[test]
    fn graphql_lists_are_quoted_arrays() {
        assert_eq!(
            RequestLifecycleState::active_runtime_graphql_list(),
            r#"["pending", "claimed", "processing"]"#
        );
        assert_eq!(
            RequestLifecycleState::terminal_graphql_list(),
            r#"["completed", "failed", "superseded", "dead", "interrupted"]"#
        );
    }
}
```

Also add to `request_admission.rs` tests: `graphql_input_fields` on a create with `initial_lifecycle_state: Claimed` returns `Err("new AgentRequest must begin in a pre-claim pending state")`, and the emitted fields contain `lifecycle_state: "workspaceBindingPending"` and no `status:` when set to `WorkspaceBindingPending`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p gents-protocol request_lifecycle 2>&1 | tail -20`
Expected: compile error (module missing).

- [ ] **Step 3: Implement the module and rewire**

Write `request_lifecycle.rs` with the interface above (move the enum, `TryFrom`, and `InvalidRequestLifecycleState` out of `client_protocol.rs`; `client_protocol.rs` re-exports them). `graphql_list` joins `"\"{}\""` with `", "` inside `[...]`. In `row.rs` delete the `status` field of `AgentRequestRow` only (other rows keep theirs). In `graphql.rs` delete `status` from the AgentRequest selection sets at the two sites. In `request_admission.rs` replace `initial_status` per the interface; the `pub fn base(...)` constructor sets `initial_lifecycle_state: RequestLifecycleState::Pending`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p gents-protocol 2>&1 | tail -20`
Expected: PASS. Then `cargo check -p gents-protocol`.

- [ ] **Step 5: Commit**

```bash
git add crates/gents-protocol
git commit -m "protocol: RequestLifecycleState owns request state; drop AgentRequest.status"
```

---

### Task 3: Schema and runtime lifecycle core

**Files:**
- Modify: `crates/gents-schemas/schemas/agent/agent_request.graphql` (delete `status: String @index`)
- Modify: `crates/gents/src/lifecycle.rs` (delete `PersistedLifecycleState`, `lifecycle_state_graphql_list*`, `active_runtime_lifecycle_state_graphql_list`, `stuck_request_lifecycle_state_graphql_list`, `terminal_lifecycle_state_graphql_list`, `nonterminal_lifecycle_state_graphql_list`, `lifecycle_state_graphql_list_for`; and the `update_AgentRequest` at `:845`)
- Modify: `crates/gents/src/lifecycle/rows.rs` (`DedupRow`, `StatusRow`, `RequestViewRow` lose `status`; `is_pending` checks `lifecycle_state == Some("pending")` via the typed owner)
- Modify: `crates/gents/src/lifecycle/transition.rs` (delete `request_status_is_terminal`; `request_view_is_terminal` = `RequestLifecycleState::is_terminal_str(view.lifecycle_state.as_deref())`; `transition_request_status` / `transition_execution_view` lose the `from_status`/`target_status` string parameters; all `update_AgentRequest` filters and inputs drop `status`)
- Modify: `crates/gents/src/lifecycle/claim.rs`, `lifecycle/recovery.rs`, `lifecycle/query.rs`, `lifecycle/manual.rs`, `lifecycle/materialize.rs`, `lifecycle/queue/{atomic_inputs,coalescing,draining}.rs`, `lifecycle/background_wake_recovery.rs`
- Modify: `crates/gents/src/watcher/query.rs` and `watcher/query/rows.rs` (filter `lifecycle_state: { _eq: "pending" }`; row struct drops `status`)
- Modify: `crates/gents/src/request_admission.rs:214-236`, `crates/gents/src/interrupt.rs:148-180`, `crates/gents/src/streaming.rs:~918`, `crates/gents/src/trigger_engine/production_materializer.rs:107-160`, `crates/gents/src/session/fork.rs`
- Modify: `crates/gents/src/goal.rs:1233` (`initial_status: _` → `initial_lifecycle_state: _`)
- Test: `crates/gents/src/lifecycle/tests.rs` (or wherever `persisted_lifecycle_terminal_partition_matches_lean_contract` lives at `lifecycle.rs:639`; keep that test but point it at `RequestLifecycleState`)

**Interfaces:**
- Consumes: `gents_protocol::request_lifecycle::RequestLifecycleState` (Task 2).
- Produces: `crate::lifecycle::activate_workspace_bound_request(node, doc_id)` performs `workspaceBindingPending -> pending` on `lifecycle_state`. `RequestLifecycle::transition_execution_view(from: RequestLifecycleState, to: RequestLifecycleState)`.

- [ ] **Step 1: Delete the column and the private enum**

Remove `status: String @index` from the SDL. In `lifecycle.rs` delete the enum block (`:340-470`) and replace uses with `use gents_protocol::request_lifecycle::RequestLifecycleState;`. Every former `PersistedLifecycleState::X` becomes `RequestLifecycleState::X`; every former list helper call becomes the corresponding `RequestLifecycleState::*_graphql_list()`; `stuck_request_lifecycle_state_graphql_list()` becomes `RequestLifecycleState::graphql_list([RequestLifecycleState::Claimed, RequestLifecycleState::Processing])` inline at its 3 call sites.

- [ ] **Step 2: Rewrite the transition core**

`transition.rs:11-24` becomes:

```rust
fn request_view_is_terminal(view: &RequestViewRow) -> bool {
    RequestLifecycleState::is_terminal_str(view.lifecycle_state.as_deref())
}
```

`transition_execution_view` signature becomes `(&self, from: RequestLifecycleState, to: RequestLifecycleState)`; its mutation filter is `_docID` + `lifecycle_state: { _eq: "{from}" }` and its input is `lifecycle_state: "{to}", backend_id, failure_reason`. `transition_request_status` likewise drops the status arguments; the `AlreadyTarget` check compares `lifecycle_state` only. Update all call sites (`claim.rs:200`, `transition.rs:311,446`, `lifecycle.rs`, `recovery.rs`) by deleting the string arguments.

`materialize.rs`: `initial_status` → `create.initial_lifecycle_state = if bound { WorkspaceBindingPending } else { Pending }`. `activate_workspace_bound_request` filter `lifecycle_state: { _eq: "workspaceBindingPending" }`, input `lifecycle_state: "pending"`; the recovery re-read checks `lifecycle_state != "workspaceBindingPending"` instead of `status`.

`request_admission.rs:214` and `claim.rs:338`: filter `lifecycle_state: pending`, input `lifecycle_state: "failed"` (delete the `status: "error"` line). `interrupt.rs:active_session_request_id`: filter `lifecycle_state: { _in: ["claimed", "processing"] }` only. `watcher/query.rs`: all three `status: { _eq: "pending" }` filters become `lifecycle_state: { _eq: "pending" }`; add `lifecycle_state` to the row struct if not already selected and delete `status`.

`production_materializer.rs:recover_workspace_binding_pending_requests`: filter `lifecycle_state: { _eq: "workspaceBindingPending" }` only.

- [ ] **Step 3: Build the library**

Run: `cargo check -p gents 2>&1 | grep -E '^(error|warning: unused)' | head -40`
Expected: errors only in files outside this task's list (Task 4 handles them). Iterate on this task's files until none of them appear.

- [ ] **Step 4: Run the lifecycle unit tests that compile**

Run: `cargo test -p gents --lib lifecycle:: 2>&1 | tail -30` (if the lib does not compile yet because of Task 4 files, skip to Task 4 and run this at the end of Task 4).

- [ ] **Step 5: Commit**

```bash
git add crates/gents-schemas crates/gents/src/lifecycle* crates/gents/src/watcher crates/gents/src/request_admission.rs crates/gents/src/interrupt.rs crates/gents/src/streaming.rs crates/gents/src/trigger_engine/production_materializer.rs crates/gents/src/session/fork.rs crates/gents/src/goal.rs
git commit -m "runtime: lifecycle_state is the only request state column"
```

---

### Task 4: Runtime consumers — delete every string predicate

**Files (all under `crates/gents/src`):**
- `admission/recovery.rs:25,198-207` — delete `ParentRequestRow.status`, `request_is_terminal`, `request_is_interrupted` string bodies; use `RequestLifecycleState::is_terminal_str(row.lifecycle_state.as_deref())` and `parse_opt(..) == Some(Interrupted)`.
- `tool_call_lifecycle/recovery.rs:153,1958,2323-2381` — same; delete `request_status_or_lifecycle_is_terminal`; `request_is_cleanly_completed` = `parse_opt == Some(Completed)`; `request_is_cancel_worthy_terminal` = terminal && != Completed (keep the existing semantics, expressed on the enum).
- `background_tools.rs:1301,1345,2178-2216` — `project_child_terminal` matches on `RequestLifecycleState::parse_opt(row.lifecycle_state.as_deref())`: `Completed => None`, `Failed => ChildTerminal::Failed{..}`, `Dead => Dead`, `Interrupted => Interrupted`, `Superseded => Superseded`, nonterminal or `None => None`. Delete the `timedOut`/`cancelled`/`complete`/`error` arms and the `status` fallback branch. Filters at `:1301,:1345` move to `lifecycle_state`.
- `background_completion/{reconciliation,side_effects,queries}.rs`, `background_completion_diagnostics.rs` — `queries.rs:94` raw list becomes `RequestLifecycleState::terminal_graphql_list()`; drop `status` selections/fields.
- `trigger_engine/cross_deployment_cancel_mirror.rs:382` — delete `is_terminal_state`; use `is_terminal_str`. `trigger_engine/subagent_source.rs`, `trigger_engine/goal_source.rs` — same replacement; drop `status` from row structs and selections.
- `goal.rs:208-215` — `GoalRequestTerminal::parse` delegates to `RequestLifecycleState::parse_opt` and maps the five terminal variants; delete the string arms.
- `descendant_graph.rs:966-982` — `lifecycle_is_terminal` for request rows = `is_terminal_str`; keep `bridge_state_is_terminal` only for tool-call/bridge rows (their vocabulary is a different document) and make the request branch call the owner.
- `graph_pipeline/run.rs:225-234` and `graph_pipeline/runtime.rs` — delete `request_is_terminal`/`request_succeeded`; use `parse_opt(state) == Some(Completed)` / `is_terminal_str`; drop `status` from all `create_AgentRequest`/`update_AgentRequest` literals (`runtime.rs:2015-2224`). Delete the test `request_terminality_accepts_legacy_status_when_lifecycle_is_absent` (`run.rs:1294`).
- `workspace/overlay.rs`, `mailbox.rs`, `agent/daemon/inference.rs`, `agent/p2p_reconcile/{embedded_impl,enrollment_reconcile}.rs`, `hook/persistence/helpers.rs`, `run_timeline.rs` (`TimelineRequestRow.status` deleted), `run_timeline_fetch.rs`, `run_timeline_fetch/request_loaders.rs`, `toolset/session_history.rs`, `toolset/context_budget.rs`, `trace_export.rs`, `adapter_projection.rs`, `external_adapter_capture.rs`, `descendant_graph.rs`, `lifecycle/rows.rs` — remove `status` selections, struct fields, and any `== "complete"`/`"error"` comparisons that were on the request (comparisons on `AgentResponse.status` stay).
- `lean_vocab_test/` — if a test enumerates request lifecycle strings, point it at `RequestLifecycleState::ALL`.

**Interfaces:**
- Consumes: Task 2 owner type; Task 3 signatures.

- [ ] **Step 1: Sweep**

Use `grep -rn 'status' crates/gents/src --include='*.rs' -l | xargs grep -ln 'AgentRequest'` to enumerate candidates and the awk inventory in `scratchpad/status_selections.txt` if present. For each site decide: request `status` (delete or convert) vs. response/tool-call `status` (keep). Replace every predicate with the owner per the file list above.

- [ ] **Step 2: Compile the library and lib tests**

Run: `cargo check -p gents --lib && cargo test -p gents --lib --no-run 2>&1 | grep -E '^error' | head`
Expected: no errors.

- [ ] **Step 3: Grep gate**

Run: `grep -rn '"workspace_binding_pending"\|status: "error"\|"complete" | "completed"\|"error" | "failed"' crates/gents/src --include='*.rs' | grep -v AgentResponse | grep -v '/tests'`
Expected: no output. Run `grep -rn '"inputRequired"' crates/gents/src --include='*.rs' | grep -v lean_vocab_test` — expected empty.

- [ ] **Step 4: Run lib tests**

Run: `cargo test -p gents --lib 2>&1 | tail -30`
Expected: PASS (fixtures inside `src/**/tests.rs` that write `status:` on AgentRequest are fixed in this task since they are in-crate).

- [ ] **Step 5: Commit**

```bash
git add crates/gents/src
git commit -m "runtime: consumers classify request state through RequestLifecycleState"
```

---

### Task 5: `crates/gents` integration test fixtures

**Files:**
- Modify: every file under `crates/gents/tests/` that writes `status:` on an `AgentRequest` create/update or asserts on a request `status` value (about 200 sites; enumerate with `grep -rn 'AgentRequest' -A40 crates/gents/tests | grep -E 'status'`).
- Modify: `crates/gents/tests/conformance/request_lifecycle.rs` — `rust_request_transition_action` gains `("workspaceBindingPending", "pending") => Some("bindWorkspace")`; any inventory of production writers that lists `status` strings moves to `lifecycle_state`; the "production writers only reach contracted edges" inventory gains the `activate_workspace_bound_request` writer for the new edge.
- Modify: `crates/gents/tests/conformance/workspace_binding.rs`, `tests/e2e_*` fixtures.

- [ ] **Step 1: Sweep fixtures**

Delete `status: "..."` lines from AgentRequest mutations (leave `lifecycle_state`). Where a fixture only had `status` (e.g. `status: "pending"` with no `lifecycle_state`), replace it with `lifecycle_state: "pending"`. Assertions on `status == "error"` for a request become `lifecycle_state == "failed"`; `"processing"` becomes `"claimed"`/`"processing"` as the assertion intends.

- [ ] **Step 2: Build all test targets**

Run: `cargo test -p gents --no-run 2>&1 | grep -E '^error' | head -40`
Expected: none.

- [ ] **Step 3: Run the full package**

Run: `cargo test -p gents 2>&1 | tail -40` (this runs `lake build` for the contract; allow time).
Expected: all pass. Any failure is a real defect or a missed fixture; fix, do not skip.

- [ ] **Step 4: Commit**

```bash
git add crates/gents/tests
git commit -m "test(gents): fixtures write lifecycle_state only"
```

---

### Task 6: `gents-cli`

**Files (under `crates/gents-cli/src`):**
- `request_helpers.rs:165` — delete `is_terminal_lifecycle_state`; callers use `RequestLifecycleState::is_terminal_str`.
- `commands/subagent.rs:382`, `commands/codex_shim/continuation_stream.rs:441`, `http/prometheus.rs:1196` (request part only), `http/subagent_tree.rs:23-31,334-340` — delete the local predicates and lists; use the owner.
- `commands/codex_shim/{history_projection,progress,subagent_projection,turn/active,thread_projection/storage,thread_projection/json,thread_projection/usage}.rs`, `commands/{request,session,trace,graph,background}.rs`, `commands/chat/streaming.rs`, `commands/pack/scenario.rs`, `http/{fleet_slots,fleet,r5_dispatch,sessions,liveness}.rs`, `cli_adapter_interop_roundtrip` test suite — remove `status` from AgentRequest selections, row structs, and comparisons; add a `WorkspaceBindingPending` arm to any exhaustive `match` on `RequestLifecycleState` (treat like `Pending`).
- `crates/gents-cli/tests/suites/*` fixtures that write `status:` on AgentRequest.

- [ ] **Step 1: Sweep, compile, grep gate**

Run: `cargo check -p gents-cli --all-targets 2>&1 | grep -E '^error' | head -40` until clean. Then `grep -rn '"completed" | "failed"\|"complete" | "completed"\|TERMINAL_STATES' crates/gents-cli/src | grep -v AgentResponse` — expected empty.

- [ ] **Step 2: Test**

Run: `cargo test -p gents-cli 2>&1 | tail -40`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/gents-cli
git commit -m "cli: request state via RequestLifecycleState; drop status reads"
```

---

### Task 7: Desktop bridge, desktop core, generated TypeScript, review demo

**Files:**
- `crates/gents-desktop-bridge/src/{cascade,cause_derivation}.rs`, `commands/task.rs`, `snapshot/{subagent_tree,operations_snapshot,operations_signature}.rs`, `snapshot/session/{live_delta,projection,pending_turn}.rs`, `snapshot/runtime_tasks.rs`, `types/views/*.rs` — remove request `status` selections/fields; `operations_snapshot.rs:22` `TERMINAL_LIFECYCLE_STATES` and `subagent_tree.rs:20-28` `TERMINAL_STATES` are deleted in favor of the owner. Any `RenderedTimelineItem`/`SubagentNodeView` field that exposed request `status` is deleted from the view struct, then `cargo test -p gents-desktop-bridge` regenerates `packages/gents-desktop-client/src/generated/*.ts` via ts-rs (run the crate's ts export test; check `git status packages/gents-desktop-client/src/generated`).
- `crates/gents-desktop-core/src/client/core/writes.rs:41` (delete `is_terminal_lifecycle_state`), `client/mutations/chat/request.rs:670`, `client/store/*`, `client/query/*` — same.
- `apps/gents-desktop/src-tauri/src/runner/live_fixture.rs`, `apps/review-demo/src/live/pollRuntime.ts` — fixture/poll code drops request `status`.
- `packages/gents-desktop-chat`, `packages/gents-desktop-fleet`, `apps/gents-desktop/src` — anything reading a removed generated field; run `npm test` in the affected packages.

- [ ] **Step 1: Sweep and compile**

Run: `cargo check --workspace --all-targets 2>&1 | grep -E '^error' | head -40` until clean.

- [ ] **Step 2: Test Rust and TS**

Run: `cargo test -p gents-desktop-bridge -p gents-desktop-core 2>&1 | tail -30`; then `npm run typecheck && npm test` from repo root (or the package-level scripts in `package.json`).
Expected: PASS; `git status` shows regenerated `generated/*.ts` diffs only where a field was removed.

- [ ] **Step 3: Commit**

```bash
git add crates/gents-desktop-bridge crates/gents-desktop-core apps packages
git commit -m "desktop: request state via RequestLifecycleState; drop status reads"
```

---

### Task 8: Workspace gate, docs, and issue cross-references

**Files:**
- Modify: `CLAUDE.md` "The system, held in your head" request-flow bullet: append "Request state is one column, `lifecycle_state`, owned by `gents_protocol::request_lifecycle::RequestLifecycleState`; nothing else names its strings."
- Modify: `docs/gents.md` if it documents `AgentRequest.status`.

- [ ] **Step 1: Full gates**

Run in order:
```bash
cd crates/gents/proofs && lake build && cd -
cargo test -p gents 2>&1 | tail -5
cargo test -p gents-protocol -p gents-cli -p gents-desktop-bridge -p gents-desktop-core 2>&1 | tail -5
cargo check --workspace --all-targets 2>&1 | tail -3
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
```
Expected: all green.

- [ ] **Step 2: Net-deletion check**

Run: `git diff --shortstat main...HEAD`
Expected: deletions exceed insertions. If not, look for compat code that crept in and delete it.

- [ ] **Step 3: Grep gate across the workspace**

```bash
grep -rn 'workspace_binding_pending\|"inputRequired"' crates apps packages --include='*.rs' --include='*.ts' --include='*.tsx' | grep -v request_lifecycle.rs | grep -v generated | grep -v '/target/'
```
Expected: empty (Lean source under `proofs/` is allowed).

- [ ] **Step 4: Commit docs**

```bash
git add CLAUDE.md docs
git commit -m "docs: lifecycle_state is the single request state owner (#1330)"
```
