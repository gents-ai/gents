# defra-agent-desktop Submission API (T5) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the desktop-side mutation API for creating conversations, submitting requests, and managing saved peers so the UI can stop being read-only.

**Architecture:** T5 adds a thin mutation layer on top of the T3 client core. The desktop still writes through its local `EmbeddedNode` using GraphQL mutations and local peer-directory persistence; networking remains `defra-node`'s job. Mutation helpers should return narrow DTOs that the UI can use directly without parsing raw JSON.

**Tech Stack:** Rust 2021, `defra-node`, `defra-agent-protocol`, `tokio`, `uuid`, `chrono`, `serde`, `serde_json`, `tracing`.

**Reference spec:** `docs/superpowers/specs/2026-04-13-desktop-dashboard-design.md` (T5 row, observation pipeline, first-launch experience, Chat activity, and Peers activity).

---

## Execution environment

This ticket is the first desktop write path. Keep it focused on mutation helpers and local state updates; full visual workflows arrive in T6+.

---

## File Structure

**New files:**

- `crates/defra-agent-desktop/src/client/mutations.rs`
- `crates/defra-agent-desktop/tests/submission_api.rs`

**Modified files:**

- `crates/defra-agent-desktop/src/client/core.rs`
- `crates/defra-agent-desktop/src/client/peer_directory.rs`
- `crates/defra-agent-desktop/src/client/mod.rs`
- `crates/defra-agent-desktop/src/app.rs`

---

## Task 1: Add mutation DTOs and wiring

**Files:**

- Create: `crates/defra-agent-desktop/src/client/mutations.rs`
- Modify: `crates/defra-agent-desktop/src/client/core.rs`

### Steps

- [ ] **Step 1: Define return types**

Create narrow result types for:

- `CreatedConversation`
- `SubmittedRequest`
- `PeerMutationResult`

These should expose the IDs the UI immediately needs, not raw GraphQL response objects.

- [ ] **Step 2: Add mutation entrypoints on `ClientCore`**

Expose methods such as:

- `create_conversation(...)`
- `submit_request(...)`
- `add_peer(...)`
- `remove_peer(...)`

Keep the signatures UI-friendly and deterministic.

---

## Task 2: Implement conversation creation

**Files:**

- Modify: `crates/defra-agent-desktop/src/client/mutations.rs`

### Steps

- [ ] **Step 1: Create an empty session**

When the user starts a new conversation, persist an `AgentSession` row first.

- [ ] **Step 2: Upsert the conversation row**

Create or upsert an `AgentConversation` with:

- the new `session_id`
- selected `agent_did`
- selected or default `behavior_id`
- placeholder title/preview fields appropriate for an empty conversation
- idle/open status acceptable to the current schema

- [ ] **Step 3: Return stable identifiers**

Return at least:

- `session_id`
- `agent_did`
- `behavior_id`

This allows T6 to switch the UI directly into the new conversation.

---

## Task 3: Implement request submission

**Files:**

- Modify: `crates/defra-agent-desktop/src/client/mutations.rs`

### Steps

- [ ] **Step 1: Mirror the existing request shape**

Use the current runtime/CLI request document shape as the source of truth. The mutation should create an `AgentRequest` with:

- generated `request_id`
- supplied `agent_did`
- session binding
- `status: "pending"`
- `lifecycle_state: "pending"`
- `admission_state: "released"`
- interactive execution origin
- retry lineage initialized to the request id

- [ ] **Step 2: Ensure the conversation summary updates**

After writing the request, update the linked `AgentConversation` so the sidebar immediately reflects:

- latest request id
- preview text
- active/non-terminal status

- [ ] **Step 3: Return the submitted request DTO**

Return at least:

- `request_id`
- `session_id`
- `agent_did`
- optional `behavior_id`

---

## Task 4: Implement peer management

**Files:**

- Modify: `crates/defra-agent-desktop/src/client/mutations.rs`
- Modify: `crates/defra-agent-desktop/src/client/peer_directory.rs`

### Steps

- [ ] **Step 1: Implement `add_peer`**

`add_peer` should:

- validate the peer address is non-empty
- persist the peer record in the peer directory
- call `connect_peer` through `defra-node::P2POps`
- surface success/failure as structured output

- [ ] **Step 2: Implement `remove_peer`**

MVP caveat: the public `P2POps` trait exposes `connect_peer` but not a disconnect operation.

So `remove_peer` should:

- remove the peer from the persisted directory
- stop surfacing it in the UI after the next reload
- leave the existing transport connection alone if one exists

Document this limitation in code comments and in the commit message.

---

## Task 5: Keep local read models coherent

**Files:**

- Modify: `crates/defra-agent-desktop/src/client/core.rs`
- Modify: `crates/defra-agent-desktop/src/app.rs`

### Steps

- [ ] **Step 1: Trigger observation refresh after successful mutations**

After successful writes, either:

- force a local snapshot refresh, or
- rely on the event subscription and wait briefly for the next version tick

Pick one explicit strategy and keep it consistent.

- [ ] **Step 2: Surface mutation errors to the shell**

Expose errors in a form the upcoming Chat UI can render inline rather than logging and dropping them.

---

## Task 6: Verify the submission API

**Files:**

- Create: `crates/defra-agent-desktop/tests/submission_api.rs`

### Steps

- [ ] **Step 1: Add integration tests**

Cover:

- creating a conversation writes `AgentSession` and `AgentConversation`
- submitting a request writes `AgentRequest` and updates the conversation summary
- adding a peer persists the peer record
- removing a peer deletes the peer record

- [ ] **Step 2: Compile and test**

Run:

- `cargo check -p defra-agent-desktop`
- `cargo test -p defra-agent-desktop submission_api`

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent-desktop/
git commit -m "Add defra-agent-desktop submission API"
```
