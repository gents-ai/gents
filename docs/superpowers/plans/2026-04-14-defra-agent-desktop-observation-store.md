# defra-agent-desktop Observation Store (T4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Materialize a desktop-side `ClientStore` from replicated DefraDB documents, refresh it in the background from `EventName::Update`, and expose a `tokio::sync::watch` change signal that lets egui repaint from immutable snapshots.

**Architecture:** The store is a desktop-local read model built from `defra-agent-protocol::row` types. A background observer task owns refresh and indexing; UI code only reads snapshots. The implementation should remain pragmatic: correctness and a stable snapshot API matter more than micro-optimizing reloads in the first pass.

**Tech Stack:** Rust 2021, `defra-agent-protocol`, `defra-node`, `events`, `tokio`, `serde_json`, `chrono`, `tracing`.

**Reference spec:** `docs/superpowers/specs/2026-04-13-desktop-dashboard-design.md` (T4 row, observation model, observation pipeline, Chat activity, and post-MVP focused-request note).

---

## Execution environment

This ticket starts consuming the replicated collections. It should stay read-only: no mutations beyond the client-core bootstrap already introduced by T3.

---

## File Structure

**New files:**

- `crates/defra-agent-desktop/src/client/query.rs`
- `crates/defra-agent-desktop/src/client/store.rs`
- `crates/defra-agent-desktop/src/client/observe.rs`
- `crates/defra-agent-desktop/tests/client_store.rs`

**Modified files:**

- `crates/defra-agent-desktop/src/client/mod.rs`
- `crates/defra-agent-desktop/src/client/core.rs`
- `crates/defra-agent-desktop/src/app.rs`
- `crates/defra-agent-desktop/src/state.rs`

---

## Task 1: Define the store snapshot model

**Files:**

- Create: `crates/defra-agent-desktop/src/client/store.rs`

### Steps

- [ ] **Step 1: Define the raw snapshot contents**

`ClientStore` should hold the replicated collections needed by MVP, at minimum:

- agent principals
- behaviors
- runtimes
- conversations
- requests
- responses
- messages
- tool calls
- tool results
- sessions
- scheduled tasks
- tool selections
- inference backends
- inference profiles
- tool service registry entries

Keep the rows in protocol-crate wire shapes rather than inventing a second serialization layer.

- [ ] **Step 2: Add derived indexes**

Build indexes needed by later views, for example:

- conversations by `agent_did`
- messages by `session_id`
- tool calls by `session_id`
- latest response by `request_id`
- requests by `session_id`
- runtimes by `agent_did`

- [ ] **Step 3: Reserve cross-view focus state**

Include `focused_request_id: Option<String>` in the store handle or top-level state now, even if P1 is the first visible consumer. The higher-level design explicitly calls this out.

- [ ] **Step 4: Add pure helper APIs**

Expose read helpers such as:

- `derive_turn(session_id) -> Option<ClientTurnState>`
- `conversation_rows(agent_did) -> &[AgentConversationRow]` or iterator equivalent
- `transcript(session_id)` view helper
- `latest_runtime(agent_did)`

Turn derivation must call `defra_agent_protocol::client_protocol::derive_turn`; do not re-implement the state machine in the desktop crate.

---

## Task 2: Implement snapshot loading queries

**Files:**

- Create: `crates/defra-agent-desktop/src/client/query.rs`

### Steps

- [ ] **Step 1: Write collection query helpers**

Add query functions that load each collection using `EmbeddedNode::execute` and deserialize into the protocol row types.

Keep the GraphQL field lists explicit and stable. Prefer one helper per collection or per closely-related collection family.

- [ ] **Step 2: Implement `load_full_snapshot`**

Add a snapshot bootstrap function that queries every replicated collection needed by MVP and returns a fully-indexed `ClientStore`.

- [ ] **Step 3: Document the refresh strategy**

First implementation rule:

- correctness first
- full snapshot reload on any update is acceptable if it keeps the code straightforward

If implementation proves simple enough to reload only affected collections, that is fine, but the ticket should not block on perfect incremental invalidation.

---

## Task 3: Implement the observation loop

**Files:**

- Create: `crates/defra-agent-desktop/src/client/observe.rs`
- Modify: `crates/defra-agent-desktop/src/client/core.rs`

### Steps

- [ ] **Step 1: Add a background observer task**

Use `node.subscribe(&[EventName::Update])` and a background Tokio task that:

- loads an initial snapshot
- waits for update events
- debounces bursts
- refreshes the store

- [ ] **Step 2: Add watch-based change signaling**

Expose a `tokio::sync::watch::Receiver<u64>` or similar monotonically increasing version signal that updates every time a new store snapshot lands.

- [ ] **Step 3: Handle dropped events safely**

If the subscription reports dropped messages, force a full snapshot reload and log the condition. The desktop is an observer; correctness beats trying to recover incrementally from missing updates.

- [ ] **Step 4: Keep the UI-thread contract simple**

The egui layer should only need:

- a way to borrow or clone the current `ClientStore` snapshot
- a watch receiver for repaint triggers

Do not make view code await or execute GraphQL directly.

---

## Task 4: Wire repaint triggers into the shell

**Files:**

- Modify: `crates/defra-agent-desktop/src/app.rs`

### Steps

- [ ] **Step 1: Attach the store handle to the app**

`DesktopApp` should receive:

- the current snapshot handle
- the watch receiver

- [ ] **Step 2: Request repaints from store updates**

When the watch receiver version changes, call `ctx.request_repaint()`.

- [ ] **Step 3: Replace placeholder counts where trivial**

Swap the T2 fake counts for real values where T4 already has the data cheaply, for example:

- peer count in the status bar
- selected principal DID
- number of observed agents/conversations if a placeholder principal is selected

Do not build the full Chat UI yet; T6 owns that.

---

## Task 5: Verify the observation layer

**Files:**

- Create: `crates/defra-agent-desktop/tests/client_store.rs`

### Steps

- [ ] **Step 1: Add store tests**

Cover at least:

- snapshot indexing
- request-chain turn derivation
- `focused_request_id` default behavior

- [ ] **Step 2: Add observer integration tests**

Use temp embedded nodes and direct GraphQL mutations to prove that:

- initial snapshot load sees existing rows
- an `EventName::Update` causes a refreshed store
- the watch receiver ticks when the store changes

- [ ] **Step 3: Compile and test**

Run:

- `cargo check -p defra-agent-desktop`
- `cargo test -p defra-agent-desktop client_store`

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-desktop/
git commit -m "Add defra-agent-desktop observation store"
```
