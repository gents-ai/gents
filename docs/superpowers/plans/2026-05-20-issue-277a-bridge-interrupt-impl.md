# Issue #277 — Plan A: Bridge interrupt/cancel impls

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the three operator-surface Tauri command stubs in `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations.rs` with real implementations (`desktop_interrupt_request`, `desktop_preview_interrupt_cascade`), and extend `desktop_session_snapshot` (in `tauri_commands/chat.rs`) with derived `cancelCause` evidence on cancelled tool calls and interrupted responses.

**Architecture:** All three bridge impls operate against the embedded DefraDB node via the existing `ClientCore` accessor (`current_core(&state)` in `bridge/state.rs:50`). Cascade traversal mirrors the CLI walker at `crates/defra-agent-cli/src/commands/subagent.rs:327` but lives in a new `bridge/cascade.rs` module so it can be reused by both bridge commands. Preview-signature computation reuses `compute_preview_signature` in `bridge/snapshot/operations_signature.rs:26` — no new hashing code. CancelCause derivation is pure-Rust over rows already loaded by the session snapshot; the derivation algorithm and its evidence vocabulary are normative in `docs/superpowers/specs/2026-05-20-desktop-operator-surfaces-design.md:470-491`.

**Tech Stack:** Rust (tokio, serde, blake3), DefraDB embedded node, GraphQL mutations via `escape_graphql_string`, chrono for timestamps. Tests use the existing seeded-node pattern from `crates/defra-agent-desktop-core/tests/client_store.rs`.

**Out of scope (lands in Plans B & C):** React components, prop threading, Lean ledger promotion. The TypeScript type additions in Task 8 are the minimum needed so the Plan B work can be done against a real contract.

---

## File Structure

**Create:**
- `apps/desktop-tauri/src-tauri/src/bridge/cascade.rs` — descendant tree walker, classification by cancel_policy, preview construction.
- `apps/desktop-tauri/src-tauri/src/bridge/cause_derivation.rs` — pure-Rust CancelCause classifier with evidence rows.
- `apps/desktop-tauri/src-tauri/src/bridge/tests/operations_cascade.rs` — bridge integration tests against a seeded node, exercising all four preview groupings.
- `apps/desktop-tauri/src-tauri/src/bridge/tests/operations_interrupt.rs` — bridge integration tests for the three `InterruptRequestResult` paths (accepted / alreadyInterrupted / stalePreview).
- `apps/desktop-tauri/src-tauri/src/bridge/tests/cause_derivation.rs` — pure-Rust unit tests for every cause variant.

**Modify:**
- `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations.rs:40-62` — replace stub bodies for `desktop_preview_interrupt_cascade` and `desktop_interrupt_request`.
- `apps/desktop-tauri/src-tauri/src/bridge/mod.rs` — register new modules (`cascade`, `cause_derivation`, `tests`).
- `apps/desktop-tauri/src-tauri/src/bridge/types/views/operations.rs:170-180` — add `DerivedCancelCauseView` struct.
- `apps/desktop-tauri/src-tauri/src/bridge/types/views/session.rs` (or wherever `RenderedToolCallView` and `ResponseView` live — locate first) — add `cancel_cause: Option<DerivedCancelCauseView>` field to both.
- `apps/desktop-tauri/src-tauri/src/bridge/snapshot/session.rs` — populate `cancel_cause` in the existing tool-call and response builders using the new derivation helper.
- `apps/desktop-tauri/src/lib/types/operations.ts:115-145` — add `DerivedCancelCauseView` type; add `cancelCause?` to `RenderedToolCallView` and `ResponseView`.

**Reference (read, don't modify in Plan A):**
- `crates/defra-agent-cli/src/commands/subagent.rs:167-300` — CLI cascade walk + GraphQL mutation reference.
- `crates/defra-agent-cli/src/commands/subagent.rs:327` — `interrupt_request_local` signature.
- `crates/defra-agent-cli/src/request_helpers.rs:98` — GraphQL field set we'll mirror.
- `apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_signature.rs:1-60` — `PreviewSignatureInput` / `PreviewSignatureRow` / `compute_preview_signature`.

---

## Verification commands (run after every task)

```bash
cargo check -p defra-agent-desktop-tauri
cargo test  -p defra-agent-desktop-tauri --lib
```

Final verification (Task 11):
```bash
cd crates/defra-agent/proofs && lake build
cargo check
cargo test
```

---

### Task 1: Scaffold `bridge/cascade.rs` with the descendant-walk contract

**Files:**
- Create: `apps/desktop-tauri/src-tauri/src/bridge/cascade.rs`
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/mod.rs` — add `mod cascade;`
- Test:   `apps/desktop-tauri/src-tauri/src/bridge/tests/operations_cascade.rs` (skeleton)

This task only lays in the public types and the function signature. Walks are filled in in Task 2.

- [ ] **Step 1: Write the failing test (types compile)**

```rust
// apps/desktop-tauri/src-tauri/src/bridge/tests/operations_cascade.rs
use crate::bridge::cascade::{CascadeClassification, CascadeWalkRequest, CascadeWalkRow};

#[test]
fn cascade_request_default_shape() {
    let req = CascadeWalkRequest {
        root_request_id: "req_root".into(),
        agent_did: None,
        include_terminal: false,
    };
    assert_eq!(req.root_request_id, "req_root");
}

#[test]
fn cascade_classification_variant_names() {
    let v = CascadeClassification::WillInterrupt;
    assert!(matches!(v, CascadeClassification::WillInterrupt));
    let _ = CascadeClassification::WillDetach;
    let _ = CascadeClassification::AlreadyTerminal;
    let _ = CascadeClassification::UnknownPolicy;
}

#[test]
fn cascade_row_carries_lineage() {
    let row = CascadeWalkRow {
        request_id: "req_b91".into(),
        session_id: Some("sess_1".into()),
        behavior_id: Some("amy-general".into()),
        lifecycle_state: Some("processing".into()),
        parent_request_id: Some("req_root".into()),
        parent_tool_call_id: Some("tc_42".into()),
        tool_name: Some("summarize".into()),
        await_mode: Some("background".into()),
        cancel_policy: Some("cascade".into()),
        classification: CascadeClassification::WillInterrupt,
    };
    assert_eq!(row.parent_request_id.as_deref(), Some("req_root"));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p defra-agent-desktop-tauri --lib bridge::tests::operations_cascade
```
Expected: FAIL with "module `cascade` not found".

- [ ] **Step 3: Create `bridge/cascade.rs` with the public types and a stub walker**

```rust
//! Descendant tree walk for cascade preview and cascade interrupt.
//!
//! Mirrors `interrupt_request_local` in
//! `crates/defra-agent-cli/src/commands/subagent.rs:327`, but stays in the
//! bridge so both `desktop_preview_interrupt_cascade` and
//! `desktop_interrupt_request` can share the walk.

use defra_agent_desktop_core::client::ClientCore;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct CascadeWalkRequest {
    pub root_request_id: String,
    pub agent_did: Option<String>,
    pub include_terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CascadeClassification {
    WillInterrupt,
    WillDetach,
    AlreadyTerminal,
    UnknownPolicy,
}

#[derive(Debug, Clone)]
pub(crate) struct CascadeWalkRow {
    pub request_id: String,
    pub session_id: Option<String>,
    pub behavior_id: Option<String>,
    pub lifecycle_state: Option<String>,
    pub parent_request_id: Option<String>,
    pub parent_tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub classification: CascadeClassification,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CascadeWalkResult {
    pub root_state: Option<String>,
    pub root_interrupt_requested_at: Option<String>,
    pub rows: Vec<CascadeWalkRow>,
}

/// Walks `AgentToolCall.child_request_id` edges from `root_request_id` down,
/// classifying each descendant by the nearest bridge row's `cancel_policy`.
/// Filters terminal rows when `include_terminal == false`, except as
/// AlreadyTerminal evidence.
pub(crate) async fn walk(
    _core: &Arc<ClientCore>,
    _req: &CascadeWalkRequest,
) -> Result<CascadeWalkResult, String> {
    // Real impl lands in Task 2.
    Err("cascade::walk not implemented yet".into())
}
```

Add to `bridge/mod.rs`:
```rust
pub(crate) mod cascade;
#[cfg(test)]
mod tests {
    mod operations_cascade;
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p defra-agent-desktop-tauri --lib bridge::tests::operations_cascade
```
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add apps/desktop-tauri/src-tauri/src/bridge/cascade.rs \
        apps/desktop-tauri/src-tauri/src/bridge/mod.rs \
        apps/desktop-tauri/src-tauri/src/bridge/tests/operations_cascade.rs
git commit -m "bridge: scaffold cascade walker public types (#277)"
```

---

### Task 2: Implement the descendant walk against a seeded node

**Files:**
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/cascade.rs`
- Test:   `apps/desktop-tauri/src-tauri/src/bridge/tests/operations_cascade.rs`

Reference the existing CLI walker at `crates/defra-agent-cli/src/commands/subagent.rs:205-330` for the GraphQL query shape and the breadth-first child enumeration. Use the same direct-node-access pattern (not GraphQL strings) — `ClientCore::node()` returns the `EmbeddedNode`.

- [ ] **Step 1: Write the failing integration test**

```rust
// In operations_cascade.rs
use crate::bridge::cascade::{walk, CascadeClassification, CascadeWalkRequest};
use crate::bridge::tests::support::seed_cascade_fixture; // helper introduced below

#[tokio::test(flavor = "multi_thread")]
async fn walk_returns_classified_descendants_for_five_child_fixture() {
    let core = seed_cascade_fixture().await;
    let req = CascadeWalkRequest {
        root_request_id: "req_root".into(),
        agent_did: Some("did:test:operator".into()),
        include_terminal: true,
    };
    let result = walk(&core, &req).await.expect("walk ok");
    let kinds: Vec<_> = result.rows.iter().map(|r| r.classification).collect();
    // Fixture has 3 cascade-children, 1 detach-child, 1 unknown-policy child,
    // and 1 already-terminal previous turn.
    assert_eq!(kinds.iter().filter(|c| **c == CascadeClassification::WillInterrupt).count(), 3);
    assert_eq!(kinds.iter().filter(|c| **c == CascadeClassification::WillDetach).count(), 1);
    assert_eq!(kinds.iter().filter(|c| **c == CascadeClassification::UnknownPolicy).count(), 1);
    assert_eq!(kinds.iter().filter(|c| **c == CascadeClassification::AlreadyTerminal).count(), 1);
    assert_eq!(result.root_state.as_deref(), Some("processing"));
}

#[tokio::test(flavor = "multi_thread")]
async fn walk_returns_no_rows_for_standalone_root() {
    let core = crate::bridge::tests::support::seed_standalone_fixture().await;
    let req = CascadeWalkRequest {
        root_request_id: "req_solo".into(),
        agent_did: Some("did:test:operator".into()),
        include_terminal: false,
    };
    let result = walk(&core, &req).await.expect("walk ok");
    assert!(result.rows.is_empty());
}
```

Also create `apps/desktop-tauri/src-tauri/src/bridge/tests/support.rs` with `seed_cascade_fixture()` and `seed_standalone_fixture()` helpers that build an in-memory `ClientCore`, insert `AgentRequest` + `AgentToolCall` rows matching the fixture shapes used by the prototype's `five-children` and `standalone` scenarios. Use existing seeding helpers from `crates/defra-agent-desktop-core/tests/client_store.rs` as a reference for how to bring up an embedded node in a test.

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p defra-agent-desktop-tauri --lib bridge::tests::operations_cascade::walk_
```
Expected: FAIL with the "not implemented yet" error from Task 1's stub (or missing `support` module).

- [ ] **Step 3: Implement `walk` and the test support module**

The walker:
1. Loads the root `AgentRequest` row (returns `Err` with a clear message if missing — root must exist).
2. Records `root_state` and `root_interrupt_requested_at`.
3. BFS via `AgentToolCall { request_id: { _eq: parent }, child_request_id: { _ne: "" } }` queries — for each match, classify the child:
   - terminal child request → `AlreadyTerminal` (always emitted; the `include_terminal` flag controls whether terminal rows are *visited for further descent*, not whether they're emitted).
   - non-terminal + `cancel_policy = "cascade"` → `WillInterrupt`, recurse.
   - non-terminal + `cancel_policy = "detach"` → `WillDetach`, do not recurse (detach severs propagation).
   - non-terminal + `cancel_policy IS NULL or other` → `UnknownPolicy`, do not recurse.
4. Stop at depth 8 (matches CLI walker's safety limit) and return `Err("cascade depth exceeded")` if hit.

Use `core.node()` for direct node access; build GraphQL strings with `defra_agent_protocol::graphql::escape_graphql_string`. Reuse the row shape and field names from `crates/defra-agent-cli/src/request_helpers.rs:80-110`.

(Full impl code is large; structure it as a `bfs(...)` helper called from `walk` so it's straightforward to unit-test descent order separately if needed.)

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p defra-agent-desktop-tauri --lib bridge::tests::operations_cascade::walk_
```
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add apps/desktop-tauri/src-tauri/src/bridge/cascade.rs \
        apps/desktop-tauri/src-tauri/src/bridge/tests/operations_cascade.rs \
        apps/desktop-tauri/src-tauri/src/bridge/tests/support.rs
git commit -m "bridge: implement cascade descendant walk against embedded node (#277)"
```

---

### Task 3: Wire `desktop_preview_interrupt_cascade` to the walker + signature

**Files:**
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations.rs:40-50` — real body.
- Test:   `apps/desktop-tauri/src-tauri/src/bridge/tests/operations_cascade.rs` — bridge-level test.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn preview_returns_four_classified_groups_and_a_signature() {
    let core = seed_cascade_fixture().await;
    let preview = build_cascade_preview(
        &core,
        &DesktopPreviewInterruptCascadeRequest {
            request_id: "req_root".into(),
            agent_did: Some("did:test:operator".into()),
            include_terminal: Some(true),
        },
    )
    .await
    .expect("preview ok");

    assert_eq!(preview.root_request_id, "req_root");
    assert_eq!(preview.root_state.as_deref(), Some("processing"));
    assert_eq!(preview.will_interrupt.len(), 3);
    assert_eq!(preview.will_detach.len(), 1);
    assert_eq!(preview.already_terminal.len(), 1);
    assert_eq!(preview.unknown_policy.len(), 1);
    assert_eq!(preview.preview_signature.len(), 64); // blake3 hex
}
```

- [ ] **Step 2: Run to verify FAIL** — `build_cascade_preview` doesn't exist.

- [ ] **Step 3: Implement `build_cascade_preview` + wire stub body**

Add to `bridge/cascade.rs`:
```rust
use crate::bridge::snapshot::{compute_preview_signature, PreviewSignatureInput, PreviewSignatureRow};
use crate::bridge::types::views::operations::{CascadeAffectedRequest, CascadeCancelPreview};
use crate::bridge::types::requests::operations::DesktopPreviewInterruptCascadeRequest;

pub(crate) async fn build_cascade_preview(
    core: &Arc<ClientCore>,
    req: &DesktopPreviewInterruptCascadeRequest,
) -> Result<CascadeCancelPreview, String> {
    let walk_req = CascadeWalkRequest {
        root_request_id: req.request_id.clone(),
        agent_did: req.agent_did.clone(),
        include_terminal: req.include_terminal.unwrap_or(true),
    };
    let result = walk(core, &walk_req).await?;

    let mut will_interrupt = Vec::new();
    let mut will_detach    = Vec::new();
    let mut already_terminal = Vec::new();
    let mut unknown_policy = Vec::new();
    let mut sig_rows = Vec::new();

    for row in &result.rows {
        let view = CascadeAffectedRequest {
            request_id: row.request_id.clone(),
            session_id: row.session_id.clone(),
            behavior_id: row.behavior_id.clone(),
            lifecycle_state: row.lifecycle_state.clone(),
            parent_request_id: row.parent_request_id.clone(),
            parent_tool_call_id: row.parent_tool_call_id.clone(),
            tool_name: row.tool_name.clone(),
            await_mode: row.await_mode.clone(),
            cancel_policy: row.cancel_policy.clone(),
        };
        sig_rows.push(PreviewSignatureRow {
            request_id: row.request_id.clone(),
            lifecycle_state: row.lifecycle_state.clone(),
            await_mode: row.await_mode.clone(),
            cancel_policy: row.cancel_policy.clone(),
            parent_tool_call_id: row.parent_tool_call_id.clone(),
        });
        match row.classification {
            CascadeClassification::WillInterrupt  => will_interrupt.push(view),
            CascadeClassification::WillDetach     => will_detach.push(view),
            CascadeClassification::AlreadyTerminal=> already_terminal.push(view),
            CascadeClassification::UnknownPolicy  => unknown_policy.push(view),
        }
    }

    let preview_signature = compute_preview_signature(&PreviewSignatureInput {
        root_request_id: req.request_id.clone(),
        root_state: result.root_state.clone(),
        root_interrupt_requested_at: result.root_interrupt_requested_at.clone(),
        affected: sig_rows,
    });

    Ok(CascadeCancelPreview {
        root_request_id: req.request_id.clone(),
        preview_signature,
        root_state: result.root_state,
        will_interrupt,
        will_detach,
        already_terminal,
        unknown_policy,
    })
}
```

Replace `desktop_preview_interrupt_cascade` body in `tauri_commands/operations.rs`:
```rust
#[tauri::command]
pub(crate) async fn desktop_preview_interrupt_cascade(
    state: State<'_, DesktopAppState>,
    request: DesktopPreviewInterruptCascadeRequest,
) -> Result<CascadeCancelPreview, String> {
    let core = super::super::state::current_core(&state)
        .ok_or_else(|| "desktop bridge core not initialized".to_string())?;
    crate::bridge::cascade::build_cascade_preview(&core, &request).await
}
```

- [ ] **Step 4: Run to verify PASS**.

- [ ] **Step 5: Commit**

```bash
git commit -am "tauri_commands: real impl of desktop_preview_interrupt_cascade (#277)"
```

---

### Task 4: Add the bridge interrupt latch (no cascade)

**Files:**
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/cascade.rs` — add `latch_root_interrupt`.
- Test:   `apps/desktop-tauri/src-tauri/src/bridge/tests/operations_interrupt.rs` (new).

- [ ] **Step 1: Write the failing test**

```rust
// apps/desktop-tauri/src-tauri/src/bridge/tests/operations_interrupt.rs
use crate::bridge::cascade::latch_root_interrupt;
use crate::bridge::tests::support::{seed_standalone_fixture, fetch_request_row};

#[tokio::test(flavor = "multi_thread")]
async fn latch_writes_interrupt_requested_at_when_absent() {
    let core = seed_standalone_fixture().await;
    let before = fetch_request_row(&core, "req_solo").await;
    assert!(before.interrupt_requested_at.is_none());

    let latched = latch_root_interrupt(&core, "req_solo").await.expect("latch ok");
    assert!(latched.was_first);
    assert!(!latched.interrupt_requested_at.is_empty());

    let after = fetch_request_row(&core, "req_solo").await;
    assert_eq!(after.interrupt_requested_at.as_deref(), Some(latched.interrupt_requested_at.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn latch_is_noop_when_already_interrupted() {
    let core = seed_standalone_fixture().await;
    let _ = latch_root_interrupt(&core, "req_solo").await.expect("first latch");
    let second = latch_root_interrupt(&core, "req_solo").await.expect("second latch");
    assert!(!second.was_first);
}
```

- [ ] **Step 2: Run to verify FAIL**.

- [ ] **Step 3: Implement `latch_root_interrupt`**

Mirror `interrupt_request_graphql` from `crates/defra-agent-cli/src/commands/subagent.rs:167`. Read the row first; if `interrupt_requested_at` is already present, return `was_first: false` with that timestamp. Otherwise compute `chrono::Utc::now().to_rfc3339()`, run the GraphQL `update_AgentRequest` mutation, and return `was_first: true`. Use `defra_agent_protocol::graphql::escape_graphql_string` for the request_id.

```rust
#[derive(Debug, Clone)]
pub(crate) struct LatchResult {
    pub interrupt_requested_at: String,
    pub was_first: bool,
}

pub(crate) async fn latch_root_interrupt(
    core: &Arc<ClientCore>,
    request_id: &str,
) -> Result<LatchResult, String> {
    // Implementation: fetch row → branch on interrupt_requested_at →
    // optionally mutate → return LatchResult.
}
```

- [ ] **Step 4: Run to verify PASS**.

- [ ] **Step 5: Commit**

```bash
git commit -am "bridge: add root-request interrupt latch helper (#277)"
```

---

### Task 5: Wire `desktop_interrupt_request` — accepted + alreadyInterrupted paths

**Files:**
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations.rs:52-62`
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/cascade.rs` — orchestration entry `interrupt_request`.
- Test:   `apps/desktop-tauri/src-tauri/src/bridge/tests/operations_interrupt.rs`

- [ ] **Step 1: Write the failing tests for both terminal paths**

```rust
use crate::bridge::types::requests::operations::DesktopInterruptRequest;
use crate::bridge::cascade::interrupt_request;

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_request_no_cascade_returns_accepted() {
    let core = seed_standalone_fixture().await;
    let result = interrupt_request(&core, &DesktopInterruptRequest {
        request_id: "req_solo".into(),
        cause: "userCancelled".into(),
        cascade: false,
        expected_preview_signature: None,
        agent_did: Some("did:test:operator".into()),
    }).await.expect("ok");
    assert!(result.accepted);
    assert!(!result.already_interrupted);
    assert!(!result.stale_preview);
    assert!(result.interrupt_requested_at.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_request_returns_already_interrupted_for_second_call() {
    let core = seed_standalone_fixture().await;
    let _ = interrupt_request(&core, &DesktopInterruptRequest {
        request_id: "req_solo".into(),
        cause: "userCancelled".into(), cascade: false,
        expected_preview_signature: None, agent_did: None,
    }).await.expect("first");
    let second = interrupt_request(&core, &DesktopInterruptRequest {
        request_id: "req_solo".into(),
        cause: "userCancelled".into(), cascade: false,
        expected_preview_signature: None, agent_did: None,
    }).await.expect("second");
    assert!(!second.accepted);
    assert!(second.already_interrupted);
}
```

- [ ] **Step 2: Run to verify FAIL**.

- [ ] **Step 3: Implement `interrupt_request` for non-cascade path**

```rust
pub(crate) async fn interrupt_request(
    core: &Arc<ClientCore>,
    req: &DesktopInterruptRequest,
) -> Result<InterruptRequestResult, String> {
    // Only "userCancelled" is operator-authentic. Other causes must be rejected
    // — the runtime owns deadline/interrupted derivation.
    if req.cause != "userCancelled" {
        return Err(format!(
            "operator may only authentically produce cause=\"userCancelled\", got {:?}",
            req.cause
        ));
    }

    if !req.cascade {
        let latched = latch_root_interrupt(core, &req.request_id).await?;
        return Ok(InterruptRequestResult {
            request_id: req.request_id.clone(),
            accepted: latched.was_first,
            interrupt_requested_at: Some(latched.interrupt_requested_at),
            already_interrupted: !latched.was_first,
            stale_preview: false,
            preview: None,
        });
    }
    // Cascade branch lands in Task 6.
    todo!("cascade branch — Task 6")
}
```

Replace the stub body in `tauri_commands/operations.rs`:
```rust
#[tauri::command]
pub(crate) async fn desktop_interrupt_request(
    state: State<'_, DesktopAppState>,
    request: DesktopInterruptRequest,
) -> Result<InterruptRequestResult, String> {
    let core = super::super::state::current_core(&state)
        .ok_or_else(|| "desktop bridge core not initialized".to_string())?;
    crate::bridge::cascade::interrupt_request(&core, &request).await
}
```

- [ ] **Step 4: Run to verify the two new tests pass; verify `cargo build` still succeeds despite the `todo!`** (it will at compile time; we just won't exercise that branch yet).

- [ ] **Step 5: Commit**

```bash
git commit -am "tauri_commands: real impl of desktop_interrupt_request (non-cascade path) (#277)"
```

---

### Task 6: Add the cascade branch with `stalePreview` detection

**Files:**
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/cascade.rs` — replace the `todo!` in `interrupt_request`.
- Test:   `apps/desktop-tauri/src-tauri/src/bridge/tests/operations_interrupt.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn interrupt_request_cascade_returns_accepted_when_signature_matches() {
    let core = seed_cascade_fixture().await;
    let preview = build_cascade_preview(&core, &DesktopPreviewInterruptCascadeRequest {
        request_id: "req_root".into(),
        agent_did: Some("did:test:operator".into()),
        include_terminal: Some(true),
    }).await.unwrap();

    let result = interrupt_request(&core, &DesktopInterruptRequest {
        request_id: "req_root".into(),
        cause: "userCancelled".into(),
        cascade: true,
        expected_preview_signature: Some(preview.preview_signature.clone()),
        agent_did: Some("did:test:operator".into()),
    }).await.expect("ok");

    assert!(result.accepted);
    assert!(!result.stale_preview);
    assert!(result.preview.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_request_cascade_returns_stale_preview_when_signature_drifts() {
    let core = seed_cascade_fixture().await;
    let result = interrupt_request(&core, &DesktopInterruptRequest {
        request_id: "req_root".into(),
        cause: "userCancelled".into(),
        cascade: true,
        expected_preview_signature: Some("00".repeat(32)), // wrong sig
        agent_did: Some("did:test:operator".into()),
    }).await.expect("ok");

    assert!(!result.accepted);
    assert!(result.stale_preview);
    let fresh = result.preview.expect("fresh preview attached");
    assert_eq!(fresh.root_request_id, "req_root");
    assert_eq!(fresh.preview_signature.len(), 64);
    assert_ne!(fresh.preview_signature, "00".repeat(32));
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_request_cascade_rejects_when_expected_signature_missing() {
    let core = seed_cascade_fixture().await;
    let err = interrupt_request(&core, &DesktopInterruptRequest {
        request_id: "req_root".into(),
        cause: "userCancelled".into(),
        cascade: true,
        expected_preview_signature: None,
        agent_did: None,
    }).await.unwrap_err();
    assert!(err.contains("expectedPreviewSignature"));
}
```

- [ ] **Step 2: Run to verify FAIL**.

- [ ] **Step 3: Replace the `todo!` with the cascade branch**

```rust
// Cascade path:
let expected_sig = req.expected_preview_signature.clone().ok_or_else(|| {
    "cascade=true requires expectedPreviewSignature".to_string()
})?;
let preview = build_cascade_preview(core, &DesktopPreviewInterruptCascadeRequest {
    request_id: req.request_id.clone(),
    agent_did: req.agent_did.clone(),
    include_terminal: Some(true),
}).await?;
if preview.preview_signature != expected_sig {
    return Ok(InterruptRequestResult {
        request_id: req.request_id.clone(),
        accepted: false,
        interrupt_requested_at: None,
        already_interrupted: false,
        stale_preview: true,
        preview: Some(preview),
    });
}

// Signature matches — latch the root. Cascade observers / runtime will
// complete child requests; the bridge does not eagerly cancel children
// (that's the runtime's contract).
let latched = latch_root_interrupt(core, &req.request_id).await?;
Ok(InterruptRequestResult {
    request_id: req.request_id.clone(),
    accepted: latched.was_first,
    interrupt_requested_at: Some(latched.interrupt_requested_at),
    already_interrupted: !latched.was_first,
    stale_preview: false,
    preview: None,
})
```

- [ ] **Step 4: Run all interrupt tests to verify PASS**.

- [ ] **Step 5: Commit**

```bash
git commit -am "bridge: cascade interrupt with stalePreview signature guard (#277)"
```

---

### Task 7: Add `DerivedCancelCauseView` types (Rust + TS)

**Files:**
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/types/views/operations.rs` — add the view struct after `InterruptRequestResult`.
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/types/views/session.rs` (locate first; likely contains `RenderedToolCallView`, `ResponseView`) — add `cancel_cause: Option<DerivedCancelCauseView>` field on both.
- Modify: `apps/desktop-tauri/src/lib/types/operations.ts:115-145` — mirror in TS.
- Modify: `apps/desktop-tauri/src/lib/types/session.ts` (locate) — add `cancelCause?` to mirror types.

Pure type additions — no logic. Compile is the test.

- [ ] **Step 1: Add Rust types**

```rust
// in views/operations.rs (after InterruptRequestResult)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DerivedCancelCauseView {
    pub cause: String,        // "userCancelled" | "interrupted" | "deadline" | "unknown"
    pub source: String,       // "requestInterrupt" | "parentCascade" | "deadline" | "toolLifecycle" | "responseInterruptedAt" | "unresolved"
    pub confidence: String,   // "direct" | "derived"
    pub at: Option<String>,
    pub evidence: Vec<String>,
}
```

Add `pub cancel_cause: Option<DerivedCancelCauseView>` to both `RenderedToolCallView` and `ResponseView` (use `#[serde(skip_serializing_if = "Option::is_none")]`).

- [ ] **Step 2: Mirror in TypeScript**

```ts
// in lib/types/operations.ts
export type DerivedCancelCauseView = {
  cause: "userCancelled" | "interrupted" | "deadline" | "unknown";
  source:
    | "requestInterrupt" | "parentCascade" | "deadline"
    | "toolLifecycle"   | "responseInterruptedAt" | "unresolved";
  confidence: "direct" | "derived";
  at?: string | null;
  evidence: string[];
};
```
Add optional `cancelCause?: DerivedCancelCauseView | null` to `RenderedToolCallView` and `ResponseView` (locate in `lib/types/session.ts` or wherever they live).

- [ ] **Step 3: Verify compilation**

```bash
cargo check -p defra-agent-desktop-tauri
( cd apps/desktop-tauri && npx tsc --noEmit )
```
Expected: both pass.

- [ ] **Step 4: Commit**

```bash
git commit -am "types: add DerivedCancelCauseView (Rust + TS) (#277)"
```

---

### Task 8: Implement `cause_derivation` module

**Files:**
- Create: `apps/desktop-tauri/src-tauri/src/bridge/cause_derivation.rs`
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/mod.rs` — register module.
- Test:   `apps/desktop-tauri/src-tauri/src/bridge/tests/cause_derivation.rs`

Derivation is pure-Rust over already-loaded rows — fast unit tests.

- [ ] **Step 1: Write failing unit tests for all four variants**

```rust
use crate::bridge::cause_derivation::{
    derive_tool_call_cause, derive_response_cause,
    ToolCallEvidence, RequestEvidence, ResponseEvidence,
};

#[test]
fn user_cancelled_when_root_has_interrupt_and_no_parent_cascade() {
    let req = RequestEvidence {
        request_id: "req_root".into(),
        interrupt_requested_at: Some("2026-05-20T10:32:14Z".into()),
        caused_by_parent_request_id: None,
        deadline_breached: false,
    };
    let tool = ToolCallEvidence {
        tool_call_id: "tc_1".into(),
        lifecycle_state: Some("cancelled".into()),
        deadline_at: None,
        cancel_policy: Some("cascade".into()),
        completed_at: Some("2026-05-20T10:32:15Z".into()),
        timed_out: false,
    };
    let cause = derive_tool_call_cause(&req, &tool).expect("derives");
    assert_eq!(cause.cause, "userCancelled");
    assert_eq!(cause.source, "requestInterrupt");
    assert_eq!(cause.confidence, "direct");
    assert!(cause.evidence.iter().any(|e| e.contains("interrupt_requested_at")));
}

#[test]
fn interrupted_when_request_has_parent_cascade() {
    let req = RequestEvidence {
        request_id: "req_child".into(),
        interrupt_requested_at: None,
        caused_by_parent_request_id: Some("req_parent".into()),
        deadline_breached: false,
    };
    let tool = ToolCallEvidence { /* cancelled, policy=cascade */ ..todo_default() };
    let cause = derive_tool_call_cause(&req, &tool).expect("derives");
    assert_eq!(cause.cause, "interrupted");
    assert_eq!(cause.source, "parentCascade");
}

#[test]
fn deadline_when_tool_lifecycle_is_timedout() {
    let req = RequestEvidence { /* no interrupt, no parent */ ..todo_default() };
    let tool = ToolCallEvidence {
        timed_out: true,
        lifecycle_state: Some("timedOut".into()),
        deadline_at: Some("2026-05-20T10:34:00Z".into()),
        completed_at: Some("2026-05-20T10:35:02Z".into()),
        ..todo_default()
    };
    let cause = derive_tool_call_cause(&req, &tool).expect("derives");
    assert_eq!(cause.cause, "deadline");
    assert_eq!(cause.source, "toolLifecycle");
}

#[test]
fn unknown_when_cancelled_but_no_evidence() {
    let req = RequestEvidence { /* no interrupt, no parent */ ..todo_default() };
    let tool = ToolCallEvidence {
        lifecycle_state: Some("cancelled".into()),
        ..todo_default()
    };
    let cause = derive_tool_call_cause(&req, &tool).expect("derives");
    assert_eq!(cause.cause, "unknown");
    assert_eq!(cause.source, "unresolved");
    // Evidence should enumerate what was checked.
    assert!(cause.evidence.iter().any(|e| e.contains("no parent cascade")));
    assert!(cause.evidence.iter().any(|e| e.contains("no deadline")));
    assert!(cause.evidence.iter().any(|e| e.contains("no interrupt_requested_at")));
}

#[test]
fn none_for_non_cancelled_tool_calls() {
    let req = RequestEvidence::default();
    let tool = ToolCallEvidence {
        lifecycle_state: Some("completed".into()),
        ..todo_default()
    };
    assert!(derive_tool_call_cause(&req, &tool).is_none());
}

#[test]
fn response_cause_uses_response_interrupted_at_when_present() {
    let req = RequestEvidence { /* no parent */ ..todo_default() };
    let resp = ResponseEvidence {
        interrupted_at: Some("2026-05-20T10:36:11Z".into()),
        completed_at: None,
    };
    let cause = derive_response_cause(&req, &resp).expect("derives");
    assert_eq!(cause.cause, "interrupted");
    assert_eq!(cause.source, "responseInterruptedAt");
}
```

(The `todo_default()` helper just returns a `ToolCallEvidence::default()` — define `Default` on the evidence structs.)

- [ ] **Step 2: Run to verify FAIL**.

- [ ] **Step 3: Implement `cause_derivation.rs`**

Pure-Rust, no IO. The derivation precedence (from spec §470-491):
1. If tool was `timedOut` (or request deadline breached and no other signal) → `deadline`.
2. Else if request has `caused_by_parent_request_id` AND tool's `cancel_policy = "cascade"` → `interrupted` (parent cascade evidence wins over user-cancel evidence on the *child*).
3. Else if the root request has `interrupt_requested_at` and no parent → `userCancelled`.
4. Else if tool is cancelled but none of the above → `unknown` with evidence enumerating what was checked.

Return `None` for non-cancelled tool calls. Construct `DerivedCancelCauseView` with appropriate `evidence: Vec<String>` lines.

- [ ] **Step 4: Run to verify PASS**.

- [ ] **Step 5: Commit**

```bash
git commit -am "bridge: pure-rust CancelCause derivation with evidence (#277)"
```

---

### Task 9: Wire derivation into `build_session_snapshot_from_store_for_agent`

**Files:**
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/snapshot/session.rs` — populate `cancel_cause` on cancelled tool calls and interrupted responses.
- Test:   `apps/desktop-tauri/src-tauri/src/bridge/snapshot/tests/session_state.rs` — add a case.

- [ ] **Step 1: Write the failing snapshot test**

In `session_state.rs`, add a test that builds a session with one cancelled tool call (cause: userCancelled), runs the snapshot, and asserts `snapshot.tool_groups[0].tools[0].cancel_cause.as_ref().unwrap().cause == "userCancelled"`. Pattern after existing `session_state.rs` cases (read the file first to match style).

- [ ] **Step 2: Run to verify FAIL**.

- [ ] **Step 3: Inject derivation into the snapshot builder**

In `session.rs`, locate where `RenderedToolCallView` is constructed. For each tool call, build a `ToolCallEvidence` + `RequestEvidence` from the rows already in scope, call `derive_tool_call_cause`, and assign the result to the new `cancel_cause` field. Same for `ResponseView` — call `derive_response_cause` with the AgentResponse row.

- [ ] **Step 4: Run to verify PASS**, and re-run the full session_state suite to ensure no regression.

```bash
cargo test -p defra-agent-desktop-tauri --lib bridge::snapshot::tests::session_state
```

- [ ] **Step 5: Commit**

```bash
git commit -am "snapshot: derive cancelCause on cancelled tool calls and interrupted responses (#277)"
```

---

### Task 10: Update the lone existing reference to the stubs

Search for any tests or callers that asserted on the old stub error strings (e.g. "not implemented yet; landing via panel #283") and update them. Likely candidates: `apps/desktop-tauri/src-tauri/src/bridge/tests.rs`.

- [ ] **Step 1: Search**
```bash
grep -rn "not implemented yet; landing via panel" apps/desktop-tauri
```

- [ ] **Step 2: For each match, replace the stub-error assertion with an assertion against the real return shape** (or remove the test if it was only asserting "stub returns error").

- [ ] **Step 3: Run the full desktop-tauri test suite**
```bash
cargo test -p defra-agent-desktop-tauri
```

- [ ] **Step 4: Commit**
```bash
git commit -am "tests: update bridge stub assertions to real interrupt impl (#277)"
```

---

### Task 11: Final verification + push

- [ ] **Step 1: Full workspace check**

```bash
cd crates/defra-agent/proofs && lake build && cd -
cargo check
cargo test
```
Expected: all pass.

- [ ] **Step 2: Push**

```bash
git push origin design/issue-277-cancel-ux-prototype
```

- [ ] **Step 3: Hand off to Plan B**

Plan A is complete. Plan B (frontend components) can now build against the real `desktop_interrupt_request`, `desktop_preview_interrupt_cascade`, and the new `cancelCause` field on snapshot rows. Update PR #325's description to add a "Plan A landed" section listing the commits.

---

## Self-Review

- **Spec coverage:** Tasks 3 + 6 cover Panel 6 (cascade dialog backend). Tasks 4 + 5 + 6 cover Panel 2 (interrupt button backend). Tasks 7 + 8 + 9 cover Panel 4 (CancelCause surfacing) up to the bridge boundary; the React side lives in Plan B.
- **Placeholder scan:** Task 2's `walk` impl is described prose-style rather than coded line-by-line because the GraphQL query strings are long and the executor can follow the CLI reference at `subagent.rs:205-330`. Task 9's snapshot test is described in style rather than full code because it needs to match patterns the executor will read in-context. These are intentional, not placeholders — every step has a concrete output and verification command.
- **Type consistency:** `InterruptRequestResult` field names (`accepted`, `already_interrupted`, `stale_preview`, `interrupt_requested_at`, `preview`) match `apps/desktop-tauri/src/lib/types/operations.ts:137-144` exactly. `DerivedCancelCauseView` cause/source enum strings match the TS union spec'd in the operator-surfaces design doc.
- **Risk:** Task 2 (descendant walk) is the largest unknown. If the seeded-node test fixture takes longer than ~2 hours to build, split into a separate task or borrow more aggressively from `crates/defra-agent-desktop-core/tests/client_store.rs` seeding helpers.
