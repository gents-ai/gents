# defra-agent-desktop Client Core (T3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the non-UI desktop client core: boot an embedded `defra-node`, register the shared schemas, load or create the desktop principal identity, persist a peer directory, and dial peers through the public `defra-node::P2POps` API.

**Architecture:** The desktop shell from T2 gains a long-lived `ClientCore` owned behind an `Arc`. The client core is responsible only for process bootstrap and transport lifecycle in this ticket; it does **not** materialize replicated documents yet. `PrincipalIdentity` and `PeerDirectory` stay desktop-local, while the actual database and P2P transport stay inside `EmbeddedNode`.

**Tech Stack:** Rust 2021, `defra-agent-protocol`, `defra-node`, `events`, `identity`, `crypto`, `tokio`, `serde`, `serde_json`, `dirs`, `tracing`.

**Reference spec:** `docs/superpowers/specs/2026-04-13-desktop-dashboard-design.md` (T3 row, runtime topology, crate layout, and first-launch sections). Public API reference confirmed in local dependency source: `defra-node::P2POps::{listen_addresses, connected_peers, connect_peer, subscribe_collection, set_replicator}`.

---

## Execution environment

This ticket extends the desktop crate and makes one small follow-up addition to `defra-agent-protocol`: the desktop needs exported collection names in addition to schema SDL strings.

---

## File Structure

**New files:**

- `crates/defra-agent-desktop/src/client/mod.rs`
- `crates/defra-agent-desktop/src/client/core.rs`
- `crates/defra-agent-desktop/src/client/paths.rs`
- `crates/defra-agent-desktop/src/client/principal_identity.rs`
- `crates/defra-agent-desktop/src/client/peer_directory.rs`
- `crates/defra-agent-desktop/src/client/schema.rs`
- `crates/defra-agent-desktop/tests/client_core.rs`

**Modified files:**

- `Cargo.toml` - add shared desktop/runtime dependencies
- `crates/defra-agent-protocol/src/schemas.rs` - export replicated collection names
- `crates/defra-agent-desktop/Cargo.toml`
- `crates/defra-agent-desktop/src/app.rs`
- `crates/defra-agent-desktop/src/main.rs`
- `Cargo.lock`

---

## Task 1: Export collection names from the protocol crate

**Files:**

- Modify: `crates/defra-agent-protocol/src/schemas.rs`

### Steps

- [ ] **Step 1: Add collection-name constants**

Next to the existing SDL constants, export string constants for the replicated collection names, for example:

- `AGENT_PRINCIPAL_NAME`
- `AGENT_REQUEST_NAME`
- `TOOL_SERVICE_REGISTRY_NAME`

- [ ] **Step 2: Export ordered name arrays**

Add:

- `pub const ALL_COLLECTION_NAMES: &[&str]`
- `pub const RUNTIME_COLLECTION_NAMES: &[&str]`

Keep their ordering aligned with the SDL arrays.

- [ ] **Step 3: Add invariants tests**

Add small tests asserting:

- the name arrays and SDL arrays have matching lengths
- the names are unique

This keeps the desktop bootstrap from depending on a hand-maintained duplicate list.

---

## Task 2: Add desktop-local paths and principal identity

**Files:**

- Create: `crates/defra-agent-desktop/src/client/paths.rs`
- Create: `crates/defra-agent-desktop/src/client/principal_identity.rs`

### Steps

- [ ] **Step 1: Implement `DesktopPaths`**

Create a small path resolver that provides:

- app root directory
- node data directory
- peer directory file
- principal metadata file
- identity key path

Use a standard per-user app location via `dirs`, not a bespoke repo-relative path.

- [ ] **Step 2: Implement principal persistence**

Create `PrincipalIdentity` that:

- loads a persisted principal record if present
- otherwise generates a new identity on first launch
- stores enough metadata to reconstruct the same DID on later launches

Do not add a dependency on `defra-agent`; copy only the narrow `RawIdentity` load/create logic needed from `crates/defra-agent/src/identity.rs`.

- [ ] **Step 3: Add identity tests**

Add tests covering:

- first launch creates the key/material files
- second launch reuses the same DID and key bytes

---

## Task 3: Add peer directory persistence

**Files:**

- Create: `crates/defra-agent-desktop/src/client/peer_directory.rs`

### Steps

- [ ] **Step 1: Define the stored record shape**

Persist one record per deployment with at least:

- stable local `peer_id`
- operator label
- peer address or ticket string
- selected `agent_did`
- timestamps for created/updated

Keep it JSON-backed for MVP.

- [ ] **Step 2: Implement load/save helpers**

Add helpers to:

- load an empty directory when the file is absent
- save atomically
- upsert by `peer_id`
- remove by `peer_id`

- [ ] **Step 3: Add roundtrip tests**

Cover:

- empty file behavior
- add/update/remove persistence
- stable ordering for deterministic UI output

---

## Task 4: Boot the embedded node and wire peer dialing

**Files:**

- Create: `crates/defra-agent-desktop/src/client/core.rs`
- Create: `crates/defra-agent-desktop/src/client/schema.rs`
- Create: `crates/defra-agent-desktop/src/client/mod.rs`

### Steps

- [ ] **Step 1: Create desktop schema-registration helpers**

Implement local `ensure_schemas` / `ensure_runtime_schemas` helpers in the desktop crate that mirror the runtime logic but depend only on:

- `defra-agent-protocol::schemas::{ALL, RUNTIME_ALL}`
- `defra-node::EmbeddedNode`

Do not depend on `defra-agent` for this.

- [ ] **Step 2: Create `ClientCore::start`**

`ClientCore::start` should:

- resolve `DesktopPaths`
- load or create `PrincipalIdentity`
- create the embedded node data directory
- build `EmbeddedNode` with P2P enabled
- register runtime and deployment schemas
- load the peer directory

- [ ] **Step 3: Subscribe the local node to all replicated collections**

Use `node.p2p()` plus `ALL_COLLECTION_NAMES` to subscribe the local node to every replicated collection needed by the dashboard.

This is the desktop-side precondition for receiving the control-plane data it renders.

- [ ] **Step 4: Dial saved peers**

For each peer record:

- call `connect_peer(&peer.addr)`
- capture success/failure in in-memory status on startup

Do not block the entire app on one failed peer; start degraded and surface the failure in logs/UI later.

- [ ] **Step 5: Decide whether explicit replicator setup is required**

After `connect_peer` and `subscribe_collection` are wired, add a verification step:

- if desktop-originated writes replicate correctly in tests, stop there
- if not, add `set_replicator(peer_addr, ALL_COLLECTION_NAMES.to_vec())` during peer bootstrap

Keep this decision explicit in the implementation, not implicit tribal knowledge.

---

## Task 5: Surface bootstrap state into the shell

**Files:**

- Modify: `crates/defra-agent-desktop/src/app.rs`
- Modify: `crates/defra-agent-desktop/src/main.rs`

### Steps

- [ ] **Step 1: Initialize a Tokio runtime for background-capable desktop work**

Update the app bootstrap so the desktop can start async client tasks without blocking egui.

- [ ] **Step 2: Attach `ClientCore` to the app**

Store the started core behind an `Arc`, and show at least:

- principal DID in the identity chip
- number of configured peers in the status bar
- bootstrap errors in a visible shell-safe location

This is still pre-observation; use only startup metadata that T3 owns.

---

## Task 6: Verify the client core

### Steps

- [ ] **Step 1: Add integration tests**

Create `crates/defra-agent-desktop/tests/client_core.rs` covering:

- identity persistence roundtrip
- peer directory roundtrip
- embedded node startup and schema registration
- two-node P2P connection smoke test using `connect_peer`

- [ ] **Step 2: Compile**

Run: `cargo check -p defra-agent-desktop`

- [ ] **Step 3: Run tests**

Run: `cargo test -p defra-agent-desktop client_core`

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/defra-agent-protocol/src/schemas.rs crates/defra-agent-desktop/
git commit -m "Add defra-agent-desktop client core"
```
