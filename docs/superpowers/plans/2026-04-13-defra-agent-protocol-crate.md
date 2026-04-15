# defra-agent-protocol Crate Extraction (T1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract a new `defra-agent-protocol` crate from `defra-agent` containing GraphQL schema strings, the client turn-observation protocol, and serde row mirrors for every replicated collection. The runtime keeps its existing surface via re-exports.

**Architecture:** The new crate is dependency-light — `serde` only — with no `defra-node`, no `tokio`, no `rig`. It is the shared substrate for any DefraDB peer participating in a `defra-agent` control plane (runtime, CLI, and forthcoming `defra-agent-desktop`). `defra-agent` re-exports `client_protocol` and keeps its `schema::ensure_schemas` / `schema::ensure_runtime_schemas` helpers, which now delegate their schema string arrays to `defra_agent_protocol::schemas::{ALL, RUNTIME_ALL}`.

**Tech Stack:** Rust 2021, `serde`, `serde_json` (dev), Cargo workspaces.

**Reference spec:** `docs/superpowers/specs/2026-04-13-desktop-dashboard-design.md` (T1 row of the ticket table).

---

## Execution environment

This plan runs on `main` directly — no worktree. Each task commits a coherent slice, and the final verification pushes to `origin/main` when the operator authorizes it.

---

## File Structure

**New files:**

- `crates/defra-agent-protocol/Cargo.toml`
- `crates/defra-agent-protocol/src/lib.rs` — declares `schemas`, `client_protocol`, `row` modules
- `crates/defra-agent-protocol/src/schemas.rs` — per-collection `&str` consts + `ALL` / `RUNTIME_ALL`
- `crates/defra-agent-protocol/src/client_protocol.rs` — turn derivation (moved from `defra-agent`)
- `crates/defra-agent-protocol/src/client_protocol/tests.rs` — conformance tests (moved)
- `crates/defra-agent-protocol/src/row.rs` — serde row mirrors for all 16 collections
- `crates/defra-agent-protocol/schemas/**/*.graphql` — the 16 schema files (moved from `defra-agent/schemas/`)

**Modified files:**

- `Cargo.toml` (workspace root) — add new member + path dep
- `crates/defra-agent/Cargo.toml` — add `defra-agent-protocol` dep
- `crates/defra-agent/src/lib.rs` — swap `pub mod client_protocol;` for `pub use defra_agent_protocol::client_protocol;`
- `crates/defra-agent/src/schema.rs` — delete local `include_str!` consts, import `ALL`/`RUNTIME_ALL` from `defra_agent_protocol::schemas`, keep `ensure_*` helpers

**Deleted files:**

- `crates/defra-agent/src/client_protocol.rs`
- `crates/defra-agent/src/client_protocol/tests.rs` (and the enclosing directory)
- `crates/defra-agent/schemas/` (the entire directory)

---

## Task 1: Bootstrap the `defra-agent-protocol` crate

**Files:**

- Create: `crates/defra-agent-protocol/Cargo.toml`
- Create: `crates/defra-agent-protocol/src/lib.rs`
- Create: `crates/defra-agent-protocol/src/schemas.rs` (stub)
- Create: `crates/defra-agent-protocol/src/client_protocol.rs` (stub)
- Create: `crates/defra-agent-protocol/src/row.rs` (stub)
- Modify: `Cargo.toml` (workspace root)

### Steps

- [ ] **Step 1: Create the crate manifest**

Create `crates/defra-agent-protocol/Cargo.toml`:

```toml
[package]
name = "defra-agent-protocol"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Shared schemas, client turn-observation protocol, and collection row mirrors for defra-agent"

[dependencies]
serde = { workspace = true, features = ["derive"] }

[dev-dependencies]
serde_json = { workspace = true }
```

- [ ] **Step 2: Create the module stubs**

Create `crates/defra-agent-protocol/src/lib.rs`:

```rust
//! Shared substrate for any DefraDB peer participating in a `defra-agent`
//! control plane: GraphQL schema strings, client turn-observation protocol,
//! and serde row mirrors for every replicated collection.

pub mod client_protocol;
pub mod row;
pub mod schemas;
```

Create `crates/defra-agent-protocol/src/schemas.rs`:

```rust
//! Static GraphQL schema strings for every replicated collection.
//! (Populated in Task 2.)
```

Create `crates/defra-agent-protocol/src/client_protocol.rs`:

```rust
//! Client turn-observation protocol.
//! (Populated in Task 3.)
```

Create `crates/defra-agent-protocol/src/row.rs`:

```rust
//! Serde mirrors for replicated collection rows.
//! (Populated in Task 4.)
```

- [ ] **Step 3: Add the crate to the workspace**

Modify `Cargo.toml` (workspace root): add `"crates/defra-agent-protocol"` to the `members` array so it reads:

```toml
[workspace]
resolver = "2"
members = [
    "crates/defra-agent",
    "crates/defra-agent-cli",
    "crates/defra-agent-protocol",
]
```

- [ ] **Step 4: Verify the crate compiles**

Run: `cargo check -p defra-agent-protocol`
Expected: clean compile with no warnings.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/defra-agent-protocol/
git commit -m "Add defra-agent-protocol crate skeleton"
```

---

## Task 2: Move GraphQL schemas into the protocol crate

**Files:**

- Copy: `crates/defra-agent/schemas/**` → `crates/defra-agent-protocol/schemas/**`
- Modify: `crates/defra-agent-protocol/src/schemas.rs`

(The old `crates/defra-agent/schemas/` stays until Task 7 — we do not break the runtime yet.)

### Steps

- [ ] **Step 1: Copy the schemas directory**

```bash
cp -R crates/defra-agent/schemas crates/defra-agent-protocol/schemas
```

Verify:

```bash
find crates/defra-agent-protocol/schemas -name "*.graphql" | wc -l
```

Expected: `16`.

- [ ] **Step 2: Write the failing tests**

Append to `crates/defra-agent-protocol/src/schemas.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_contains_every_schema() {
        assert_eq!(ALL.len(), 15, "ALL should enumerate every non-runtime schema");
    }

    #[test]
    fn every_schema_starts_with_type_declaration() {
        for sdl in ALL.iter().chain(RUNTIME_ALL.iter()) {
            assert!(
                sdl.trim_start().starts_with("type "),
                "schema must begin with `type`: {}",
                sdl.lines().next().unwrap_or("")
            );
        }
    }
}
```

- [ ] **Step 3: Run the tests and confirm they fail**

Run: `cargo test -p defra-agent-protocol schemas`
Expected: compile error — `ALL` and `RUNTIME_ALL` are undefined.

- [ ] **Step 4: Implement `schemas.rs`**

Replace `crates/defra-agent-protocol/src/schemas.rs` with the complete content below:

```rust
//! Static GraphQL schema strings for every replicated collection.
//!
//! Schema files are `include_str!`-compiled into the binary so that runtime
//! nodes and client peers register identical collection schemas without
//! pulling the files in at startup. `ALL` lists the deployment schemas in
//! registration order; `RUNTIME_ALL` lists the schemas that must be
//! registered before runtime reconciliation can begin.

// ── agent domain ────────────────────────────────────────────────
pub const AGENT_PRINCIPAL: &str = include_str!("../schemas/agent/agent_principal.graphql");
pub const AGENT_BEHAVIOR: &str = include_str!("../schemas/agent/agent_behavior.graphql");
pub const AGENT_RUNTIME: &str = include_str!("../schemas/agent/agent_runtime.graphql");
pub const AGENT_CONVERSATION: &str = include_str!("../schemas/agent/agent_conversation.graphql");
pub const AGENT_REQUEST: &str = include_str!("../schemas/agent/agent_request.graphql");
pub const AGENT_RESPONSE: &str = include_str!("../schemas/agent/agent_response.graphql");
pub const AGENT_MESSAGE: &str = include_str!("../schemas/agent/agent_message.graphql");
pub const AGENT_SESSION: &str = include_str!("../schemas/agent/agent_session.graphql");
pub const AGENT_TOOL_CALL: &str = include_str!("../schemas/agent/agent_tool_call.graphql");
pub const AGENT_TOOL_RESULT: &str = include_str!("../schemas/agent/agent_tool_result.graphql");
pub const COMPACTION_ENTRY: &str = include_str!("../schemas/agent/compaction_entry.graphql");
pub const SCHEDULED_TASK: &str = include_str!("../schemas/agent/scheduled_task.graphql");
pub const TOOL_SELECTION: &str = include_str!("../schemas/agent/tool_selection.graphql");

// ── inference domain ────────────────────────────────────────────
pub const INFERENCE_BACKEND: &str = include_str!("../schemas/inference/inference_backend.graphql");
pub const INFERENCE_PROFILE: &str = include_str!("../schemas/inference/inference_profile.graphql");

// ── services domain ─────────────────────────────────────────────
pub const TOOL_SERVICE_REGISTRY: &str =
    include_str!("../schemas/services/tool_service_registry.graphql");

/// Schemas that must be registered before the runtime can start reconciling.
/// Mirrors the legacy `defra_agent::schema::RUNTIME_ALL`.
pub const RUNTIME_ALL: &[&str] = &[INFERENCE_BACKEND];

/// Every schema required by a full agent deployment. Registration order
/// matches the legacy `defra_agent::schema::ALL`.
pub const ALL: &[&str] = &[
    AGENT_PRINCIPAL,
    AGENT_BEHAVIOR,
    AGENT_RUNTIME,
    TOOL_SELECTION,
    INFERENCE_PROFILE,
    AGENT_CONVERSATION,
    AGENT_REQUEST,
    AGENT_RESPONSE,
    AGENT_TOOL_RESULT,
    AGENT_SESSION,
    AGENT_MESSAGE,
    AGENT_TOOL_CALL,
    COMPACTION_ENTRY,
    SCHEDULED_TASK,
    TOOL_SERVICE_REGISTRY,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_contains_every_schema() {
        assert_eq!(ALL.len(), 15, "ALL should enumerate every non-runtime schema");
    }

    #[test]
    fn every_schema_starts_with_type_declaration() {
        for sdl in ALL.iter().chain(RUNTIME_ALL.iter()) {
            assert!(
                sdl.trim_start().starts_with("type "),
                "schema must begin with `type`: {}",
                sdl.lines().next().unwrap_or("")
            );
        }
    }
}
```

- [ ] **Step 5: Run tests and verify green**

Run: `cargo test -p defra-agent-protocol schemas`
Expected: both tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent-protocol/
git commit -m "Move GraphQL schemas to defra-agent-protocol"
```

---

## Task 3: Move the `client_protocol` module

**Files:**

- Copy: `crates/defra-agent/src/client_protocol.rs` → `crates/defra-agent-protocol/src/client_protocol.rs`
- Copy: `crates/defra-agent/src/client_protocol/tests.rs` → `crates/defra-agent-protocol/src/client_protocol/tests.rs`

(The source files in `defra-agent` remain intact until Task 5.)

### Steps

- [ ] **Step 1: Copy the module file**

```bash
cp crates/defra-agent/src/client_protocol.rs \
   crates/defra-agent-protocol/src/client_protocol.rs
```

- [ ] **Step 2: Copy the tests sub-module**

```bash
mkdir -p crates/defra-agent-protocol/src/client_protocol
cp crates/defra-agent/src/client_protocol/tests.rs \
   crates/defra-agent-protocol/src/client_protocol/tests.rs
```

- [ ] **Step 3: Verify the protocol crate compiles**

Run: `cargo check -p defra-agent-protocol`
Expected: clean compile.

- [ ] **Step 4: Run the ported conformance tests**

Run: `cargo test -p defra-agent-protocol client_protocol`
Expected: all conformance tests pass (identical behavior to the old location; same file contents, same assertions).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-protocol/src/client_protocol.rs \
        crates/defra-agent-protocol/src/client_protocol/
git commit -m "Move client_protocol to defra-agent-protocol"
```

---

## Task 4: Add serde row mirrors for every collection

**Files:**

- Modify: `crates/defra-agent-protocol/src/row.rs`

### Steps

- [ ] **Step 1: Write the failing tests**

Replace the stub contents of `crates/defra-agent-protocol/src/row.rs` with only the test module first (the implementation below will fail to compile — that is the "failing test"):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_request_row_roundtrips() {
        let json = r#"{
            "request_id": "req-1",
            "agent_did": "did:defra:amy",
            "behavior_id": "amy-code",
            "session_id": "s-1",
            "retry_parent_request": "",
            "retry_root_request": "req-1",
            "superseded_by_request": "",
            "content": "hello",
            "status": "pending",
            "lifecycle_state": "pending",
            "backend_id": "",
            "execution_origin": "interactive",
            "failure_reason": "",
            "created_at": "2026-04-13T12:00:00Z",
            "retry_count": 0,
            "max_retries": 3
        }"#;
        let row: AgentRequestRow = serde_json::from_str(json).expect("parse");
        assert_eq!(row.request_id, "req-1");
        assert_eq!(row.retry_count, Some(0));
        let re: String = serde_json::to_string(&row).expect("serialize");
        let round: AgentRequestRow = serde_json::from_str(&re).expect("reparse");
        assert_eq!(row, round);
    }

    #[test]
    fn tool_selection_row_handles_missing_arrays() {
        let json = r#"{
            "selection_id": "sel-1",
            "agent_did": "did:defra:amy",
            "display_name": "tools-engineering",
            "enable_file_tools": true,
            "file_tools_mode": "read",
            "enable_bash": false,
            "bash_mode": "deny",
            "enable_meta_tools": true
        }"#;
        let row: ToolSelectionRow = serde_json::from_str(json).expect("parse");
        assert!(row.cli_tool_names.is_empty());
        assert!(row.delegate_to.is_empty());
    }
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p defra-agent-protocol row`
Expected: compile error — `AgentRequestRow` and `ToolSelectionRow` are undefined.

- [ ] **Step 3: Write the full `row.rs` implementation**

Replace `crates/defra-agent-protocol/src/row.rs` with the complete file below (paste as-is, including the test module at the bottom):

```rust
//! Serde mirrors for replicated collection rows.
//!
//! These types are deliberately permissive: most scalar fields are wrapped
//! in `Option<T>` because DefraDB may omit unpopulated fields from GraphQL
//! responses, and list fields use `#[serde(default)]` so missing arrays
//! deserialize as empty vectors. Callers should treat these as the wire
//! shape, not a runtime invariant.

use serde::{Deserialize, Serialize};

// ── agent domain ────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentPrincipalRow {
    pub agent_did: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub default_behavior_id: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentBehaviorRow {
    pub behavior_id: String,
    pub agent_did: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub backend_id: Option<String>,
    #[serde(default)]
    pub model_name: Option<String>,
    #[serde(default)]
    pub tool_selection_id: Option<String>,
    #[serde(default)]
    pub inference_profile_id: Option<String>,
    #[serde(default)]
    pub compaction_strategy: Option<String>,
    #[serde(default)]
    pub compaction_threshold: Option<f64>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRuntimeRow {
    pub agent_did: String,
    #[serde(default)]
    pub process_state: Option<String>,
    #[serde(default)]
    pub reconcile_phase: Option<String>,
    #[serde(default)]
    pub active_generation: Option<i64>,
    #[serde(default)]
    pub router_generation: Option<i64>,
    #[serde(default)]
    pub default_behavior_id: Option<String>,
    #[serde(default)]
    pub runnable_behavior_count: Option<i64>,
    #[serde(default)]
    pub unavailable_behavior_count: Option<i64>,
    #[serde(default)]
    pub last_reconcile_result: Option<String>,
    #[serde(default)]
    pub last_reconcile_error: Option<String>,
    #[serde(default)]
    pub last_reconcile_completed_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentConversationRow {
    pub session_id: String,
    #[serde(default)]
    pub agent_name: Option<String>,
    pub agent_did: String,
    #[serde(default)]
    pub behavior_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub preview_text: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub latest_request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRequestRow {
    pub request_id: String,
    pub agent_did: String,
    #[serde(default)]
    pub behavior_id: Option<String>,
    pub session_id: String,
    #[serde(default)]
    pub retry_parent_request: Option<String>,
    #[serde(default)]
    pub retry_root_request: Option<String>,
    #[serde(default)]
    pub superseded_by_request: Option<String>,
    pub content: String,
    pub status: String,
    pub lifecycle_state: String,
    #[serde(default)]
    pub backend_id: Option<String>,
    #[serde(default)]
    pub execution_origin: Option<String>,
    #[serde(default)]
    pub failure_reason: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub claimed_at: Option<String>,
    #[serde(default)]
    pub deadline: Option<String>,
    #[serde(default)]
    pub retry_count: Option<i64>,
    #[serde(default)]
    pub max_retries: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentResponseRow {
    pub response_key: String,
    pub request_id: String,
    pub agent_did: String,
    #[serde(default)]
    pub behavior_id: Option<String>,
    pub session_id: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    pub status: String,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub token_count: Option<i64>,
    #[serde(default)]
    pub progress_seq: Option<i64>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMessageRow {
    pub message_key: String,
    pub session_id: String,
    pub sequence: i64,
    pub role: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionRow {
    pub session_id: String,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub behavior_id: Option<String>,
    #[serde(default)]
    pub started: Option<String>,
    #[serde(default)]
    pub ended: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolCallRow {
    pub tool_call_key: String,
    pub session_id: String,
    #[serde(default)]
    pub message_sequence: Option<i64>,
    pub tool_name: String,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub args: Option<String>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolResultRow {
    pub agent_did: String,
    pub session_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Option<String>,
    #[serde(default)]
    pub output_text: Option<String>,
    #[serde(default)]
    pub truncated: Option<bool>,
    #[serde(default)]
    pub truncation_metadata: Option<String>,
    #[serde(default)]
    pub conversation_doc_id: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionEntryRow {
    pub compaction_key: String,
    pub session_id: String,
    pub sequence: i64,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub files_read: Option<String>,
    #[serde(default)]
    pub files_modified: Option<String>,
    #[serde(default)]
    pub messages_compacted: Option<i64>,
    #[serde(default)]
    pub original_tokens: Option<i64>,
    #[serde(default)]
    pub compacted_tokens: Option<i64>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduledTaskRow {
    pub task_id: String,
    pub agent_did: String,
    #[serde(default)]
    pub behavior_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub interval_secs: Option<i64>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub next_run_at: Option<String>,
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub run_count: Option<i64>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSelectionRow {
    pub selection_id: String,
    pub agent_did: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub enable_file_tools: Option<bool>,
    #[serde(default)]
    pub file_tools_mode: Option<String>,
    #[serde(default)]
    pub enable_bash: Option<bool>,
    #[serde(default)]
    pub bash_mode: Option<String>,
    #[serde(default)]
    pub cli_tool_names: Vec<String>,
    #[serde(default)]
    pub enable_meta_tools: Option<bool>,
    #[serde(default)]
    pub delegate_to: Vec<String>,
}

// ── inference domain ────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferenceBackendRow {
    pub backend_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub provider_kind: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_env_var: Option<String>,
    #[serde(default)]
    pub max_concurrent: Option<i64>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub supports_tool_calls: Option<bool>,
    #[serde(default)]
    pub supports_streaming: Option<bool>,
    #[serde(default)]
    pub supports_structured_outputs: Option<bool>,
    #[serde(default)]
    pub supports_json_schema: Option<bool>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub last_probe: Option<String>,
    #[serde(default)]
    pub probe_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferenceProfileRow {
    pub profile_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub context_window: Option<i64>,
    #[serde(default)]
    pub max_output_tokens: Option<i64>,
    #[serde(default)]
    pub max_turns: Option<i64>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub stream_batch_ms: Option<i64>,
    #[serde(default)]
    pub deadline_duration_secs: Option<i64>,
}

// ── services domain ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolServiceEntry {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolServiceRegistryRow {
    pub service_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub tailscale_ip: Option<String>,
    #[serde(default)]
    pub lan_ip: Option<String>,
    #[serde(default)]
    pub mcp_port: Option<i64>,
    #[serde(default)]
    pub mcp_path: Option<String>,
    #[serde(default)]
    pub tools: Vec<ToolServiceEntry>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_request_row_roundtrips() {
        let json = r#"{
            "request_id": "req-1",
            "agent_did": "did:defra:amy",
            "behavior_id": "amy-code",
            "session_id": "s-1",
            "retry_parent_request": "",
            "retry_root_request": "req-1",
            "superseded_by_request": "",
            "content": "hello",
            "status": "pending",
            "lifecycle_state": "pending",
            "backend_id": "",
            "execution_origin": "interactive",
            "failure_reason": "",
            "created_at": "2026-04-13T12:00:00Z",
            "retry_count": 0,
            "max_retries": 3
        }"#;
        let row: AgentRequestRow = serde_json::from_str(json).expect("parse");
        assert_eq!(row.request_id, "req-1");
        assert_eq!(row.retry_count, Some(0));
        let re: String = serde_json::to_string(&row).expect("serialize");
        let round: AgentRequestRow = serde_json::from_str(&re).expect("reparse");
        assert_eq!(row, round);
    }

    #[test]
    fn tool_selection_row_handles_missing_arrays() {
        let json = r#"{
            "selection_id": "sel-1",
            "agent_did": "did:defra:amy",
            "display_name": "tools-engineering",
            "enable_file_tools": true,
            "file_tools_mode": "read",
            "enable_bash": false,
            "bash_mode": "deny",
            "enable_meta_tools": true
        }"#;
        let row: ToolSelectionRow = serde_json::from_str(json).expect("parse");
        assert!(row.cli_tool_names.is_empty());
        assert!(row.delegate_to.is_empty());
    }
}
```

- [ ] **Step 4: Run tests to verify green**

Run: `cargo test -p defra-agent-protocol`
Expected: all tests pass (schemas + client_protocol + row).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-protocol/src/row.rs
git commit -m "Add collection row mirrors to defra-agent-protocol"
```

---

## Task 5: Wire `defra-agent` to consume the new crate

**Files:**

- Modify: `Cargo.toml` (workspace root, `[workspace.dependencies]`)
- Modify: `crates/defra-agent/Cargo.toml`
- Modify: `crates/defra-agent/src/lib.rs`
- Delete: `crates/defra-agent/src/client_protocol.rs`
- Delete: `crates/defra-agent/src/client_protocol/` (directory)

### Steps

- [ ] **Step 1: Register the workspace dependency**

Modify `Cargo.toml` (workspace root) `[workspace.dependencies]` table: add

```toml
defra-agent-protocol = { path = "crates/defra-agent-protocol" }
```

- [ ] **Step 2: Pull the dependency into `defra-agent`**

Modify `crates/defra-agent/Cargo.toml` under `[dependencies]`: add

```toml
defra-agent-protocol.workspace = true
```

- [ ] **Step 3: Re-export `client_protocol` from `defra-agent`**

Modify `crates/defra-agent/src/lib.rs`: replace

```rust
pub mod client_protocol;
```

with

```rust
pub use defra_agent_protocol::client_protocol;
```

- [ ] **Step 4: Delete the old `client_protocol` files**

```bash
git rm crates/defra-agent/src/client_protocol.rs
git rm -r crates/defra-agent/src/client_protocol
```

- [ ] **Step 5: Verify defra-agent compiles**

Run: `cargo check -p defra-agent`
Expected: clean compile.

- [ ] **Step 6: Verify defra-agent tests pass**

Run: `cargo test -p defra-agent`
Expected: all tests pass. (Conformance tests now execute as part of `defra-agent-protocol`, not `defra-agent`.)

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/defra-agent/Cargo.toml crates/defra-agent/src/lib.rs
git commit -m "Wire defra-agent to defra-agent-protocol client_protocol"
```

---

## Task 6: Switch `defra-agent::schema` to use protocol crate's schemas

**Files:**

- Modify: `crates/defra-agent/src/schema.rs`

### Steps

- [ ] **Step 1: Replace `schema.rs` contents**

Replace `crates/defra-agent/src/schema.rs` with:

```rust
//! Runtime-side schema registration helpers.
//!
//! Schema strings are the canonical exports of `defra_agent_protocol::schemas`.
//! This module wires them to an `EmbeddedNode` via `ensure_schemas` and
//! `ensure_runtime_schemas`.

use anyhow::Result;
use defra_agent_protocol::schemas::{ALL, RUNTIME_ALL};
use defra_node::EmbeddedNode;

async fn ensure_schema_set(node: &EmbeddedNode, schemas: &[&str]) -> Result<()> {
    for sdl in schemas {
        match node.add_schema(sdl).await {
            Ok(()) => {}
            Err(error) => {
                if error.to_string().contains("already exists") {
                    tracing::debug!(
                        schema = %sdl.lines().next().unwrap_or(""),
                        "schema already exists"
                    );
                } else {
                    return Err(error);
                }
            }
        }
    }

    Ok(())
}

pub async fn ensure_runtime_schemas(node: &EmbeddedNode) -> Result<()> {
    ensure_schema_set(node, RUNTIME_ALL).await?;
    ensure_schemas(node).await
}

pub async fn ensure_schemas(node: &EmbeddedNode) -> Result<()> {
    ensure_schema_set(node, ALL).await
}
```

- [ ] **Step 2: Verify defra-agent compiles**

Run: `cargo check -p defra-agent`
Expected: clean compile. Internal callers `crate::schema::ensure_schemas` (in `oneshot.rs`) and `crate::schema::ensure_runtime_schemas` (in `streaming/tests.rs`) continue to work — same signatures, same module path.

- [ ] **Step 3: Verify full workspace test suite**

Run: `cargo test --workspace`
Expected: all tests pass across `defra-agent`, `defra-agent-cli`, and `defra-agent-protocol`.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/schema.rs
git commit -m "Wire defra-agent::schema to defra-agent-protocol schemas"
```

---

## Task 7: Delete the old schemas directory and verify clean build

**Files:**

- Delete: `crates/defra-agent/schemas/` (entire directory)

### Steps

- [ ] **Step 1: Confirm no code still references the old path**

Run: `git grep -n '"\\.\\./schemas/'` (inside the worktree)
Expected: no matches. (If any appear, stop and fix — something still `include_str!`-imports the old location.)

Also run: `git grep -n 'crates/defra-agent/schemas'`
Expected: no matches in code (matches inside `docs/` are fine, those are historical references in the spec).

- [ ] **Step 2: Delete the old directory**

```bash
git rm -r crates/defra-agent/schemas
```

- [ ] **Step 3: Full workspace check**

Run: `cargo check --workspace`
Expected: clean compile.

- [ ] **Step 4: Full workspace tests**

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git commit -m "Remove duplicate schemas directory from defra-agent"
```

---

## Final verification

- [ ] **Step 1: Working tree is clean**

Run: `git status`
Expected: `nothing to commit, working tree clean`.

- [ ] **Step 2: Every test passes**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 3: Lints are clean**

Run: `cargo clippy --workspace -- -D warnings`
Expected: no warnings.

- [ ] **Step 4: Protocol crate dependency audit**

Run: `cargo tree -p defra-agent-protocol --depth 1`
Expected: `defra-agent-protocol` depends only on `serde` (and `serde_json` as a dev-dep). No `defra-node`, `tokio`, `rig`, or `iroh` in the tree. If any heavier dep has crept in, investigate before merging.

- [ ] **Step 5: Push to origin (operator-authorized)**

When tests are green and clippy is clean, confirm with the operator and push:

```bash
git push origin main
```

Since the work lands on `main`, there is no PR to open. The commits are now public history.
