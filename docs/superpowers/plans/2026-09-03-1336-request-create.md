# #1336 One AgentRequestCreate Constructor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One function builds, stamps, and signs every production `AgentRequestCreate`; the six hand-rolled `AgentRequestCreate::base(...)` + field-stamping + sign sequences in the runtime call it.

**Architecture:** `lifecycle/materialize.rs` gains `RequestSpec` (the inputs a writer actually decides: identity, admission, initial state, lineage, workspace, subagent link, retry link, sampling carry-over, metadata, retry key, validity) and `build_signed_request(node_or_identity, spec) -> Result<AgentRequestCreate>`. Every field that today is copied by hand at some sites and forgotten at others (workspace lineage, conversation title metadata, `max_retries`, `subagent_depth`, `caused_by_*`) is stamped in exactly one place. Signing is chosen by a `RequestSigner` enum: `RegisteredTarget` (today's `sign_agent_request_create_as_registered_target`) or `Identity(&Identity)` (today's `sign_agent_request_create`).

**Tech Stack:** Rust; `gents_protocol::request_admission::{AgentRequestCreate, AgentRequestAdmissionRecord}`.

**Spec:** GitHub issue #1336.

## Global Constraints

- Every production writer produces byte-identical `AgentRequestCreate` values to today for the same inputs (assert with the existing fixtures where they compare mutations; add a table test that drives the new constructor through each historical site's inputs and compares against a snapshot of the old output captured before the refactor).
- No behavior change: admission kind per site, retry-key schemes, `max_retries` defaults, `subagent_depth`, `valid_until`, workspace propagation all unchanged.
- `MAX_SUBAGENT_DEPTH` and Lean-fenced admission checks stay where they are; this is DTO construction only.
- Net code deletion.

## Site inventory (what each stamps today)

| site | admission | extras stamped |
|---|---|---|
| `lifecycle/materialize.rs:255` (pending, trigger lineage) | `runtime_automated_trigger` / local_self depending on lineage (read the code) | metadata title + selected skills, retry_key, initial_lifecycle_state (pending / workspaceBindingPending), caused_by_trigger_* (6), max_retries, workspace_* (4); signer: registered target |
| `lifecycle/materialize.rs:425` (`materialize_claimed_with_execution_binding`) | `local_self` | backend_id, caused_by_trigger_* (5, no doc id), max_retries; signer: identity |
| `lifecycle/queue/mutation.rs:22` (session/steering wake) | `runtime_local_control` | metadata, retry_key, max_retries, subagent_depth (parent), caused_by_parent_request(+doc), correlation, trigger_context; signer: registered target |
| `lifecycle/queue/goal_continuation.rs:48` | `runtime_local_control` | metadata, retry_key, caused_by_trigger_id=goal, kind=goal, correlation, trigger_context, parent request(+doc), max_retries, subagent_depth, workspace_* (4); signer: registered target |
| `lifecycle/background_wake_recovery.rs:402` (redrive) | `runtime_local_control` | retry_parent(+doc), retry_root, retry_key, sampling (temperature, top_p, top_k, seed, max_tokens, max_total_tokens), metadata, backend_id, retry_count+1, max_retries (inherited), subagent_depth, parent request(+doc); signer: registered target |
| `tool_call_lifecycle/subagent_request.rs:335` | `runtime_local_child` / `runtime_cross_deployment_child` | metadata, valid_until, max_retries, subagent_depth, parent request(+doc), parent tool call(+doc), caused_by_trigger_id=tool call, kind=subagent, correlation, trigger_context, workspace_* (4); signer: registered target |
| `request_admission.rs:1284` | `local_self` | (read the site) signer: identity |
| `agent/p2p_reconcile/enrollment_reconcile.rs:1071` | `enrollment` | signer: identity (member) |
| `agent/daemon/inference.rs:902, 980` | `local_self` / `enrollment` | test-only fixtures (`#[cfg(test)]`); leave them |

---

### Task 1: `RequestSpec` and `build_signed_request`

**Files:**
- Modify: `crates/gents/src/lifecycle/materialize.rs` (add the types and function next to the existing helper; keep the existing pub helper signature as a thin wrapper if external callers use it, else delete it)
- Test: `crates/gents/src/lifecycle/materialize.rs` tests or `crates/gents/src/lifecycle/tests.rs`

**Interfaces (produces):**

```rust
pub(crate) struct RequestIdentity { pub request_id: String, pub agent_did: String, pub requester_did: Option<String>, pub behavior_id: String, pub session_id: String, pub content: String, pub execution_origin: ExecutionOrigin, pub created_at: String }
pub(crate) struct SubagentLink { pub depth: u32, pub parent_request_id: String, pub parent_request_doc_id: String, pub parent_tool_call_id: Option<String>, pub parent_tool_call_doc_id: Option<String> }
pub(crate) struct RetryLink { pub parent_request_id: String, pub parent_request_doc_id: String, pub root_request_id: String, pub retry_count: i64, pub max_retries: i64 }
pub(crate) struct SamplingCarryover { pub temperature: Option<f64>, pub top_p: Option<f64>, pub top_k: Option<i64>, pub seed: Option<i64>, pub max_tokens: Option<i64>, pub max_total_tokens: Option<i64>, pub backend_id: Option<String> }
pub(crate) struct RequestSpec {
    pub identity: RequestIdentity,
    pub admission: AgentRequestAdmissionRecord,
    pub initial_lifecycle_state: RequestLifecycleState,   // Pending | WorkspaceBindingPending
    pub trigger_lineage: TriggerLineage,                   // existing type; empty = none
    pub workspace: Option<WorkspaceLineage>,
    pub subagent: Option<SubagentLink>,
    pub retry: Option<RetryLink>,                          // None => max_retries = DEFAULT_REQUEST_MAX_RETRIES, retry_root = request_id
    pub sampling: Option<SamplingCarryover>,
    pub metadata: Option<String>,
    pub retry_key: Option<String>,
    pub valid_until: Option<String>,
}
pub(crate) enum RequestSigner<'a> { RegisteredTarget, Identity(&'a dyn crate::identity::SigningIdentity /* whatever sign_agent_request_create takes */) }
pub(crate) async fn build_signed_request(spec: RequestSpec, signer: RequestSigner<'_>) -> Result<AgentRequestCreate>;
```

(Use the real types `sign_agent_request_create` and `TriggerLineage`/`WorkspaceLineage` already take; adjust names to fit, keep the field set.)

- [ ] **Step 1: Capture today's outputs.** Before refactoring, write a test module that constructs each of the six production sites' `AgentRequestCreate` through the current code paths with fixed inputs (call the existing functions where they are pure enough; where they need a node, use the in-crate test node helpers the file's neighbors use) and snapshot `graphql_input_fields()` strings to `insta`-style literal assertions or plain `assert_eq!` against inline expected strings. Commit these tests first: `test(lifecycle): pin AgentRequestCreate outputs per writer`.
- [ ] **Step 2: Implement** `RequestSpec` and `build_signed_request` so the snapshot tests pass when the six sites are switched (Task 2). Field stamping rules: `retry_root_request = retry.map(root).unwrap_or(request_id)`; `max_retries = retry.map(|r| r.max_retries).unwrap_or(DEFAULT_REQUEST_MAX_RETRIES)`; `retry_count = retry.map(|r| r.retry_count).unwrap_or(0)`; `subagent_depth = subagent.map(depth).unwrap_or(0)`; workspace four fields from `workspace`; trigger six fields from `trigger_lineage`; parent four fields from `subagent`; `initial_lifecycle_state` from spec (validated to pre-claim by `graphql_input_fields` already).
- [ ] **Step 3: Commit** — `refactor(lifecycle): one signed AgentRequestCreate constructor (#1336)`.

### Task 2: Switch the six production sites

**Files:** the six sites in the inventory (not the test fixtures in `daemon/inference.rs`).

- [ ] **Step 1:** Replace each site's `AgentRequestCreate::base(...)` + stamping + sign with a `RequestSpec` literal and `build_signed_request`. Delete the now-unused helper `build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title` if `RequestSpec` covers it (or make it the one wrapper that only maps arguments). Keep each site's retry-key scheme and metadata building where it is.
- [ ] **Step 2:** `cargo test -p gents --lib lifecycle::` and the pinned snapshot tests green; `cargo test -p gents --test conformance request_lifecycle` green (the "production writers" fence); `cargo test -p gents --test e2e_subagent` green (subagent creation); `cargo test -p gents --test e2e_triggers` green.
- [ ] **Step 3:** Grep gate: `grep -rn 'AgentRequestCreate::base(' crates/gents/src | grep -v '#\[cfg(test)\]'` shows only `build_signed_request` (and test fixtures).
- [ ] **Step 4: Commit** — `refactor(lifecycle): request writers use RequestSpec (#1336)`.

### Task 3: Retry-key lookup shared (goal creation and continuation)

**Files:** `crates/gents/src/goal.rs:~1482-1652`, `crates/gents/src/lifecycle/queue/goal_continuation.rs:~110-152`.

- [ ] Extract one `pub(crate) async fn load_agent_request_by_retry_key(node, retry_key, expected: &GoalBackedRequestFingerprint) -> Result<Option<String>>` in `goal.rs` (query by `retry_key`, `rows.len() <= 1` assertion, fingerprint equality) and call it from both; the goal-creation caller keeps its extra Goal/GoalCreationClaim cross-check after the shared call.
- [ ] `cargo test -p gents --test misc goal_controller` and `--test conformance goals` green. Commit — `refactor(goal): one retry-key fingerprint lookup (#1336)`.

### Task 4: Gate
- [ ] `cargo test -p gents` full; `cargo check --workspace --all-targets`; `cargo fmt --all --check`; net deletion check against the base branch.
