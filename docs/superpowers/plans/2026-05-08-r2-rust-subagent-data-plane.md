# R2 — Rust Subagent Data Plane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the Rust runtime data plane defined in `docs/superpowers/specs/2026-05-08-r2-rust-subagent-data-plane-design.md` — `ToolCallLifecycle` extensions for subagent invocation (mode flips, bridge transitions, child-link field), schema migrations v2→v3 across three collections, and three conformance buckets.

**Architecture:** Mirror R1's data plane discipline (PR #152's `ToolCallLifecycle`). Extend the same struct with three new fields (`await_mode`, `cancel_policy`, `child_request_id`) and six new transition methods (`background`, `foreground`, `detach`, `bridge_complete`, `bridge_failure`, `bridge_cancel_cascade`). Single unified WASM lens crate covers AgentToolCall + AgentRequest + ToolSelection migrations. SubagentSource and agent-facing tools are deferred to R3+.

**Tech Stack:** Rust (edition 2021), Lean 4, Tokio, DefraDB embedded node, `lens_sdk` (WASM), `cargo` workspace.

---

## Conventions

- **Build/verify commands:**
  - Rust check: `cargo check -p defra-agent`
  - Rust lib tests: `cargo test -p defra-agent --lib`
  - Rust integration tests: `cargo test -p defra-agent --test <name>`
  - Lean: `cd crates/defra-agent/proofs && lake build`
  - WASM lens build: `cd crates/defra-agent-lenses/agent_subagent_v2_to_v3 && cargo build --release --target wasm32-unknown-unknown`
- **TDD cadence:** failing test → confirm fail → minimal impl → confirm pass → commit. One commit per task.
- **Working directory:** all paths relative to repo root `/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-design-subagent-management`.
- **Branch:** `design/subagent-management` (currently at HEAD `7bde2e5` — R2 spec revisions). Do NOT switch branches.
- **Commit messages:** imperative, scoped, end with `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer. Use HEREDOC for multi-line bodies.
- **DefraDB Kind codes** (verified against R1's `migration.rs:17`):
  - `Kind: 11` → String
  - `Kind: 5` → Int
  - `Kind: 2` → Boolean
  - `Kind: 17` → `[String]` (list of strings)
  
  If any of these turn out wrong at implementation time, find the correct code by reading the `defradb.rs` crate's schema definitions (likely under `~/.cargo/git/checkouts/defradb-rs-*/`) or by examining how existing schemas like `command_allowed_argv_prefixes: [String]` are persisted. Pin the corrected codes inline in `migration.rs` before proceeding.

---

## What's NOT in this plan (deferred per spec)

- **R3** — `SubagentSource` (TriggerSource implementation), daemon interrupt dispatcher consuming `CascadeIntent`, spawn-time invariant enforcement, cross-reference validation (`subagent_targets` resolution, `caused_by_parent_request_id` existence).
- **R4** — Agent-facing tools (`spawn_subagent`, `wait_task`, `get_task_result`, `cancel_task`, `read_subagent_transcript`, `send_message_to_subagent`, `list_tasks`, `background_task`); hook integration that routes them.
- **R5** — Apply-time cross-reference validation; conformance Bucket 4 (multi-flight stress).
- **R6 / sourcenetwork/defra-agent#9** — Cross-principal delegation.
- **Future** (no R-phase yet) — token/cost budget propagation, output streaming, detach orphan reaper, subagent retry semantics, persistent subagents across daemon restarts, runtime backfill of `request_id` for historic AgentToolCall rows.

---

## Task 0: Extend Lean conformance contract (`Machines.lean`)

Buckets 1 and 2 depend on Lean emitting the new vocabularies and transition pairs. Mirror R1's pattern.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines.lean`

- [ ] **Step 1: Read the current Machines.lean to understand existing emission patterns**

```bash
cat crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines.lean
```

Note how the existing `toolCallMachine` (added by issue-149's `a1d4bd9`) emits `ToolCallState`, `ToolFailureClass`, `ToolRetryDisposition`. Look at the JSON shape (typically `name`, `vocabulary`, `transitions` arrays) so the new emissions match.

- [ ] **Step 2: Add `awaitModeMachine` emission**

In `Machines.lean`, after the existing `toolCallMachine` definition, add:

```lean
def awaitModeMachine : Machine where
  name := "AwaitMode"
  vocabulary := Subagent.AwaitMode.all.map Subagent.AwaitMode.toDefraDB
  transitions := []  -- mode is a static enum; no transitions emitted
```

(Adapt the `Machine` struct field names to match what's actually in the file. `Subagent.AwaitMode` and its `all` / `toDefraDB` already exist from B1's spec landing.)

- [ ] **Step 3: Add `cancelPolicyMachine` emission**

```lean
def cancelPolicyMachine : Machine where
  name := "CancelPolicy"
  vocabulary := Subagent.CancelPolicy.all.map Subagent.CancelPolicy.toDefraDB
  transitions := []
```

- [ ] **Step 4: Add `childTerminalMachine` with projection emission**

```lean
def childTerminalMachine : Machine where
  name := "ChildTerminal"
  vocabulary := ["failed", "dead", "interrupted", "superseded"]
  transitions := [
    -- (childTerminal, projectedToolState) pairs from B2's projection rule
    { from := "failed",      to := "failed" },
    { from := "dead",         to := "failed" },
    { from := "interrupted",  to := "cancelled" },
    { from := "superseded",   to := "failed" }
  ]
```

(Adapt to the actual `Transition` field shape in `Machine`.)

- [ ] **Step 5: Extend `toolCallMachine` with the new bridge + mode-flip transitions**

Find `toolCallMachine`'s `transitions` array. Add entries for the 6 new `ToolCallContext.Transition` constructors (`background`, `foreground`, `detach`, `bridge_complete`, `bridge_failure`, `bridge_cancel_cascade`). Each entry pairs the inner-state pre/post (using the existing `ToolCallState` vocabulary) with a `transition_name` discriminator. Emit native `complete` / `fail` rows with a precondition flag (e.g., `requires_native: true`) so Bucket 2 can assert that calling those on a subagent-typed tool fails.

If the existing `Machine` schema doesn't have a precondition-flag field, extend the schema (small Lean change, document it in this task's commit message).

- [ ] **Step 6: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build, zero new sorrys, no warnings. The build emits an updated JSON file (typically at `crates/defra-agent/proofs/conformance.json` or similar — locate it via the existing build pipeline).

- [ ] **Step 7: Sanity-check the JSON output**

```bash
# Find where the contract JSON lands (per R1's pipeline):
find crates/defra-agent -name "*.json" -newer crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines.lean | head -5
```

Verify the output JSON contains entries for `AwaitMode`, `CancelPolicy`, `ChildTerminal`, and the new `toolCallMachine` transitions.

- [ ] **Step 8: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines.lean
git commit -m "$(cat <<'EOF'
Extend conformance contract with AwaitMode, CancelPolicy, ChildTerminal

Adds awaitModeMachine, cancelPolicyMachine, childTerminalMachine
emissions plus bridge transitions on toolCallMachine. Prerequisite
for R2's Bucket 1 (vocabulary round-trip) and Bucket 2 (Lean transition
matrix) — Rust tests consume these JSON entries to assert vocabulary
parity and matrix coverage.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 1: Scaffold the `agent_subagent_v2_to_v3` lens crate

Create the WASM lens crate; mirror the layout of `agent_tool_call_lifecycle_v1_to_v2/`.

**Files:**
- Create: `crates/defra-agent-lenses/agent_subagent_v2_to_v3/Cargo.toml`
- Create: `crates/defra-agent-lenses/agent_subagent_v2_to_v3/src/lib.rs` (skeleton)
- Modify: `Cargo.toml` (workspace root — add new member)

- [ ] **Step 1: Create the crate directory and Cargo.toml**

```bash
mkdir -p crates/defra-agent-lenses/agent_subagent_v2_to_v3/src
```

Write `crates/defra-agent-lenses/agent_subagent_v2_to_v3/Cargo.toml`:

```toml
[package]
name = "agent-subagent-v2-to-v3-lens"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
lens_sdk = "^0.8"
```

- [ ] **Step 2: Add the workspace member**

Edit the root `Cargo.toml`. Find the existing `members = [...]` list and add the new entry alongside `agent_tool_call_lifecycle_v1_to_v2`:

```toml
members = [
    # ...existing members...
    "crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2",
    "crates/defra-agent-lenses/agent_subagent_v2_to_v3",
]
```

- [ ] **Step 3: Write the skeleton `src/lib.rs`**

Write `crates/defra-agent-lenses/agent_subagent_v2_to_v3/src/lib.rs`:

```rust
//! Lens v2→v3: adds subagent extensions to AgentToolCall, AgentRequest,
//! and ToolSelection. Forward transform populates new fields with their
//! defaults; inverse transform drops them for P2P backward-compat.
//!
//! Operates over the same JSON-document iterator API as the v1→v2 lens.

use lens_sdk::define;
use std::collections::HashMap;
use std::error::Error;
use serde_json::Value;

fn try_transform(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<lens_sdk::StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    // Forward transform — implemented in Task 2.
    match iter.next() {
        Some(Ok(Some(_doc))) => Ok(lens_sdk::StreamOption::Some(HashMap::new())),
        Some(Ok(None)) => Ok(lens_sdk::StreamOption::None),
        Some(Err(e)) => Err(Box::new(e)),
        None => Ok(lens_sdk::StreamOption::EndOfStream),
    }
}

fn try_inverse(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<lens_sdk::StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    // Inverse transform — implemented in Task 2.
    match iter.next() {
        Some(Ok(Some(_doc))) => Ok(lens_sdk::StreamOption::Some(HashMap::new())),
        Some(Ok(None)) => Ok(lens_sdk::StreamOption::None),
        Some(Err(e)) => Err(Box::new(e)),
        None => Ok(lens_sdk::StreamOption::EndOfStream),
    }
}

define!(try_transform, try_inverse);
```

This is a stub that compiles but doesn't do real work yet. Task 2 fills in the transforms.

- [ ] **Step 4: Verify the workspace builds**

```bash
cargo check -p agent-subagent-v2-to-v3-lens
```

Expected: clean compile (a few "unused variable" warnings on `_doc` are fine — those go away in Task 2).

```bash
cargo build --release --target wasm32-unknown-unknown -p agent-subagent-v2-to-v3-lens
```

Expected: produces `target/wasm32-unknown-unknown/release/agent_subagent_v2_to_v3_lens.wasm`. If the WASM target isn't installed, run `rustup target add wasm32-unknown-unknown` first.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/defra-agent-lenses/agent_subagent_v2_to_v3/
git commit -m "$(cat <<'EOF'
Scaffold agent_subagent_v2_to_v3 lens crate

New WASM lens crate that will carry the v2→v3 forward and inverse
transforms for AgentToolCall, AgentRequest, and ToolSelection.
Mirrors the layout of agent_tool_call_lifecycle_v1_to_v2.

Stub transforms compile cleanly; real transforms land in Task 2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Implement the v2→v3 lens transforms

Fill in the forward and inverse transforms for the three collections.

**Files:**
- Modify: `crates/defra-agent-lenses/agent_subagent_v2_to_v3/src/lib.rs`

- [ ] **Step 1: Replace the stub `try_transform` with the forward implementation**

Replace `try_transform` in `crates/defra-agent-lenses/agent_subagent_v2_to_v3/src/lib.rs` with:

```rust
fn try_transform(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<lens_sdk::StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    match iter.next() {
        Some(Ok(Some(mut doc))) => {
            // Detect collection by which "shape" fields exist. Each collection's
            // transform is independent — adding a missing field, never overwriting.

            // AgentToolCall extensions
            if doc.contains_key("tool_call_key") || doc.contains_key("session_id") {
                doc.entry("await_mode".to_string())
                    .or_insert(Value::String("foreground".to_string()));
                doc.entry("cancel_policy".to_string())
                    .or_insert(Value::String("cascade".to_string()));
                doc.entry("child_request_id".to_string())
                    .or_insert(Value::Null);
                doc.entry("request_id".to_string())
                    .or_insert(Value::Null);
            }

            // AgentRequest extensions
            if doc.contains_key("request_id") && doc.contains_key("agent_did") {
                doc.entry("subagent_depth".to_string())
                    .or_insert(Value::Number(0.into()));
                doc.entry("caused_by_parent_request_id".to_string())
                    .or_insert(Value::Null);
                doc.entry("caused_by_parent_tool_call_id".to_string())
                    .or_insert(Value::Null);
            }

            // ToolSelection extensions
            if doc.contains_key("selection_id") {
                doc.entry("subagent_targets".to_string())
                    .or_insert(Value::Array(Vec::new()));
                doc.entry("subagent_spawn_enabled".to_string())
                    .or_insert(Value::Bool(false));
                doc.entry("subagent_steering_enabled".to_string())
                    .or_insert(Value::Bool(false));
                doc.entry("subagent_background_enabled".to_string())
                    .or_insert(Value::Bool(false));
            }

            Ok(lens_sdk::StreamOption::Some(doc))
        }
        Some(Ok(None)) => Ok(lens_sdk::StreamOption::None),
        Some(Err(e)) => Err(Box::new(e)),
        None => Ok(lens_sdk::StreamOption::EndOfStream),
    }
}
```

Note the collection-detection heuristics: AgentToolCall uniquely has `tool_call_key`; AgentRequest uniquely has `request_id` AND `agent_did` (without `tool_call_key`); ToolSelection uniquely has `selection_id`. If a doc matches multiple, the transforms are designed to be commutative (only `or_insert`, no overwrites), so the order doesn't matter.

- [ ] **Step 2: Replace the stub `try_inverse` with the inverse implementation**

Replace `try_inverse`:

```rust
fn try_inverse(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<lens_sdk::StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    match iter.next() {
        Some(Ok(Some(mut doc))) => {
            // Drop all v3-only fields for P2P compat with v2 nodes.
            for field in &[
                // AgentToolCall
                "await_mode", "cancel_policy", "child_request_id", "request_id",
                // AgentRequest
                "subagent_depth", "caused_by_parent_request_id", "caused_by_parent_tool_call_id",
                // ToolSelection
                "subagent_targets",
                "subagent_spawn_enabled",
                "subagent_steering_enabled",
                "subagent_background_enabled",
            ] {
                doc.remove(*field);
            }
            Ok(lens_sdk::StreamOption::Some(doc))
        }
        Some(Ok(None)) => Ok(lens_sdk::StreamOption::None),
        Some(Err(e)) => Err(Box::new(e)),
        None => Ok(lens_sdk::StreamOption::EndOfStream),
    }
}
```

- [ ] **Step 3: Build the WASM artifact and check size**

```bash
cargo build --release --target wasm32-unknown-unknown -p agent-subagent-v2-to-v3-lens
wc -c target/wasm32-unknown-unknown/release/agent_subagent_v2_to_v3_lens.wasm
```

Expected: build succeeds; record the binary size for Risk 1 monitoring (per spec — flag if it grows beyond R1's lens by more than a factor of 2).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-lenses/agent_subagent_v2_to_v3/src/lib.rs
git commit -m "$(cat <<'EOF'
Implement v2→v3 lens forward and inverse transforms

Forward transform: pure field additions with defaults — await_mode
defaults to "foreground", cancel_policy to "cascade", child_request_id
and request_id to null on AgentToolCall; subagent_depth=0 + null parent
fields on AgentRequest; empty list + false booleans on ToolSelection.

Inverse transform: drops all v3-only fields for P2P backward-compat
with v2 nodes during rollout.

Collection detection via shape-unique field names; per-document
transforms are commutative (or_insert never overwrites), so a single
lens module covers all three collections without ordering risks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Update GraphQL schemas + define JSON Patches

Add the v3 fields to all three collection schemas. The patches will be applied at runtime by the migration orchestrator (Task 4).

**Files:**
- Modify: `crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql`
- Modify: `crates/defra-agent-protocol/schemas/agent/agent_request.graphql`
- Modify: `crates/defra-agent-protocol/schemas/agent/tool_selection.graphql`
- Modify: `crates/defra-agent/src/migration.rs` (add patch constants only — orchestrator in Task 4)

- [ ] **Step 1: Update `agent_tool_call.graphql`**

Add four new fields just before the closing `}` of the `AgentToolCall` type:

```graphql
type AgentToolCall @branchable {
    # ...existing fields including lifecycle_state...
    await_mode: String
    cancel_policy: String
    child_request_id: String @index
    request_id: String @index
}
```

`@index` on `child_request_id` and `request_id` supports the lineage queries planned for R3 (`AgentToolCall`s spawned by request R; tool calls owning child request C).

- [ ] **Step 2: Update `agent_request.graphql`**

Add three new fields:

```graphql
type AgentRequest @branchable {
    # ...existing fields...
    subagent_depth: Int
    caused_by_parent_request_id: String @index
    caused_by_parent_tool_call_id: String @index
}
```

- [ ] **Step 3: Update `tool_selection.graphql`**

Add four new fields:

```graphql
type ToolSelection {
    # ...existing fields including delegate_to...
    subagent_targets: [String]
    subagent_spawn_enabled: Boolean
    subagent_steering_enabled: Boolean
    subagent_background_enabled: Boolean
}
```

- [ ] **Step 4: Add JSON Patch constants to `migration.rs`**

In `crates/defra-agent/src/migration.rs`, after the existing `ADD_LIFECYCLE_STATE_PATCH` constant, add three new patches:

```rust
const ADD_AGENT_TOOL_CALL_SUBAGENT_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"await_mode","Kind":11}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"cancel_policy","Kind":11}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"child_request_id","Kind":11}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"request_id","Kind":11}}
]"#;

const ADD_AGENT_REQUEST_SUBAGENT_PATCH: &str = r#"[
    {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"subagent_depth","Kind":5}},
    {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"caused_by_parent_request_id","Kind":11}},
    {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"caused_by_parent_tool_call_id","Kind":11}}
]"#;

const ADD_TOOL_SELECTION_SUBAGENT_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_targets","Kind":17}},
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_spawn_enabled","Kind":2}},
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_steering_enabled","Kind":2}},
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_background_enabled","Kind":2}}
]"#;
```

If any Kind code is wrong (build error at runtime when applying the patch), trace through DefraDB's `Kind` enum to find the right code and pin it inline. Common values: 11=String, 5=Int, 2=Boolean, 17=[String]. R1's `migration.rs:17` confirms 11=String.

- [ ] **Step 5: Verify everything compiles**

```bash
cargo check -p defra-agent
```

Expected: clean (the new constants are unused until Task 4; suppress the dead-code warning with `#[allow(dead_code)]` on each constant if it fires, or — better — just commit and pick it up in Task 4 when the orchestrator references them).

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent-protocol/schemas/agent/agent_tool_call.graphql \
        crates/defra-agent-protocol/schemas/agent/agent_request.graphql \
        crates/defra-agent-protocol/schemas/agent/tool_selection.graphql \
        crates/defra-agent/src/migration.rs
git commit -m "$(cat <<'EOF'
Add v3 GraphQL schema fields and migration JSON Patches

AgentToolCall gains 4 fields: await_mode, cancel_policy,
child_request_id (indexed), request_id (indexed).
AgentRequest gains 3 fields: subagent_depth,
caused_by_parent_request_id (indexed),
caused_by_parent_tool_call_id (indexed).
ToolSelection gains 4 fields: subagent_targets, plus three
boolean policy flags (spawn / steering / background).

Migration patch constants land in migration.rs; orchestrator
that applies them lands in Task 4.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Implement `ensure_subagent_extensions_migrations`

Per-collection idempotent orchestrator that applies the three patches and registers the lens. Mirror `ensure_tool_call_migrations`.

**Files:**
- Modify: `crates/defra-agent/src/migration.rs`

- [ ] **Step 1: Add detection helpers**

In `migration.rs`, add three helper functions for per-collection idempotency:

```rust
async fn has_await_mode_field(node: &Arc<EmbeddedNode>) -> Result<bool> {
    let collection = node.get_collection("AgentToolCall").await?;
    Ok(collection
        .map(|c| c.fields.iter().any(|f| f.name == "await_mode"))
        .unwrap_or(false))
}

async fn has_caused_by_parent_request_id_field(node: &Arc<EmbeddedNode>) -> Result<bool> {
    let collection = node.get_collection("AgentRequest").await?;
    Ok(collection
        .map(|c| c.fields.iter().any(|f| f.name == "caused_by_parent_request_id"))
        .unwrap_or(false))
}

async fn has_subagent_targets_field(node: &Arc<EmbeddedNode>) -> Result<bool> {
    let collection = node.get_collection("ToolSelection").await?;
    Ok(collection
        .map(|c| c.fields.iter().any(|f| f.name == "subagent_targets"))
        .unwrap_or(false))
}
```

The exact API for `node.get_collection` and what it returns may differ — read R1's `migration.rs:35-97` for the actual EmbeddedNode collection-introspection API and mirror its style. If `get_collection` returns `Option<CollectionDescription>` with a `.fields` accessor, the above shape works; if not, adapt.

- [ ] **Step 2: Implement the orchestrator**

After the existing `ensure_tool_call_migrations`, add:

```rust
/// Per-collection idempotent migration orchestrator for v2→v3.
/// Applies the three subagent-extension patches and registers the unified
/// lens. Re-running after a partial failure picks up at the un-migrated
/// collection without manual intervention.
pub async fn ensure_subagent_extensions_migrations(
    node: Arc<EmbeddedNode>,
) -> Result<()> {
    // 1. AgentToolCall — patch only if v3 fields not already present.
    if !has_await_mode_field(&node).await? {
        let _v3_atc = node
            .patch_collection("AgentToolCall", ADD_AGENT_TOOL_CALL_SUBAGENT_PATCH)
            .await
            .context("patch_collection v2 -> v3 (AgentToolCall subagent fields)")?;
        // No set_active_collection_version here — patch_collection bumps the
        // active version automatically per R1's pattern. If R1's migration
        // calls set_active_collection_version explicitly, mirror that.
    }

    // 2. AgentRequest — independent idempotency check.
    if !has_caused_by_parent_request_id_field(&node).await? {
        let _v3_ar = node
            .patch_collection("AgentRequest", ADD_AGENT_REQUEST_SUBAGENT_PATCH)
            .await
            .context("patch_collection v2 -> v3 (AgentRequest subagent fields)")?;
    }

    // 3. ToolSelection — independent idempotency check.
    if !has_subagent_targets_field(&node).await? {
        let _v3_ts = node
            .patch_collection("ToolSelection", ADD_TOOL_SELECTION_SUBAGENT_PATCH)
            .await
            .context("patch_collection v2 -> v3 (ToolSelection subagent fields)")?;
    }

    // 4. Register the unified lens (idempotent — safe to call repeatedly).
    // The lens module path mirrors R1's pattern; `lens_module_path()` is the
    // shared helper that resolves the WASM artifact's location.
    let lens_path = lens_module_path("agent_subagent_v2_to_v3_lens.wasm")?;
    let forward_lens = LensConfig::new(/* v_pre id */, /* v_post id */, LensModule::from_path(lens_path)?);
    node.set_migration(forward_lens)
        .await
        .context("register agent_subagent_v2_to_v3 lens")?;

    Ok(())
}
```

The `LensConfig::new` arguments are placeholders — read R1's `ensure_tool_call_migrations` for the exact pattern of obtaining the `from`/`to` version IDs (typically returned by the `patch_collection` calls themselves; chain them through). If the version IDs need to be threaded across the three patches, restructure to capture them before the lens registration.

- [ ] **Step 3: Verify it compiles**

```bash
cargo check -p defra-agent
```

Expected: clean. If `LensConfig::new` signature errors, fix per R1's pattern.

- [ ] **Step 4: Add a unit test for the detection helpers**

Append to the existing test module in `migration.rs`:

```rust
#[cfg(test)]
mod tests_v3 {
    use super::*;

    #[tokio::test]
    async fn detection_helpers_return_false_on_pristine_v2_node() {
        // This test depends on a test_db() helper that gives a v2 node.
        // If R1 provides one, use it; if not, this test is a placeholder
        // that must be wired up in Task 21 alongside the bucket-3 helpers.
    }
}
```

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/migration.rs
git commit -m "$(cat <<'EOF'
Implement ensure_subagent_extensions_migrations

Per-collection idempotent orchestrator: each collection's v3 patch
runs only if its detector field is absent, so a partial failure
recovers on daemon restart without manual intervention. Three
independent detection helpers (has_await_mode_field,
has_caused_by_parent_request_id_field, has_subagent_targets_field).

After all three patches, registers the unified
agent_subagent_v2_to_v3 lens via set_migration.

Daemon wiring lands in Task 5.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Wire the migration into daemon startup

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/serve.rs`

- [ ] **Step 1: Locate the existing `ensure_tool_call_migrations` call**

```bash
grep -n "ensure_tool_call_migrations" crates/defra-agent-cli/src/commands/serve.rs
```

- [ ] **Step 2: Add the v3 migration call immediately after**

Find the line where `ensure_tool_call_migrations(node.clone()).await?;` is called. Add the new call right after:

```rust
defra_agent::migration::ensure_tool_call_migrations(node.clone()).await?;
defra_agent::migration::ensure_subagent_extensions_migrations(node.clone()).await?;
```

The serial ordering is important: v1→v2 (`ensure_tool_call_migrations`) must run before v2→v3 (`ensure_subagent_extensions_migrations`).

- [ ] **Step 3: Verify it compiles**

```bash
cargo check -p defra-agent-cli
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-cli/src/commands/serve.rs
git commit -m "$(cat <<'EOF'
Wire ensure_subagent_extensions_migrations into daemon startup

Called serially after ensure_tool_call_migrations. The v1→v2 baseline
must land before v2→v3 patches apply to the upgraded AgentToolCall
schema.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Add `AwaitMode`, `CancelPolicy`, `ChildTerminal` enums and `CascadeIntent` struct

Adds the four new types to `tool_call_lifecycle.rs` alongside the existing `ToolCallState` and `FailureClass` enums.

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle.rs`

- [ ] **Step 1: Read the existing structure**

```bash
sed -n '1,80p' crates/defra-agent/src/tool_call_lifecycle.rs
```

Note where `ToolCallState` is defined (lines ~10-90) and where `FailureClass` is defined. Place the new enums in the same idiomatic shape (`#[derive]`, `as_str` / `from_persisted` / `ALL`).

- [ ] **Step 2: Add `AwaitMode`**

After the `FailureClass` definition (or wherever fits naturally), add:

```rust
/// Whether the parent's narrative is blocked on this tool's terminal state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AwaitMode {
    Foreground,
    Background,
}

impl AwaitMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AwaitMode::Foreground => "foreground",
            AwaitMode::Background => "background",
        }
    }

    pub fn from_persisted(s: &str) -> Option<Self> {
        match s {
            "foreground" => Some(AwaitMode::Foreground),
            "background" => Some(AwaitMode::Background),
            _ => None,
        }
    }

    pub const ALL: &'static [AwaitMode] = &[AwaitMode::Foreground, AwaitMode::Background];
}
```

- [ ] **Step 3: Add `CancelPolicy`**

```rust
/// Whether parent termination drives the linked child request to .interrupted
/// (cascade) or detaches the child to its own deadline.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CancelPolicy {
    Cascade,
    Detach,
}

impl CancelPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            CancelPolicy::Cascade => "cascade",
            CancelPolicy::Detach => "detach",
        }
    }

    pub fn from_persisted(s: &str) -> Option<Self> {
        match s {
            "cascade" => Some(CancelPolicy::Cascade),
            "detach" => Some(CancelPolicy::Detach),
            _ => None,
        }
    }

    pub const ALL: &'static [CancelPolicy] = &[CancelPolicy::Cascade, CancelPolicy::Detach];
}
```

- [ ] **Step 4: Add `ChildTerminal` and projection**

```rust
/// The four non-.completed terminal states a child AgentRequest can reach.
/// Used as the argument shape to bridge_failure to project the child terminal
/// onto a parent ToolCallState (.failed for most, .cancelled for .interrupted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildTerminal {
    Failed { reason: String, failure_class: FailureClass },
    Dead,
    Interrupted,
    Superseded,
}

impl ChildTerminal {
    /// Lean B2 projection: .interrupted → .cancelled, all others → .failed.
    pub fn projected_state(&self) -> ToolCallState {
        match self {
            ChildTerminal::Interrupted => ToolCallState::Cancelled,
            _ => ToolCallState::Failed,
        }
    }

    /// Persisted vocabulary names for conformance enumeration.
    pub const ALL_KIND: &'static [&'static str] =
        &["failed", "dead", "interrupted", "superseded"];
}
```

- [ ] **Step 5: Add `CascadeIntent`**

```rust
/// Returned by `bridge_cancel_cascade` (wrapped in Option). The caller — typically
/// R3's daemon interrupt dispatcher — performs the actual write to the child
/// AgentRequest's interrupt_requested_at field. Returning None from
/// bridge_cancel_cascade means no cascade is required: the bridge tool is
/// native (no child link), detached (no cascade), or not in .cancelled state.
#[derive(Clone, Debug)]
pub struct CascadeIntent {
    pub child_request_id: String,
    pub at: chrono::DateTime<chrono::Utc>,
}
```

- [ ] **Step 6: Verify compilation**

```bash
cargo check -p defra-agent
```

Expected: clean (six warnings about unused `AwaitMode::ALL`, etc. are expected — they get exercised in Task 19's tests).

- [ ] **Step 7: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle.rs
git commit -m "$(cat <<'EOF'
Add AwaitMode, CancelPolicy, ChildTerminal enums and CascadeIntent struct

AwaitMode (Foreground/Background) and CancelPolicy (Cascade/Detach)
follow the same as_str/from_persisted/ALL pattern as ToolCallState.
ChildTerminal carries the four non-.completed terminal projections
B2 demands; projected_state() implements the Lean projection rule
(.interrupted → .cancelled, others → .failed).
CascadeIntent is the pure return type of bridge_cancel_cascade —
it describes the action the caller must take on the child request.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Extend `ToolCallLifecycle` struct, add `new_subagent` constructor, and new error variants

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle.rs`

- [ ] **Step 1: Add the three new fields to `ToolCallLifecycle`**

Locate the struct (around lines 118-177 per the survey). Add three fields at the end (just before any closing braces, after `failure_class`):

```rust
pub struct ToolCallLifecycle {
    // ...existing fields...
    pub(crate) await_mode: AwaitMode,           // default Foreground
    pub(crate) cancel_policy: CancelPolicy,     // default Cascade
    pub(crate) child_request_id: Option<String>, // None = native; Some = subagent invocation
}
```

(`pub(crate)` matches the existing field visibility.)

- [ ] **Step 2: Update the existing `new()` constructor**

The existing `new()` constructor doesn't yet set the three new fields. Update it to populate them with defaults so the struct compiles:

```rust
pub fn new(
    node: Arc<EmbeddedNode>,
    session_id: String,
    tool_call_id: String,
    message_sequence: u32,
    tool_name: String,
    args: String,
) -> Self {
    Self {
        // ...existing field initializers...
        await_mode: AwaitMode::Foreground,
        cancel_policy: CancelPolicy::Cascade,
        child_request_id: None,
    }
}
```

- [ ] **Step 3: Add `new_subagent` constructor**

After the existing `new`:

```rust
/// Constructor for the subagent invocation path. Sets child_request_id (the
/// link to the spawned child AgentRequest) and lets the caller pick await_mode
/// and cancel_policy. Synchronous and does not persist — first transition
/// (typically start_running) creates the row.
pub fn new_subagent(
    node: Arc<EmbeddedNode>,
    session_id: String,
    tool_call_id: String,
    message_sequence: u32,
    tool_name: String,
    args: String,
    await_mode: AwaitMode,
    cancel_policy: CancelPolicy,
    child_request_id: String,
) -> Self {
    Self {
        node,
        session_id,
        tool_call_id,
        message_sequence,
        tool_name,
        args,
        doc_id: None,
        state: ToolCallState::Pending,
        started_at: None,
        failure_class: None,
        await_mode,
        cancel_policy,
        child_request_id: Some(child_request_id),
    }
}
```

- [ ] **Step 4: Add the new `IllegalToolCallTransition` variants**

Locate the `IllegalToolCallTransition` enum. Add ten new variants:

```rust
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum IllegalToolCallTransition {
    // ...existing variants...
    #[error("await_mode flip rejected: tool already Background")]
    ModeAlreadyBackground,
    #[error("await_mode flip rejected: tool already Foreground")]
    ModeAlreadyForeground,
    #[error("cancel_policy flip rejected: tool already Detach")]
    PolicyAlreadyDetach,
    #[error("bridge_complete called on tool without child_request_id")]
    BridgeCompleteRequiresChildLink,
    #[error("bridge_failure called on tool without child_request_id")]
    BridgeFailureRequiresChildLink,
    #[error("bridge_cancel_cascade called on tool not in .cancelled state")]
    CascadeRequiresCancelled,
    #[error("create_subagent_request rejected: depth exceeds maxSubagentDepth")]
    SubagentDepthExceeded,
    #[error("AgentRequest parent linkage incoherent: must set both or neither parent fields")]
    ParentLinkageIncoherent,
    #[error("native complete() called on subagent-typed tool (child_request_id is set)")]
    NativeCompleteOnSubagentTool,
    #[error("native fail() called on subagent-typed tool (child_request_id is set)")]
    NativeFailOnSubagentTool,
}
```

(Adapt to the existing `thiserror` style if it's already in use; copy the exact attribute pattern from existing variants.)

- [ ] **Step 5: Verify compilation**

```bash
cargo check -p defra-agent
```

Expected: clean. Some "unused variant" warnings are fine — those go away as later tasks reference each one.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle.rs
git commit -m "$(cat <<'EOF'
Extend ToolCallLifecycle with subagent fields and new_subagent constructor

Three new fields (await_mode, cancel_policy, child_request_id) with
defaults that preserve R1's existing semantics for native tools.
new_subagent constructor sets child_request_id explicitly and lets
the caller choose await_mode + cancel_policy; first transition still
creates the persisted row, matching new()'s sync-only contract.

Ten new IllegalToolCallTransition variants cover the new error cases
exposed by Tasks 8-14.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Add symmetric `h_native` guards to native `complete()` and `fail()`

The Lean inner `complete` constructor requires `childRequestId = none`. Without symmetric Rust guards, a subagent-typed lifecycle could call `complete()` directly and bypass `bridge_complete`'s preconditions.

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Locate the existing `complete()` method**

```bash
grep -n "pub async fn complete\|pub async fn fail" crates/defra-agent/src/tool_call_lifecycle/transition.rs
```

- [ ] **Step 2: Write a failing test for the new guard on `complete`**

Add to the existing test module in `transition.rs` (or wherever R1's tests live for `complete`):

```rust
#[tokio::test]
async fn complete_rejects_subagent_typed_tool() {
    let lc = ToolCallLifecycle::new_subagent(
        test_node().await,
        "sess-1".to_string(),
        "tc-1".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-req-1".to_string(),
    );
    // start_running is allowed on subagent-typed tools.
    let mut lc = lc;
    lc.start_running().await.unwrap();
    // complete() must reject because child_request_id is set.
    let err = lc.complete(test_result()).await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<IllegalToolCallTransition>(),
        Some(IllegalToolCallTransition::NativeCompleteOnSubagentTool)
    ));
}
```

(`test_node()` and `test_result()` are placeholders — use whatever R1's existing tests use; check R1's `tool_call_lifecycle_conformance.rs` for the actual helper names.)

- [ ] **Step 3: Run the test to confirm it fails**

```bash
cargo test -p defra-agent --lib complete_rejects_subagent_typed_tool
```

Expected: FAIL — the current `complete()` doesn't have the guard, so it'll either succeed unexpectedly or fail with a different error.

- [ ] **Step 4: Add the guard to `complete()`**

Find the body of `complete()` (typically around lines 105-147 per the survey). Right after `ensure_state(&[ToolCallState::Running])?` add:

```rust
if self.child_request_id.is_some() {
    return Err(IllegalToolCallTransition::NativeCompleteOnSubagentTool.into());
}
```

- [ ] **Step 5: Run the test to confirm it passes**

```bash
cargo test -p defra-agent --lib complete_rejects_subagent_typed_tool
```

Expected: PASS.

- [ ] **Step 6: Repeat Steps 2-5 for `fail()`**

Add a parallel test:

```rust
#[tokio::test]
async fn fail_rejects_subagent_typed_tool() {
    // ...same setup...
    let err = lc.fail(test_result(), FailureClass::ExternalDependencyFailure).await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<IllegalToolCallTransition>(),
        Some(IllegalToolCallTransition::NativeFailOnSubagentTool)
    ));
}
```

Run, confirm FAIL, add the guard to `fail()`:

```rust
if self.child_request_id.is_some() {
    return Err(IllegalToolCallTransition::NativeFailOnSubagentTool.into());
}
```

Run, confirm PASS.

- [ ] **Step 7: Verify the existing native-path tests still pass**

```bash
cargo test -p defra-agent --lib --test tool_call_lifecycle_conformance
```

Expected: PASS (no regressions; native tools still complete and fail normally because they have `child_request_id = None`).

- [ ] **Step 8: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle/transition.rs
git commit -m "$(cat <<'EOF'
Add symmetric h_native guards on complete() and fail()

Mirrors Lean's inner complete constructor's h_native precondition
(Proofs/ToolExecution/Transition.lean:28-33). A subagent-typed
ToolCallLifecycle (child_request_id = Some) calling complete() or
fail() now returns NativeCompleteOnSubagentTool or
NativeFailOnSubagentTool respectively. This forces the bridge path
(bridge_complete / bridge_failure) to be the only way subagent tools
reach a terminal state.

Native tools (child_request_id = None) are unaffected; existing R1
tests pass.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Implement `background()` mode-flip transition

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Write the failing test**

Add to the test module:

```rust
#[tokio::test]
async fn background_flips_await_mode_from_foreground_to_background() {
    let mut lc = ToolCallLifecycle::new_subagent(
        test_node().await,
        "sess-2".to_string(),
        "tc-2".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-req-2".to_string(),
    );
    lc.start_running().await.unwrap();
    assert_eq!(lc.await_mode, AwaitMode::Foreground);
    lc.background().await.unwrap();
    assert_eq!(lc.await_mode, AwaitMode::Background);

    // Calling background() again returns ModeAlreadyBackground.
    let err = lc.background().await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<IllegalToolCallTransition>(),
        Some(IllegalToolCallTransition::ModeAlreadyBackground)
    ));
}

#[tokio::test]
async fn background_rejects_pending_state() {
    let mut lc = ToolCallLifecycle::new(/* ...same params, native... */);
    // Don't start_running; lifecycle is still Pending.
    let err = lc.background().await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<IllegalToolCallTransition>(),
        Some(IllegalToolCallTransition::WrongState { .. })
    ));
}
```

- [ ] **Step 2: Run the tests to confirm they fail**

```bash
cargo test -p defra-agent --lib background_flips
cargo test -p defra-agent --lib background_rejects_pending
```

Expected: FAIL — `background()` doesn't exist yet.

- [ ] **Step 3: Implement `background()`**

In `transition.rs`, after the existing `cancel_during_run` (or wherever fits), add:

```rust
impl ToolCallLifecycle {
    /// Lean parity: ToolCallContext.Transition.background.
    /// Pending|Running stays; await_mode .foreground → .background.
    /// Persists the new await_mode to the row.
    pub async fn background(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running])?;
        if self.await_mode == AwaitMode::Background {
            return Err(IllegalToolCallTransition::ModeAlreadyBackground.into());
        }
        // Persist the change. Mirror the existing UPDATE-via-mutation pattern.
        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    docID: "{doc_id}",
                    input: {{ await_mode: "background" }}
                ) {{ _docID }}
            }}"#,
            doc_id = self
                .doc_id
                .as_deref()
                .ok_or(IllegalToolCallTransition::DocIdMissing)?,
        );
        self.execute_mutation_with_retry(&mutation).await?;
        self.await_mode = AwaitMode::Background;
        Ok(())
    }
}
```

(The exact mutation string format must match how existing transitions do persistence — read `complete()`'s body for the precise GraphQL escaping pattern, error handling, and retry helper signature. `IllegalToolCallTransition::DocIdMissing` may already exist; if not, add it as a new variant alongside the others.)

- [ ] **Step 4: Run the tests to confirm they pass**

```bash
cargo test -p defra-agent --lib background_flips
cargo test -p defra-agent --lib background_rejects_pending
```

Expected: PASS.

- [ ] **Step 5: Verify the wider test suite still passes**

```bash
cargo test -p defra-agent --lib
```

Expected: PASS (no regressions).

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle/transition.rs
git commit -m "$(cat <<'EOF'
Implement background() mode-flip transition

Lean parity: ToolCallContext.Transition.background. Requires Running
state (ensure_state guard) and Foreground mode (returns
ModeAlreadyBackground if violated). Persists await_mode = "background"
via UPDATE mutation, then updates in-memory state on success.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Implement `foreground()` mode-flip transition

Mirror of Task 9 in the opposite direction.

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn foreground_flips_await_mode_from_background_to_foreground() {
    let mut lc = ToolCallLifecycle::new_subagent(
        test_node().await,
        "sess-3".to_string(),
        "tc-3".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        AwaitMode::Background,    // start in Background
        CancelPolicy::Cascade,
        "child-req-3".to_string(),
    );
    lc.start_running().await.unwrap();
    assert_eq!(lc.await_mode, AwaitMode::Background);
    lc.foreground().await.unwrap();
    assert_eq!(lc.await_mode, AwaitMode::Foreground);

    // Calling foreground() again returns ModeAlreadyForeground.
    let err = lc.foreground().await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<IllegalToolCallTransition>(),
        Some(IllegalToolCallTransition::ModeAlreadyForeground)
    ));
}
```

- [ ] **Step 2: Run to confirm fail, implement, run to confirm pass**

```bash
cargo test -p defra-agent --lib foreground_flips
```

Expected: FAIL.

Implementation in `transition.rs`:

```rust
impl ToolCallLifecycle {
    /// Lean parity: ToolCallContext.Transition.foreground.
    pub async fn foreground(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running])?;
        if self.await_mode == AwaitMode::Foreground {
            return Err(IllegalToolCallTransition::ModeAlreadyForeground.into());
        }
        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    docID: "{doc_id}",
                    input: {{ await_mode: "foreground" }}
                ) {{ _docID }}
            }}"#,
            doc_id = self
                .doc_id
                .as_deref()
                .ok_or(IllegalToolCallTransition::DocIdMissing)?,
        );
        self.execute_mutation_with_retry(&mutation).await?;
        self.await_mode = AwaitMode::Foreground;
        Ok(())
    }
}
```

```bash
cargo test -p defra-agent --lib foreground_flips
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle/transition.rs
git commit -m "$(cat <<'EOF'
Implement foreground() mode-flip transition

Symmetric to background(): Running state + Background mode → Foreground.
Returns ModeAlreadyForeground if already Foreground.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Implement `detach()` policy transition

Pending|Running → flip cancel_policy from Cascade to Detach. One-way (no `cascade()` method, mirroring Lean's structural irreversibility).

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn detach_flips_cancel_policy_one_way() {
    let mut lc = ToolCallLifecycle::new_subagent(
        test_node().await,
        "sess-4".to_string(),
        "tc-4".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-req-4".to_string(),
    );
    // detach() is allowed in Pending too — mirror the Lean transition's h_live
    // that allows .pending OR .running.
    assert_eq!(lc.cancel_policy, CancelPolicy::Cascade);
    lc.detach().await.unwrap();
    assert_eq!(lc.cancel_policy, CancelPolicy::Detach);

    // Calling detach() again returns PolicyAlreadyDetach.
    let err = lc.detach().await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<IllegalToolCallTransition>(),
        Some(IllegalToolCallTransition::PolicyAlreadyDetach)
    ));
}
```

- [ ] **Step 2: Run, confirm fail, implement**

```bash
cargo test -p defra-agent --lib detach_flips
```

Expected: FAIL.

```rust
impl ToolCallLifecycle {
    /// Lean parity: ToolCallContext.Transition.detach. Pending|Running stays;
    /// cancel_policy .cascade → .detach. One-way — no inverse method.
    pub async fn detach(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Pending, ToolCallState::Running])?;
        if self.cancel_policy == CancelPolicy::Detach {
            return Err(IllegalToolCallTransition::PolicyAlreadyDetach.into());
        }
        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    docID: "{doc_id}",
                    input: {{ cancel_policy: "detach" }}
                ) {{ _docID }}
            }}"#,
            doc_id = self
                .doc_id
                .as_deref()
                .ok_or(IllegalToolCallTransition::DocIdMissing)?,
        );
        self.execute_mutation_with_retry(&mutation).await?;
        self.cancel_policy = CancelPolicy::Detach;
        Ok(())
    }
}
```

```bash
cargo test -p defra-agent --lib detach_flips
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle/transition.rs
git commit -m "$(cat <<'EOF'
Implement detach() policy-change transition

Lean parity: ToolCallContext.Transition.detach. Pending|Running stays;
cancel_policy .cascade → .detach. One-way (no cascade() method —
matches Lean's structural irreversibility).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Implement `bridge_complete()`

Parent tool .running → .completed when the linked child request has reached .completed. Trust boundary: caller verifies child state.

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn bridge_complete_transitions_running_to_completed() {
    let mut lc = ToolCallLifecycle::new_subagent(
        test_node().await,
        "sess-5".to_string(),
        "tc-5".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-req-5".to_string(),
    );
    lc.start_running().await.unwrap();
    let result = "child final assistant message".to_string();
    lc.bridge_complete(result.clone()).await.unwrap();
    assert_eq!(lc.state, ToolCallState::Completed);
    // Verify result was persisted — read back via load() if available.
    // (Bucket 3 will exercise the persistence end-to-end in Task 25.)
}

#[tokio::test]
async fn bridge_complete_rejects_native_tool() {
    let mut lc = ToolCallLifecycle::new(/* ...native, no child_request_id... */);
    lc.start_running().await.unwrap();
    let err = lc.bridge_complete("x".to_string()).await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<IllegalToolCallTransition>(),
        Some(IllegalToolCallTransition::BridgeCompleteRequiresChildLink)
    ));
}
```

- [ ] **Step 2: Run, confirm fail, implement**

```bash
cargo test -p defra-agent --lib bridge_complete
```

Expected: FAIL.

```rust
impl ToolCallLifecycle {
    /// Lean parity: bridge_complete. Parent tool .running → .completed when the
    /// linked child request has reached .completed (caller verifies). Persists
    /// child_result as the row's `result` field; sets state, completed_at,
    /// latency_ms following R1's complete() persistence pattern.
    pub async fn bridge_complete(&mut self, child_result: String) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running])?;
        if self.child_request_id.is_none() {
            return Err(IllegalToolCallTransition::BridgeCompleteRequiresChildLink.into());
        }
        // Mirror complete()'s persistence pattern, but write state="completed"
        // alongside the result. Read R1's complete() body for the exact mutation
        // shape (latency_ms = now - started_at; completed_at = now; etc.).
        let now = chrono::Utc::now();
        let latency_ms = self
            .started_at
            .map(|s| (now - s).num_milliseconds())
            .unwrap_or(0);
        let escaped_result = graphql::escape_graphql_string(&child_result);
        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    docID: "{doc_id}",
                    input: {{
                        lifecycle_state: "completed",
                        result: "{result}",
                        completed_at: "{completed_at}",
                        latency_ms: {latency_ms}
                    }}
                ) {{ _docID }}
            }}"#,
            doc_id = self
                .doc_id
                .as_deref()
                .ok_or(IllegalToolCallTransition::DocIdMissing)?,
            result = escaped_result,
            completed_at = now.to_rfc3339(),
            latency_ms = latency_ms,
        );
        self.execute_mutation_with_retry(&mutation).await?;
        self.state = ToolCallState::Completed;
        Ok(())
    }
}
```

If `graphql::escape_graphql_string` lives at a different path, find it via `grep -rn "escape_graphql_string" crates/defra-agent/src/` and use the actual import path.

```bash
cargo test -p defra-agent --lib bridge_complete
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle/transition.rs
git commit -m "$(cat <<'EOF'
Implement bridge_complete() bridge transition

Lean parity: bridge_complete. Parent tool .running → .completed when
the caller has verified the linked child request reached .completed.
Persists child_result, completed_at, latency_ms following R1's
complete() pattern. Returns BridgeCompleteRequiresChildLink for tools
without a child_request_id.

Trust boundary: bridge_complete does NOT verify the child's terminal
state internally (Lean's precondition is on the caller). R3's
SubagentSource will be the natural place for that check.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Implement `bridge_failure()`

Parent tool .running → .failed (or .cancelled for child .interrupted) per the `ChildTerminal::projected_state()` rule.

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Write the failing tests for the four projection cases**

```rust
#[tokio::test]
async fn bridge_failure_failed_projects_to_failed() {
    let mut lc = make_running_subagent_tool().await;
    lc.bridge_failure(ChildTerminal::Failed {
        reason: "child failed".to_string(),
        failure_class: FailureClass::ExternalDependencyFailure,
    }).await.unwrap();
    assert_eq!(lc.state, ToolCallState::Failed);
    assert_eq!(lc.failure_class, Some(FailureClass::ExternalDependencyFailure));
}

#[tokio::test]
async fn bridge_failure_dead_projects_to_failed() {
    let mut lc = make_running_subagent_tool().await;
    lc.bridge_failure(ChildTerminal::Dead).await.unwrap();
    assert_eq!(lc.state, ToolCallState::Failed);
}

#[tokio::test]
async fn bridge_failure_interrupted_projects_to_cancelled() {
    let mut lc = make_running_subagent_tool().await;
    lc.bridge_failure(ChildTerminal::Interrupted).await.unwrap();
    assert_eq!(lc.state, ToolCallState::Cancelled);
}

#[tokio::test]
async fn bridge_failure_superseded_projects_to_failed() {
    let mut lc = make_running_subagent_tool().await;
    lc.bridge_failure(ChildTerminal::Superseded).await.unwrap();
    assert_eq!(lc.state, ToolCallState::Failed);
}

#[tokio::test]
async fn bridge_failure_rejects_native_tool() {
    let mut lc = make_running_native_tool().await;
    let err = lc.bridge_failure(ChildTerminal::Dead).await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<IllegalToolCallTransition>(),
        Some(IllegalToolCallTransition::BridgeFailureRequiresChildLink)
    ));
}

// helpers
async fn make_running_subagent_tool() -> ToolCallLifecycle {
    let mut lc = ToolCallLifecycle::new_subagent(
        test_node().await,
        format!("sess-{}", uuid::Uuid::new_v4()),
        format!("tc-{}", uuid::Uuid::new_v4()),
        1, "spawn_subagent".to_string(), "{}".to_string(),
        AwaitMode::Foreground, CancelPolicy::Cascade,
        format!("child-req-{}", uuid::Uuid::new_v4()),
    );
    lc.start_running().await.unwrap();
    lc
}

async fn make_running_native_tool() -> ToolCallLifecycle {
    let mut lc = ToolCallLifecycle::new(
        test_node().await,
        format!("sess-{}", uuid::Uuid::new_v4()),
        format!("tc-{}", uuid::Uuid::new_v4()),
        1, "echo".to_string(), "{}".to_string(),
    );
    lc.start_running().await.unwrap();
    lc
}
```

- [ ] **Step 2: Run, confirm fail, implement**

```bash
cargo test -p defra-agent --lib bridge_failure
```

Expected: FAIL on all five tests.

```rust
impl ToolCallLifecycle {
    /// Lean parity: bridge_failure. Parent tool .running → .failed (or .cancelled
    /// for ChildTerminal::Interrupted). Projection per
    /// ChildTerminal::projected_state(). Persists state, failure_class (if
    /// applicable), and reason.
    pub async fn bridge_failure(&mut self, child_terminal: ChildTerminal) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running])?;
        if self.child_request_id.is_none() {
            return Err(IllegalToolCallTransition::BridgeFailureRequiresChildLink.into());
        }
        let projected = child_terminal.projected_state();
        let (failure_class_for_persist, reason_for_persist) = match &child_terminal {
            ChildTerminal::Failed { reason, failure_class } => (Some(*failure_class), Some(reason.clone())),
            _ => (None, None),
        };

        let now = chrono::Utc::now();
        let escaped_reason = reason_for_persist
            .as_deref()
            .map(graphql::escape_graphql_string)
            .unwrap_or_default();
        let failure_class_persist = failure_class_for_persist
            .map(|fc| format!(r#", tool_failure_class: "{}""#, fc.as_str()))
            .unwrap_or_default();
        let result_persist = if !escaped_reason.is_empty() {
            format!(r#", result: "{}""#, escaped_reason)
        } else {
            String::new()
        };
        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    docID: "{doc_id}",
                    input: {{
                        lifecycle_state: "{state_str}",
                        completed_at: "{completed_at}"
                        {failure_class_persist}
                        {result_persist}
                    }}
                ) {{ _docID }}
            }}"#,
            doc_id = self
                .doc_id
                .as_deref()
                .ok_or(IllegalToolCallTransition::DocIdMissing)?,
            state_str = projected.as_str(),
            completed_at = now.to_rfc3339(),
            failure_class_persist = failure_class_persist,
            result_persist = result_persist,
        );
        self.execute_mutation_with_retry(&mutation).await?;
        self.state = projected;
        self.failure_class = failure_class_for_persist;
        Ok(())
    }
}
```

```bash
cargo test -p defra-agent --lib bridge_failure
```

Expected: all five PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle/transition.rs
git commit -m "$(cat <<'EOF'
Implement bridge_failure() bridge transition with B2 projection

Lean parity: bridge_failure. ChildTerminal::Failed/Dead/Superseded
project to ToolCallState::Failed; ChildTerminal::Interrupted projects
to ToolCallState::Cancelled (matches Lean B2's projection rule).
Failed variant carries reason + failure_class; the others carry
neither. Returns BridgeFailureRequiresChildLink for native tools.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 14: Implement `bridge_cancel_cascade()`

Pure (no DB writes); returns `Option<CascadeIntent>`. Caller (R3's daemon dispatcher) executes the cascade write.

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn bridge_cancel_cascade_returns_intent_for_cascade_subagent() {
    let mut lc = ToolCallLifecycle::new_subagent(
        test_node().await,
        "sess-c1".to_string(), "tc-c1".to_string(), 1,
        "spawn_subagent".to_string(), "{}".to_string(),
        AwaitMode::Foreground, CancelPolicy::Cascade,
        "child-req-c1".to_string(),
    );
    lc.start_running().await.unwrap();
    lc.cancel_during_run().await.unwrap();   // forces .cancelled
    let intent = lc.bridge_cancel_cascade().await.unwrap();
    let intent = intent.expect("should return Some(CascadeIntent)");
    assert_eq!(intent.child_request_id, "child-req-c1");
}

#[tokio::test]
async fn bridge_cancel_cascade_returns_none_for_detached() {
    let mut lc = ToolCallLifecycle::new_subagent(
        test_node().await, "sess-c2".to_string(), "tc-c2".to_string(), 1,
        "spawn_subagent".to_string(), "{}".to_string(),
        AwaitMode::Foreground, CancelPolicy::Detach,
        "child-req-c2".to_string(),
    );
    lc.start_running().await.unwrap();
    lc.cancel_during_run().await.unwrap();
    let intent = lc.bridge_cancel_cascade().await.unwrap();
    assert!(intent.is_none(), "Detach policy returns None");
}

#[tokio::test]
async fn bridge_cancel_cascade_returns_none_for_native() {
    let mut lc = ToolCallLifecycle::new(
        test_node().await, "sess-c3".to_string(), "tc-c3".to_string(), 1,
        "echo".to_string(), "{}".to_string(),
    );
    lc.start_running().await.unwrap();
    lc.cancel_during_run().await.unwrap();
    let intent = lc.bridge_cancel_cascade().await.unwrap();
    assert!(intent.is_none(), "Native tool (no child_request_id) returns None");
}

#[tokio::test]
async fn bridge_cancel_cascade_rejects_non_cancelled_state() {
    let mut lc = ToolCallLifecycle::new_subagent(
        test_node().await, "sess-c4".to_string(), "tc-c4".to_string(), 1,
        "spawn_subagent".to_string(), "{}".to_string(),
        AwaitMode::Foreground, CancelPolicy::Cascade,
        "child-req-c4".to_string(),
    );
    lc.start_running().await.unwrap();
    // state is Running, not Cancelled.
    let err = lc.bridge_cancel_cascade().await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<IllegalToolCallTransition>(),
        Some(IllegalToolCallTransition::CascadeRequiresCancelled)
    ));
}
```

- [ ] **Step 2: Run, confirm fail, implement**

```bash
cargo test -p defra-agent --lib bridge_cancel_cascade
```

Expected: FAIL on all four.

```rust
impl ToolCallLifecycle {
    /// Lean parity: bridge_cancel_cascade. Pure — returns the action that should
    /// be taken on the child AgentRequest. Caller (typically R3's daemon
    /// interrupt dispatcher) performs the actual write to set
    /// interrupt_requested_at on the child. Returns None for native tools,
    /// detached subagents, or non-cancelled bridge tools.
    pub async fn bridge_cancel_cascade(&self) -> Result<Option<CascadeIntent>> {
        if self.state != ToolCallState::Cancelled {
            return Err(IllegalToolCallTransition::CascadeRequiresCancelled.into());
        }
        if self.cancel_policy != CancelPolicy::Cascade {
            return Ok(None); // detached: no cascade
        }
        let Some(child_request_id) = self.child_request_id.clone() else {
            return Ok(None); // native: no bridge edge
        };
        Ok(Some(CascadeIntent {
            child_request_id,
            at: chrono::Utc::now(),
        }))
    }
}
```

```bash
cargo test -p defra-agent --lib bridge_cancel_cascade
```

Expected: all four PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle/transition.rs
git commit -m "$(cat <<'EOF'
Implement bridge_cancel_cascade() pure return-intent transition

Lean parity: bridge_cancel_cascade. Returns Option<CascadeIntent>:
- Some(CascadeIntent { child_request_id, at }) for cancelled
  cascade-mode subagent tools
- None for detached subagents
- None for native tools (no child link)
- Err(CascadeRequiresCancelled) for non-cancelled state

Pure: no DB writes. R3's daemon interrupt dispatcher consumes the
intent to set interrupt_requested_at on the child AgentRequest.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 15: Extend `AgentRequestRow` and `AgentRequest` DAO structs

**Files:**
- Modify: `crates/defra-agent/src/watcher.rs:19` (the `AgentRequest` struct)
- Modify: `crates/defra-agent/src/watcher/query.rs:93` (the `AgentRequestRow` struct)

- [ ] **Step 1: Locate and extend `AgentRequest`**

```bash
grep -n "pub struct AgentRequest" crates/defra-agent/src/watcher.rs
```

Add three new fields to the struct (matching the GraphQL schema):

```rust
pub struct AgentRequest {
    // ...existing fields...
    pub subagent_depth: u32,
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_parent_tool_call_id: Option<String>,
}
```

- [ ] **Step 2: Extend `AgentRequestRow`**

In `crates/defra-agent/src/watcher/query.rs`:

```rust
pub struct AgentRequestRow {
    // ...existing fields...
    pub subagent_depth: Option<u32>,            // Option because v2 rows have null
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_parent_tool_call_id: Option<String>,
}
```

(Use `Option<u32>` not `u32` to absorb any genuine nulls from the database during the migration window. Convert to `u32::default()` (= 0) when materializing into the public `AgentRequest` struct.)

- [ ] **Step 3: Update the GraphQL query string that reads AgentRequest rows**

```bash
grep -n "request_id\|agent_did" crates/defra-agent/src/watcher/query.rs | head -10
```

Find the GraphQL query (typically a const string with the field list). Add the three new fields to the SELECT-style projection:

```rust
const AGENT_REQUEST_QUERY: &str = r#"
    query {
        AgentRequest {
            request_id
            agent_did
            // ...existing field list...
            subagent_depth
            caused_by_parent_request_id
            caused_by_parent_tool_call_id
        }
    }
"#;
```

(Adapt to the actual query syntax in this file.)

- [ ] **Step 4: Update the row→struct conversion**

Find where `AgentRequestRow` becomes `AgentRequest`. Add field forwarding:

```rust
impl From<AgentRequestRow> for AgentRequest {
    fn from(row: AgentRequestRow) -> Self {
        Self {
            // ...existing field forwarding...
            subagent_depth: row.subagent_depth.unwrap_or(0),
            caused_by_parent_request_id: row.caused_by_parent_request_id,
            caused_by_parent_tool_call_id: row.caused_by_parent_tool_call_id,
        }
    }
}
```

(The conversion function may not exist in this exact form; adapt to the existing pattern in `query.rs`.)

- [ ] **Step 5: Verify compilation**

```bash
cargo check -p defra-agent
```

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/watcher.rs crates/defra-agent/src/watcher/query.rs
git commit -m "$(cat <<'EOF'
Extend AgentRequestRow and AgentRequest with subagent fields

Three new fields (subagent_depth: u32, caused_by_parent_request_id,
caused_by_parent_tool_call_id) flow from the v3 schema through the
GraphQL query string into AgentRequestRow (Option types absorb v2-row
nulls during migration window) and into the public AgentRequest
struct (defaults to depth=0 + None for top-level).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 16: Extend `ToolSelectionDocument` and add apply-time well-formedness validation

**Files:**
- Modify: `crates/defra-agent/src/document_config/tool_selection.rs`

- [ ] **Step 1: Add the four new optional fields to the struct**

```rust
pub struct ToolSelectionDocument {
    // ...existing fields...
    pub subagent_targets: Option<Vec<String>>,
    pub subagent_spawn_enabled: Option<bool>,
    pub subagent_steering_enabled: Option<bool>,
    pub subagent_background_enabled: Option<bool>,
}
```

- [ ] **Step 2: Update the deserialization / row materialization**

Find the function that reads `ToolSelectionDocument` from a DefraDB row (likely a `From<...>` or a query handler). Add the four new fields to the materialization. Read the existing pattern for `delegate_to: Option<Vec<String>>` and copy it for `subagent_targets`; copy the boolean pattern from `enable_file_tools` for the three new booleans.

- [ ] **Step 3: Write a failing test for well-formedness validation**

Add to the test module in `tool_selection.rs`:

```rust
#[test]
fn validate_rejects_empty_string_in_subagent_targets() {
    let doc = ToolSelectionDocument {
        // ...minimal valid baseline...
        subagent_targets: Some(vec!["".to_string()]),
        subagent_spawn_enabled: Some(true),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(result.is_err());
    assert!(format!("{}", result.unwrap_err()).contains("subagent_targets"));
}

#[test]
fn validate_accepts_well_formed_subagent_targets() {
    let doc = ToolSelectionDocument {
        // ...minimal valid baseline...
        subagent_targets: Some(vec!["amy-code".to_string(), "amy-research".to_string()]),
        subagent_spawn_enabled: Some(true),
        subagent_steering_enabled: Some(false),
        subagent_background_enabled: Some(true),
        ..Default::default()
    };
    assert!(doc.validate().is_ok());
}
```

(If `ToolSelectionDocument` doesn't already implement `Default`, mock the baseline struct instance directly with all fields.)

- [ ] **Step 4: Run, confirm fail, implement validation**

```bash
cargo test -p defra-agent --lib validate_rejects_empty_string_in_subagent_targets
```

Expected: FAIL — `validate()` doesn't exist or doesn't check.

If a `validate()` method already exists on `ToolSelectionDocument`, extend it with:

```rust
pub fn validate(&self) -> Result<()> {
    // ...existing validations...
    if let Some(targets) = &self.subagent_targets {
        for (i, target) in targets.iter().enumerate() {
            if target.is_empty() {
                return Err(anyhow!(
                    "subagent_targets[{}] is empty; behavior IDs must be non-empty strings",
                    i
                ));
            }
        }
    }
    Ok(())
}
```

If `validate()` does NOT exist, add it with just the new check (the existing flow probably validates implicitly via row deserialization).

```bash
cargo test -p defra-agent --lib validate_rejects_empty_string_in_subagent_targets
cargo test -p defra-agent --lib validate_accepts_well_formed_subagent_targets
```

Expected: both PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/document_config/tool_selection.rs
git commit -m "$(cat <<'EOF'
Extend ToolSelectionDocument with subagent fields and well-formedness validation

Four new optional fields: subagent_targets (Vec<String>), and three
boolean flags (spawn / steering / background). Apply-time validation
rejects empty strings in subagent_targets — the cross-reference check
(target resolves to a real AgentBehavior) is deferred to R3.

The three booleans are independent: enabling steering does not
require enabling spawn (R3's policy enforcement layer can add
cross-flag invariants if needed).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 17: Add AgentRequest parent-linkage coherence validation

**Files:**
- Modify: `crates/defra-agent/src/agent/document_view/apply.rs` (or wherever AgentRequest validation lives — find via grep)

- [ ] **Step 1: Locate the apply-time validation site for AgentRequest**

```bash
grep -rn "AgentRequest" crates/defra-agent/src/agent/document_view/ | head -10
grep -rn "fn apply_control_update\|fn validate_agent_request\|fn load_agent_request" crates/defra-agent/src/ | head -10
```

R2's spec (Section "AgentRequest → Apply-time validation") says this lives in `apply.rs`. If it's elsewhere, pin the actual location.

- [ ] **Step 2: Write a failing test for coherence validation**

Pick an appropriate test file. If there's an existing AgentRequest validation test, append to it; otherwise create one. Suggested location: `crates/defra-agent/src/agent/document_view/apply.rs` (inline tests):

```rust
#[test]
fn validate_rejects_mixed_parent_linkage_request_id_only() {
    let req = AgentRequest {
        // ...minimal valid baseline...
        subagent_depth: 1,
        caused_by_parent_request_id: Some("parent-req-1".to_string()),
        caused_by_parent_tool_call_id: None,    // missing
        // ...
    };
    let result = validate_agent_request_subagent_coherence(&req);
    assert!(result.is_err());
}

#[test]
fn validate_rejects_mixed_parent_linkage_tool_call_id_only() {
    let req = AgentRequest {
        subagent_depth: 1,
        caused_by_parent_request_id: None,
        caused_by_parent_tool_call_id: Some("parent-tc-1".to_string()),
        // ...
    };
    assert!(validate_agent_request_subagent_coherence(&req).is_err());
}

#[test]
fn validate_rejects_subagent_depth_zero_with_parent_fields() {
    let req = AgentRequest {
        subagent_depth: 0,
        caused_by_parent_request_id: Some("parent-req-1".to_string()),
        caused_by_parent_tool_call_id: Some("parent-tc-1".to_string()),
        // ...
    };
    // depth=0 means top-level, but parent fields are set → incoherent.
    assert!(validate_agent_request_subagent_coherence(&req).is_err());
}

#[test]
fn validate_accepts_top_level_request() {
    let req = AgentRequest {
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_tool_call_id: None,
        // ...
    };
    assert!(validate_agent_request_subagent_coherence(&req).is_ok());
}

#[test]
fn validate_accepts_subagent_request() {
    let req = AgentRequest {
        subagent_depth: 1,
        caused_by_parent_request_id: Some("parent-req-1".to_string()),
        caused_by_parent_tool_call_id: Some("parent-tc-1".to_string()),
        // ...
    };
    assert!(validate_agent_request_subagent_coherence(&req).is_ok());
}
```

- [ ] **Step 3: Run, confirm fail, implement**

```bash
cargo test -p defra-agent --lib validate_rejects_mixed_parent_linkage
```

Expected: FAIL.

```rust
/// Coherence check on AgentRequest's subagent fields:
/// - caused_by_parent_request_id and caused_by_parent_tool_call_id must be
///   set together (both Some) or together (both None).
/// - subagent_depth = 0 ↔ both parent fields are None.
pub fn validate_agent_request_subagent_coherence(req: &AgentRequest) -> Result<()> {
    let has_parent_req = req.caused_by_parent_request_id.is_some();
    let has_parent_tc = req.caused_by_parent_tool_call_id.is_some();
    if has_parent_req != has_parent_tc {
        return Err(IllegalToolCallTransition::ParentLinkageIncoherent.into());
    }
    let is_top_level = !has_parent_req; // both None
    if is_top_level && req.subagent_depth != 0 {
        return Err(IllegalToolCallTransition::ParentLinkageIncoherent.into());
    }
    if !is_top_level && req.subagent_depth == 0 {
        return Err(IllegalToolCallTransition::ParentLinkageIncoherent.into());
    }
    Ok(())
}
```

Wire this function into the existing AgentRequest validation flow (probably called during `apply_control_update` or in the watcher's row materialization).

```bash
cargo test -p defra-agent --lib validate_
```

Expected: all five PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/agent/document_view/apply.rs
git commit -m "$(cat <<'EOF'
Add AgentRequest parent-linkage coherence validation

Apply-time check: caused_by_parent_request_id and
caused_by_parent_tool_call_id are set together or neither;
subagent_depth = 0 ↔ both parent fields are None.

Mixed states return IllegalToolCallTransition::ParentLinkageIncoherent.

Cross-reference validation (does parent_request_id point to a real
AgentRequest?) remains deferred to R3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 18: Implement `create_subagent_request` helper

Public API for creating parent-linked child AgentRequests. Used by R3's SubagentSource and by Bucket 3 conformance fixtures.

**Files:**
- Create: `crates/defra-agent/src/tool_call_lifecycle/subagent_request.rs`
- Modify: `crates/defra-agent/src/tool_call_lifecycle.rs` (re-export module)

- [ ] **Step 1: Create the new module file**

Write `crates/defra-agent/src/tool_call_lifecycle/subagent_request.rs`:

```rust
//! Helper for creating subagent-parent-linked AgentRequest rows.
//! Public API surface consumed by R3's SubagentSource and by Bucket 3
//! conformance fixtures.

use std::sync::Arc;
use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;
use crate::tool_call_lifecycle::IllegalToolCallTransition;

/// The configured cap on subagent recursion depth. Matches Lean's
/// `Subagent.maxSubagentDepth = 3`.
pub const MAX_SUBAGENT_DEPTH: u32 = 3;

/// Create a new AgentRequest with subagent parent linkage. Validates:
/// - parent_subagent_depth + 1 ≤ MAX_SUBAGENT_DEPTH (returns SubagentDepthExceeded)
/// - parent_request_id and parent_tool_call_id are both non-empty (returns
///   ParentLinkageIncoherent if either is empty)
///
/// Returns the new request_id (the unique identifier of the freshly-created
/// AgentRequest row).
pub async fn create_subagent_request(
    node: Arc<EmbeddedNode>,
    parent_request_id: String,
    parent_tool_call_id: String,
    parent_subagent_depth: u32,
    behavior_id: String,
    prompt: String,
    deadline: Option<chrono::DateTime<chrono::Utc>>,
    // Add additional fields here that the existing CREATE-AgentRequest flow expects
    // (e.g., agent_did, session_id, etc.). Read the existing creation flow in
    // crates/defra-agent/src/agent/ for the full param list.
) -> Result<String> {
    // 1. Depth check.
    if parent_subagent_depth + 1 > MAX_SUBAGENT_DEPTH {
        return Err(IllegalToolCallTransition::SubagentDepthExceeded.into());
    }

    // 2. Coherence check.
    if parent_request_id.is_empty() || parent_tool_call_id.is_empty() {
        return Err(IllegalToolCallTransition::ParentLinkageIncoherent.into());
    }

    // 3. Generate a fresh request_id (mirror existing pattern — likely uuid::Uuid).
    let new_request_id = format!("req-{}", uuid::Uuid::new_v4());
    let new_subagent_depth = parent_subagent_depth + 1;

    // 4. Build and execute the CREATE mutation.
    let escaped_prompt = crate::graphql::escape_graphql_string(&prompt);
    let deadline_str = deadline
        .map(|d| format!(r#", deadline: "{}""#, d.to_rfc3339()))
        .unwrap_or_default();

    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{rid}",
                behavior_id: "{bid}",
                prompt: "{prompt}",
                subagent_depth: {depth},
                caused_by_parent_request_id: "{prid}",
                caused_by_parent_tool_call_id: "{ptcid}",
                lifecycle_state: "pending"
                {deadline_str}
            }}) {{ _docID }}
        }}"#,
        rid = new_request_id,
        bid = behavior_id,
        prompt = escaped_prompt,
        depth = new_subagent_depth,
        prid = parent_request_id,
        ptcid = parent_tool_call_id,
        deadline_str = deadline_str,
    );

    // 5. Execute (mirror the existing GraphQL mutation pattern from R1's
    // ToolCallLifecycle::start_running for retry semantics, error handling).
    crate::graphql::execute_mutation(&node, &mutation).await?;

    Ok(new_request_id)
}
```

The exact list of additional CREATE-AgentRequest input fields varies by what already exists; read the production AgentRequest creation path (search `grep -rn "create_AgentRequest" crates/defra-agent/src/`) and match its full input shape.

- [ ] **Step 2: Add the `pub mod subagent_request;` line**

In `crates/defra-agent/src/tool_call_lifecycle.rs`, near the other module declarations:

```rust
pub mod subagent_request;
```

Re-export the helper at the crate root if R1 has a `pub use` pattern for `ToolCallLifecycle`:

```rust
pub use subagent_request::{create_subagent_request, MAX_SUBAGENT_DEPTH};
```

- [ ] **Step 3: Write the failing tests**

Append to `subagent_request.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_subagent_request_at_max_depth_succeeds() {
        let node = test_node().await;
        let new_id = create_subagent_request(
            node, "parent-req".to_string(), "parent-tc".to_string(),
            MAX_SUBAGENT_DEPTH - 1,    // ok: + 1 = MAX
            "behavior-1".to_string(), "test prompt".to_string(),
            None,
        ).await.unwrap();
        assert!(new_id.starts_with("req-"));
    }

    #[tokio::test]
    async fn create_subagent_request_above_max_depth_fails() {
        let node = test_node().await;
        let err = create_subagent_request(
            node, "parent-req".to_string(), "parent-tc".to_string(),
            MAX_SUBAGENT_DEPTH,    // not ok: + 1 > MAX
            "behavior-1".to_string(), "test prompt".to_string(),
            None,
        ).await.unwrap_err();
        assert!(matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::SubagentDepthExceeded)
        ));
    }

    #[tokio::test]
    async fn create_subagent_request_empty_parent_fields_fails() {
        let node = test_node().await;
        let err = create_subagent_request(
            node, "".to_string(), "parent-tc".to_string(),
            0, "behavior-1".to_string(), "p".to_string(), None,
        ).await.unwrap_err();
        assert!(matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::ParentLinkageIncoherent)
        ));
    }
}
```

- [ ] **Step 4: Run, confirm pass**

```bash
cargo test -p defra-agent --lib subagent_request::tests
```

Expected: all three PASS (the depth and coherence cases at minimum; the success case requires a working `test_node()` and a valid CREATE mutation, so adapt as needed).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle.rs \
        crates/defra-agent/src/tool_call_lifecycle/subagent_request.rs
git commit -m "$(cat <<'EOF'
Add create_subagent_request helper

Public API for creating parent-linked child AgentRequest rows.
Validates depth + 1 ≤ MAX_SUBAGENT_DEPTH (3) and parent linkage
coherence (both parent fields non-empty). Used by R3's SubagentSource
and by Bucket 3 conformance fixtures.

The MAX_SUBAGENT_DEPTH constant is exported as part of R2's public
API surface so R3's apply-time spawn-flow validation can reference
the same value as the Lean spec.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 19: Bucket 1 — vocabulary round-trip in-module tests

**Files:**
- Modify: `crates/defra-agent/src/tool_call_lifecycle.rs` (append to existing test module)

- [ ] **Step 1: Add the round-trip + closure tests**

In the test module of `tool_call_lifecycle.rs`:

```rust
#[cfg(test)]
mod bucket_1_subagent_vocabulary {
    use super::*;

    #[test]
    fn await_mode_round_trip_via_persisted_vocab() {
        for &mode in AwaitMode::ALL {
            assert_eq!(AwaitMode::from_persisted(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn await_mode_all_has_two_variants() {
        assert_eq!(AwaitMode::ALL.len(), 2);
    }

    #[test]
    fn await_mode_from_persisted_unknown_returns_none() {
        assert_eq!(AwaitMode::from_persisted("unknown"), None);
    }

    #[test]
    fn cancel_policy_round_trip_via_persisted_vocab() {
        for &policy in CancelPolicy::ALL {
            assert_eq!(CancelPolicy::from_persisted(policy.as_str()), Some(policy));
        }
    }

    #[test]
    fn cancel_policy_all_has_two_variants() {
        assert_eq!(CancelPolicy::ALL.len(), 2);
    }

    #[test]
    fn cancel_policy_from_persisted_unknown_returns_none() {
        assert_eq!(CancelPolicy::from_persisted("unknown"), None);
    }

    #[test]
    fn child_terminal_all_kind_has_four_variants() {
        assert_eq!(ChildTerminal::ALL_KIND.len(), 4);
        assert_eq!(
            ChildTerminal::ALL_KIND,
            &["failed", "dead", "interrupted", "superseded"]
        );
    }

    #[test]
    fn child_terminal_projection_partition() {
        // .interrupted → .cancelled; everything else → .failed
        assert_eq!(
            ChildTerminal::Failed {
                reason: "x".to_string(),
                failure_class: FailureClass::ExternalDependencyFailure
            }.projected_state(),
            ToolCallState::Failed
        );
        assert_eq!(ChildTerminal::Dead.projected_state(), ToolCallState::Failed);
        assert_eq!(ChildTerminal::Interrupted.projected_state(), ToolCallState::Cancelled);
        assert_eq!(ChildTerminal::Superseded.projected_state(), ToolCallState::Failed);
    }
}
```

- [ ] **Step 2: Run all eight tests**

```bash
cargo test -p defra-agent --lib bucket_1_subagent_vocabulary
```

Expected: all eight PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle.rs
git commit -m "$(cat <<'EOF'
Add Bucket 1 — vocabulary round-trip tests for new types

Eight in-module tests:
- AwaitMode and CancelPolicy round-trip (as_str ↔ from_persisted)
- AwaitMode::ALL.len() == 2; CancelPolicy::ALL.len() == 2
- ChildTerminal::ALL_KIND has 4 variants matching the spec
- ChildTerminal::projected_state partition (Interrupted →
  Cancelled, others → Failed) matches Lean B2

These run as part of `cargo test -p defra-agent --lib`.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 20: Bucket 2 — extend `state_machine_conformance.rs` for the new transitions

**Files:**
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs`

- [ ] **Step 1: Read R1's existing Bucket 2 structure**

```bash
sed -n '1,60p' crates/defra-agent/tests/state_machine_conformance.rs
```

Note how R1 consumes the Lean-emitted JSON (typically a const path or a `lean_vocab_test` helper module). The new transitions and vocabularies emitted in Task 0 should appear in that JSON.

- [ ] **Step 2: Add transition-matrix conformance tests**

Append to `state_machine_conformance.rs`:

```rust
#[test]
fn lean_emits_await_mode_vocabulary() {
    let contract = load_lean_contract();
    let machine = contract.machines.iter()
        .find(|m| m.name == "AwaitMode")
        .expect("Lean contract must emit AwaitMode machine");
    let mut rust_vocab: Vec<&str> = AwaitMode::ALL.iter().map(|m| m.as_str()).collect();
    rust_vocab.sort();
    let mut lean_vocab = machine.vocabulary.clone();
    lean_vocab.sort();
    assert_eq!(lean_vocab, rust_vocab,
        "AwaitMode vocabulary divergence between Lean and Rust");
}

#[test]
fn lean_emits_cancel_policy_vocabulary() {
    let contract = load_lean_contract();
    let machine = contract.machines.iter()
        .find(|m| m.name == "CancelPolicy")
        .expect("Lean contract must emit CancelPolicy machine");
    let mut rust_vocab: Vec<&str> = CancelPolicy::ALL.iter().map(|p| p.as_str()).collect();
    rust_vocab.sort();
    let mut lean_vocab = machine.vocabulary.clone();
    lean_vocab.sort();
    assert_eq!(lean_vocab, rust_vocab);
}

#[test]
fn lean_emits_child_terminal_vocabulary_and_projections() {
    let contract = load_lean_contract();
    let machine = contract.machines.iter()
        .find(|m| m.name == "ChildTerminal")
        .expect("Lean contract must emit ChildTerminal machine");
    // Vocabulary check
    let mut lean_vocab = machine.vocabulary.clone();
    lean_vocab.sort();
    let mut rust_vocab = ChildTerminal::ALL_KIND.to_vec();
    rust_vocab.sort();
    assert_eq!(lean_vocab, rust_vocab.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    // Projection check (each transition's `from`/`to` must match Rust's projected_state)
    for t in &machine.transitions {
        let rust_terminal = match t.from.as_str() {
            "failed" => ChildTerminal::Failed {
                reason: "x".to_string(),
                failure_class: FailureClass::ExternalDependencyFailure,
            },
            "dead" => ChildTerminal::Dead,
            "interrupted" => ChildTerminal::Interrupted,
            "superseded" => ChildTerminal::Superseded,
            _ => panic!("unexpected ChildTerminal vocabulary: {}", t.from),
        };
        let projected = rust_terminal.projected_state();
        assert_eq!(projected.as_str(), t.to,
            "Projection divergence: {} → Rust says {}, Lean says {}",
            t.from, projected.as_str(), t.to);
    }
}

#[test]
fn lean_emits_bridge_transitions_in_tool_call_machine() {
    let contract = load_lean_contract();
    let machine = contract.machines.iter()
        .find(|m| m.name == "ToolCall")
        .expect("Lean contract must emit ToolCall machine");
    let bridge_names = vec![
        "background", "foreground", "detach",
        "bridge_complete", "bridge_failure", "bridge_cancel_cascade"
    ];
    for name in &bridge_names {
        let found = machine.transitions.iter().any(|t| t.transition_name == *name);
        assert!(found, "Lean contract must emit '{}' transition in toolCallMachine", name);
    }
}

#[test]
fn lean_marks_native_complete_fail_as_requires_native() {
    let contract = load_lean_contract();
    let machine = contract.machines.iter()
        .find(|m| m.name == "ToolCall")
        .expect("Lean contract must emit ToolCall machine");
    let complete = machine.transitions.iter()
        .find(|t| t.transition_name == "complete")
        .expect("toolCallMachine must have native complete transition");
    assert!(complete.requires_native.unwrap_or(false),
        "native complete must be flagged with requires_native: true");
    let fail = machine.transitions.iter()
        .find(|t| t.transition_name == "fail")
        .expect("toolCallMachine must have native fail transition");
    assert!(fail.requires_native.unwrap_or(false));
}
```

(Adapt `load_lean_contract()`, `Machine`, `Transition` field names to whatever R1's `state_machine_conformance.rs` already uses — these placeholders need to match.)

- [ ] **Step 3: Run the tests**

```bash
cargo test -p defra-agent --test state_machine_conformance
```

Expected: all five PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/tests/state_machine_conformance.rs
git commit -m "$(cat <<'EOF'
Add Bucket 2 — Lean transition matrix conformance for subagent

Five tests assert Lean's emitted contract includes:
- AwaitMode vocabulary matches Rust's AwaitMode::ALL
- CancelPolicy vocabulary matches Rust's CancelPolicy::ALL
- ChildTerminal vocabulary + projection rules match Rust's
  ChildTerminal::ALL_KIND and projected_state()
- toolCallMachine includes background/foreground/detach,
  bridge_complete/failure/cancel_cascade transitions
- Native complete/fail transitions are flagged requires_native

These tests catch any drift between Lean's structural model and
Rust's runtime implementation.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 21: Bucket 3 — `make_completed_request` test helper

**Files:**
- Create: `crates/defra-agent/tests/tool_call_subagent_lifecycle_conformance.rs`

- [ ] **Step 1: Create the new test file**

Write `crates/defra-agent/tests/tool_call_subagent_lifecycle_conformance.rs`:

```rust
//! Bucket 3 — runtime integration tests for the subagent extensions to
//! ToolCallLifecycle. Spins up a real EmbeddedNode via test_db() and exercises
//! every new transition end-to-end. Mirrors R1's
//! tool_call_lifecycle_conformance.rs structure.

use std::sync::Arc;
use defra_agent::tool_call_lifecycle::{
    AwaitMode, CancelPolicy, CascadeIntent, ChildTerminal, FailureClass,
    IllegalToolCallTransition, ToolCallLifecycle, ToolCallState,
    create_subagent_request, MAX_SUBAGENT_DEPTH,
};
use defra_node::EmbeddedNode;

mod common;
use common::{test_db, fetch_tool_call_snapshots_for_session};

/// Test helper: directly constructs a child AgentRequest in `.completed`
/// state via low-level DB writes, bypassing the normal request lifecycle.
/// Used by bridge_complete tests to set up "the child has finished" state
/// without R3's SubagentSource.
async fn make_completed_request(
    node: Arc<EmbeddedNode>,
    request_id: &str,
    parent_request_id: Option<&str>,
    parent_tool_call_id: Option<&str>,
    final_message: &str,
) -> anyhow::Result<()> {
    let parent_req_field = parent_request_id
        .map(|id| format!(r#", caused_by_parent_request_id: "{}""#, id))
        .unwrap_or_default();
    let parent_tc_field = parent_tool_call_id
        .map(|id| format!(r#", caused_by_parent_tool_call_id: "{}""#, id))
        .unwrap_or_default();
    let depth = if parent_request_id.is_some() { 1 } else { 0 };
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{rid}",
                lifecycle_state: "completed",
                subagent_depth: {depth},
                final_message: "{msg}"
                {prf}
                {ptc}
            }}) {{ _docID }}
        }}"#,
        rid = request_id,
        depth = depth,
        msg = defra_agent::graphql::escape_graphql_string(final_message),
        prf = parent_req_field,
        ptc = parent_tc_field,
    );
    defra_agent::graphql::execute_mutation(&node, &mutation).await?;
    Ok(())
}

/// Test helper: same but for non-completed terminal states.
async fn make_terminal_request(
    node: Arc<EmbeddedNode>,
    request_id: &str,
    parent_request_id: Option<&str>,
    parent_tool_call_id: Option<&str>,
    state: &str,    // "failed", "dead", "interrupted", "superseded"
) -> anyhow::Result<()> {
    let parent_req_field = parent_request_id
        .map(|id| format!(r#", caused_by_parent_request_id: "{}""#, id))
        .unwrap_or_default();
    let parent_tc_field = parent_tool_call_id
        .map(|id| format!(r#", caused_by_parent_tool_call_id: "{}""#, id))
        .unwrap_or_default();
    let depth = if parent_request_id.is_some() { 1 } else { 0 };
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{rid}",
                lifecycle_state: "{state}",
                subagent_depth: {depth}
                {prf}
                {ptc}
            }}) {{ _docID }}
        }}"#,
        rid = request_id,
        state = state,
        depth = depth,
        prf = parent_req_field,
        ptc = parent_tc_field,
    );
    defra_agent::graphql::execute_mutation(&node, &mutation).await?;
    Ok(())
}

#[tokio::test]
async fn test_make_completed_request_creates_row() {
    let (node, _td) = test_db().await;
    make_completed_request(
        node.clone(), "req-test-1", None, None, "all done"
    ).await.unwrap();
    // Sanity: a row exists. Adapt the verification query to the real DB API.
    let row = defra_agent::graphql::execute_query(
        &node,
        r#"query { AgentRequest(request_id: "req-test-1") { request_id lifecycle_state } }"#
    ).await.unwrap();
    assert!(row.contains("\"completed\""), "expected the request to be in completed state");
}
```

(`common::test_db` and `fetch_tool_call_snapshots_for_session` are placeholders — these likely exist in R1's `tool_call_lifecycle_conformance.rs`. Either reuse via `mod common;` or copy the relevant bits if they're not already extracted.)

- [ ] **Step 2: Run the helper test**

```bash
cargo test -p defra-agent --test tool_call_subagent_lifecycle_conformance
```

Expected: PASS for `test_make_completed_request_creates_row`.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/tests/tool_call_subagent_lifecycle_conformance.rs
git commit -m "$(cat <<'EOF'
Bucket 3 scaffolding — make_completed_request and make_terminal_request helpers

Two test helpers that construct child AgentRequest rows in arbitrary
terminal states via direct DB writes. Used by subsequent bridge_complete
and bridge_failure integration tests to set up "the child has finished"
state without R3's SubagentSource.

Subsequent tasks add the actual integration tests (mode flips, detach,
bridge_complete, bridge_failure projections, cascade intent, migration
round-trip).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 22: Bucket 3 — mode-flip + detach end-to-end tests

**Files:**
- Modify: `crates/defra-agent/tests/tool_call_subagent_lifecycle_conformance.rs`

- [ ] **Step 1: Add the mode-flip integration tests**

Append to the file:

```rust
#[tokio::test]
async fn integration_background_then_foreground_persists_round_trip() {
    let (node, _td) = test_db().await;
    let mut lc = ToolCallLifecycle::new_subagent(
        node.clone(),
        "sess-int-1".to_string(),
        "tc-int-1".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-req-int-1".to_string(),
    );
    lc.start_running().await.unwrap();
    lc.background().await.unwrap();
    // Verify persisted state.
    let snapshots = fetch_tool_call_snapshots_for_session(&node, "sess-int-1").await.unwrap();
    let row = snapshots.first().unwrap();
    assert_eq!(row.await_mode.as_deref(), Some("background"));
    // Flip back.
    lc.foreground().await.unwrap();
    let snapshots = fetch_tool_call_snapshots_for_session(&node, "sess-int-1").await.unwrap();
    let row = snapshots.first().unwrap();
    assert_eq!(row.await_mode.as_deref(), Some("foreground"));
    // Calling foreground() again returns ModeAlreadyForeground.
    let err = lc.foreground().await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<IllegalToolCallTransition>(),
        Some(IllegalToolCallTransition::ModeAlreadyForeground)
    ));
}

#[tokio::test]
async fn integration_detach_one_way_persists() {
    let (node, _td) = test_db().await;
    let mut lc = ToolCallLifecycle::new_subagent(
        node.clone(),
        "sess-det-1".to_string(),
        "tc-det-1".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-req-det-1".to_string(),
    );
    lc.start_running().await.unwrap();
    lc.detach().await.unwrap();
    let snapshots = fetch_tool_call_snapshots_for_session(&node, "sess-det-1").await.unwrap();
    assert_eq!(snapshots.first().unwrap().cancel_policy.as_deref(), Some("detach"));
    // detach again errors.
    let err = lc.detach().await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<IllegalToolCallTransition>(),
        Some(IllegalToolCallTransition::PolicyAlreadyDetach)
    ));
}
```

(The `fetch_tool_call_snapshots_for_session` helper from R1 may need extending to read the new `await_mode` and `cancel_policy` fields. Check its definition and add the field if missing.)

- [ ] **Step 2: Run, confirm pass**

```bash
cargo test -p defra-agent --test tool_call_subagent_lifecycle_conformance integration_background
cargo test -p defra-agent --test tool_call_subagent_lifecycle_conformance integration_detach
```

Expected: both PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/tests/tool_call_subagent_lifecycle_conformance.rs
git commit -m "$(cat <<'EOF'
Bucket 3 — mode-flip and detach end-to-end tests

Two integration tests exercising background/foreground/detach via real
EmbeddedNode round-trips. Verifies persisted await_mode and
cancel_policy match in-memory state and that one-way constraints fire
correctly (ModeAlreadyForeground, PolicyAlreadyDetach).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 23: Bucket 3 — `bridge_complete` end-to-end test

**Files:**
- Modify: `crates/defra-agent/tests/tool_call_subagent_lifecycle_conformance.rs`

- [ ] **Step 1: Add the test**

```rust
#[tokio::test]
async fn integration_bridge_complete_with_real_child() {
    let (node, _td) = test_db().await;
    // 1. Spawn a real child request via create_subagent_request.
    let child_request_id = create_subagent_request(
        node.clone(),
        "parent-req-bc1".to_string(),
        "parent-tc-bc1".to_string(),
        0,    // parent depth
        "behavior-bc1".to_string(),
        "child prompt".to_string(),
        None,
    ).await.unwrap();
    // 2. Force the child to .completed state via direct DB write.
    make_completed_request(
        node.clone(),
        &child_request_id,
        Some("parent-req-bc1"),
        Some("parent-tc-bc1"),
        "child final assistant message",
    ).await.unwrap();
    // (The above call may be a no-op or upsert depending on
    // make_completed_request semantics; if it fails on duplicate, alter
    // the helper to UPDATE on conflict, or reorganize the test.)

    // 3. Construct the parent bridge tool call and start_running.
    let mut bridge = ToolCallLifecycle::new_subagent(
        node.clone(),
        "sess-bc1".to_string(),
        "tc-bc1".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        child_request_id,
    );
    bridge.start_running().await.unwrap();

    // 4. Call bridge_complete with the projected child output.
    let projected_output = "child final assistant message".to_string();
    bridge.bridge_complete(projected_output.clone()).await.unwrap();

    // 5. Verify the bridge tool's persisted state and result.
    let snapshots = fetch_tool_call_snapshots_for_session(&node, "sess-bc1").await.unwrap();
    let row = snapshots.first().unwrap();
    assert_eq!(row.lifecycle_state.as_deref(), Some("completed"));
    assert_eq!(row.result.as_deref(), Some(projected_output.as_str()));
    assert_eq!(row.child_request_id, bridge.child_request_id);
}
```

- [ ] **Step 2: Run, confirm pass**

```bash
cargo test -p defra-agent --test tool_call_subagent_lifecycle_conformance integration_bridge_complete
```

Expected: PASS. Adjust helper functions if Step 5's snapshot reads need new fields.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/tests/tool_call_subagent_lifecycle_conformance.rs
git commit -m "$(cat <<'EOF'
Bucket 3 — bridge_complete end-to-end with real child

Spawns a real child via create_subagent_request, forces it to
.completed via make_completed_request, then exercises bridge_complete
on a parent tool that's linked to that child. Verifies the parent
tool's persisted lifecycle_state, result, and child_request_id all
reflect the bridge transition.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 24: Bucket 3 — `bridge_failure` projection tests

**Files:**
- Modify: `crates/defra-agent/tests/tool_call_subagent_lifecycle_conformance.rs`

- [ ] **Step 1: Add the four projection tests**

```rust
async fn run_bridge_failure_case(
    terminal_state: &str,
    child_terminal: ChildTerminal,
    expected_tool_state: ToolCallState,
) {
    let (node, _td) = test_db().await;
    let session = format!("sess-bf-{}", terminal_state);
    let tc_id = format!("tc-bf-{}", terminal_state);
    let child_id = format!("child-req-bf-{}", terminal_state);
    let parent_req = format!("parent-req-bf-{}", terminal_state);
    let parent_tc = format!("parent-tc-bf-{}", terminal_state);

    make_terminal_request(
        node.clone(), &child_id, Some(&parent_req), Some(&parent_tc),
        terminal_state,
    ).await.unwrap();

    let mut bridge = ToolCallLifecycle::new_subagent(
        node.clone(), session.clone(), tc_id.clone(), 1,
        "spawn_subagent".to_string(), "{}".to_string(),
        AwaitMode::Foreground, CancelPolicy::Cascade,
        child_id,
    );
    bridge.start_running().await.unwrap();
    bridge.bridge_failure(child_terminal).await.unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&node, &session).await.unwrap();
    assert_eq!(
        snapshots.first().unwrap().lifecycle_state.as_deref(),
        Some(expected_tool_state.as_str())
    );
}

#[tokio::test]
async fn integration_bridge_failure_failed_projects_to_failed() {
    run_bridge_failure_case(
        "failed",
        ChildTerminal::Failed {
            reason: "child failed".to_string(),
            failure_class: FailureClass::ExternalDependencyFailure,
        },
        ToolCallState::Failed,
    ).await;
}

#[tokio::test]
async fn integration_bridge_failure_dead_projects_to_failed() {
    run_bridge_failure_case("dead", ChildTerminal::Dead, ToolCallState::Failed).await;
}

#[tokio::test]
async fn integration_bridge_failure_interrupted_projects_to_cancelled() {
    run_bridge_failure_case(
        "interrupted",
        ChildTerminal::Interrupted,
        ToolCallState::Cancelled,
    ).await;
}

#[tokio::test]
async fn integration_bridge_failure_superseded_projects_to_failed() {
    run_bridge_failure_case(
        "superseded",
        ChildTerminal::Superseded,
        ToolCallState::Failed,
    ).await;
}
```

- [ ] **Step 2: Run, confirm pass**

```bash
cargo test -p defra-agent --test tool_call_subagent_lifecycle_conformance integration_bridge_failure
```

Expected: all four PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/tests/tool_call_subagent_lifecycle_conformance.rs
git commit -m "$(cat <<'EOF'
Bucket 3 — bridge_failure projection tests for all 4 child terminals

Verifies B2's projection rule end-to-end:
- ChildTerminal::Failed/Dead/Superseded → tool_state = Failed
- ChildTerminal::Interrupted → tool_state = Cancelled

Each test sets up a child in the corresponding terminal state via
make_terminal_request and then drives bridge_failure on a parent
tool linked to it.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 25: Bucket 3 — cascade intent tests

**Files:**
- Modify: `crates/defra-agent/tests/tool_call_subagent_lifecycle_conformance.rs`

- [ ] **Step 1: Add the three cascade scenario tests**

```rust
#[tokio::test]
async fn integration_cascade_intent_for_cascade_subagent_returns_some() {
    let (node, _td) = test_db().await;
    let mut lc = ToolCallLifecycle::new_subagent(
        node.clone(),
        "sess-cas-1".to_string(),
        "tc-cas-1".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-req-cas-1".to_string(),
    );
    lc.start_running().await.unwrap();
    lc.cancel_during_run().await.unwrap();
    let intent = lc.bridge_cancel_cascade().await.unwrap();
    assert!(intent.is_some());
    assert_eq!(intent.unwrap().child_request_id, "child-req-cas-1");
}

#[tokio::test]
async fn integration_cascade_intent_for_detached_subagent_returns_none() {
    let (node, _td) = test_db().await;
    let mut lc = ToolCallLifecycle::new_subagent(
        node.clone(),
        "sess-cas-2".to_string(),
        "tc-cas-2".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        AwaitMode::Foreground,
        CancelPolicy::Detach,
        "child-req-cas-2".to_string(),
    );
    lc.start_running().await.unwrap();
    lc.cancel_during_run().await.unwrap();
    let intent = lc.bridge_cancel_cascade().await.unwrap();
    assert!(intent.is_none());
}

#[tokio::test]
async fn integration_cascade_intent_for_native_returns_none() {
    let (node, _td) = test_db().await;
    let mut lc = ToolCallLifecycle::new(
        node.clone(),
        "sess-cas-3".to_string(),
        "tc-cas-3".to_string(),
        1,
        "echo".to_string(),
        "{}".to_string(),
    );
    lc.start_running().await.unwrap();
    lc.cancel_during_run().await.unwrap();
    let intent = lc.bridge_cancel_cascade().await.unwrap();
    assert!(intent.is_none());
}
```

- [ ] **Step 2: Run, confirm pass**

```bash
cargo test -p defra-agent --test tool_call_subagent_lifecycle_conformance integration_cascade_intent
```

Expected: all three PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/tests/tool_call_subagent_lifecycle_conformance.rs
git commit -m "$(cat <<'EOF'
Bucket 3 — cascade intent integration tests

Three end-to-end scenarios for bridge_cancel_cascade:
- Cascade-policy subagent returns Some(CascadeIntent)
- Detach-policy subagent returns None (no cascade)
- Native tool (no child link) returns None

Verifies the pure return-intent semantics work correctly when the
underlying lifecycle has actually been driven through cancel_during_run.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 26: Bucket 3 — migration round-trip and `create_subagent_request` end-to-end

**Files:**
- Modify: `crates/defra-agent/tests/tool_call_subagent_lifecycle_conformance.rs`

- [ ] **Step 1: Add the migration round-trip test**

```rust
#[tokio::test]
async fn integration_migration_round_trip_populates_defaults() {
    // Use a v2 baseline test_db (one without our v3 migration applied).
    // If R1's test_db() is post-migration, we need a v2 fixture; create one
    // by directly applying R1's v1→v2 patch only.
    let (node, _td) = setup_v2_only_node().await;

    // Insert a v2-shape AgentToolCall row (no v3 fields).
    let create_v2 = r#"
        mutation {
            create_AgentToolCall(input: {
                tool_call_key: "v2-row-1",
                session_id: "v2-sess-1",
                message_sequence: 1,
                tool_name: "echo",
                tool_call_id: "v2-tc-1",
                args: "{}",
                lifecycle_state: "running"
            }) { _docID }
        }
    "#;
    defra_agent::graphql::execute_mutation(&node, create_v2).await.unwrap();

    // Apply the v2→v3 migration.
    defra_agent::migration::ensure_subagent_extensions_migrations(node.clone()).await.unwrap();

    // Read the row and verify defaults.
    let row = defra_agent::graphql::execute_query(
        &node,
        r#"query {
            AgentToolCall(tool_call_key: "v2-row-1") {
                await_mode
                cancel_policy
                child_request_id
                request_id
            }
        }"#,
    ).await.unwrap();
    assert!(row.contains("\"foreground\""), "await_mode should default to 'foreground'");
    assert!(row.contains("\"cascade\""), "cancel_policy should default to 'cascade'");
    // child_request_id and request_id remain null on migrated rows.
}
```

(`setup_v2_only_node()` is a new helper that creates a node, applies only R1's `ensure_tool_call_migrations` (and any v1→v2 baseline), and returns it. Add this helper to `common::` or inline.)

- [ ] **Step 2: Add the create_subagent_request end-to-end depth + coherence tests**

```rust
#[tokio::test]
async fn integration_create_subagent_request_at_max_depth_succeeds() {
    let (node, _td) = test_db().await;
    let new_id = create_subagent_request(
        node, "parent-req-csr-1".to_string(), "parent-tc-csr-1".to_string(),
        MAX_SUBAGENT_DEPTH - 1,
        "behavior-csr-1".to_string(), "csr test prompt".to_string(),
        None,
    ).await.unwrap();
    assert!(new_id.starts_with("req-"));
}

#[tokio::test]
async fn integration_create_subagent_request_above_max_depth_fails() {
    let (node, _td) = test_db().await;
    let err = create_subagent_request(
        node, "parent-req-csr-2".to_string(), "parent-tc-csr-2".to_string(),
        MAX_SUBAGENT_DEPTH,
        "behavior-csr-2".to_string(), "csr test prompt".to_string(),
        None,
    ).await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<IllegalToolCallTransition>(),
        Some(IllegalToolCallTransition::SubagentDepthExceeded)
    ));
}

#[tokio::test]
async fn integration_create_subagent_request_empty_parent_fields_fails() {
    let (node, _td) = test_db().await;
    let err = create_subagent_request(
        node, "".to_string(), "parent-tc".to_string(), 0,
        "behavior".to_string(), "prompt".to_string(), None,
    ).await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<IllegalToolCallTransition>(),
        Some(IllegalToolCallTransition::ParentLinkageIncoherent)
    ));
}
```

- [ ] **Step 3: Run all the integration tests**

```bash
cargo test -p defra-agent --test tool_call_subagent_lifecycle_conformance
```

Expected: every test in the file PASSES.

- [ ] **Step 4: Run the full Rust test suite to confirm no regressions**

```bash
cargo test -p defra-agent
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/tests/tool_call_subagent_lifecycle_conformance.rs
git commit -m "$(cat <<'EOF'
Bucket 3 — migration round-trip and create_subagent_request integration

Migration test: insert a v2 AgentToolCall row into a v2-only node,
apply ensure_subagent_extensions_migrations, verify the row's new
fields default to "foreground" / "cascade" / null per the spec.

create_subagent_request integration tests: at depth ≤ max-1 succeeds,
at depth = max exceeds, empty parent fields rejected.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 27: Final verification + spec coverage audit

**Files:**
- (no file changes — verification + audit task)

- [ ] **Step 1: Run the full test suite**

```bash
cargo test -p defra-agent --lib --tests
cargo test -p defra-agent --tests
cargo build --release -p defra-agent
```

Expected: all PASS, clean release build.

- [ ] **Step 2: Verify Lean still builds**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean (no `sorry`, no errors). Task 0's contract additions should compile cleanly here.

- [ ] **Step 3: Verify the WASM lens artifact builds**

```bash
cargo build --release --target wasm32-unknown-unknown -p agent-subagent-v2-to-v3-lens
wc -c target/wasm32-unknown-unknown/release/agent_subagent_v2_to_v3_lens.wasm
```

Expected: builds; record the size.

- [ ] **Step 4: Spec coverage audit**

For each section of `docs/superpowers/specs/2026-05-08-r2-rust-subagent-data-plane-design.md`, verify a corresponding task implemented it:

| Spec section | Task |
|---|---|
| Conformance prerequisite (Lean Machines.lean) | 0 |
| Lens crate scaffold | 1 |
| Lens transforms | 2 |
| GraphQL schemas + JSON Patches | 3 |
| Migration orchestrator | 4 |
| Daemon startup wiring | 5 |
| AwaitMode/CancelPolicy/ChildTerminal/CascadeIntent types | 6 |
| ToolCallLifecycle struct + new_subagent + IllegalToolCallTransition variants | 7 |
| Symmetric h_native guards on complete/fail | 8 |
| Mode-flip transitions (background, foreground, detach) | 9, 10, 11 |
| Bridge transitions (bridge_complete/failure/cancel_cascade) | 12, 13, 14 |
| AgentRequestRow + AgentRequest extensions | 15 |
| ToolSelectionDocument extensions + apply-time validation | 16 |
| AgentRequest parent-linkage coherence validation | 17 |
| create_subagent_request helper | 18 |
| Bucket 1 vocabulary round-trip | 19 |
| Bucket 2 Lean transition matrix | 20 |
| Bucket 3 helpers + integration tests | 21, 22, 23, 24, 25, 26 |

Document any gaps inline (mark a TODO comment in the relevant Rust file with a follow-up issue).

- [ ] **Step 5: Add a maintenance tracker**

Append to `crates/defra-agent/src/tool_call_lifecycle.rs` (top of file, in module-level docs):

```rust
//! ## R2 maintenance obligations
//!
//! This module implements R2 ("Rust subagent data plane"). Per the spec at
//! `docs/superpowers/specs/2026-05-08-r2-rust-subagent-data-plane-design.md`:
//!
//! - SubagentSource (R3) consumes `create_subagent_request` and the bridge methods.
//! - Agent-facing tools (R4) are routed via hook integration that uses
//!   `new_subagent` and recognizes spawn_subagent / wait_task / etc. tool names.
//! - Cross-reference validation (target resolution, parent existence) lands in R3.
//! - Cross-principal delegation (R6) lands with sourcenetwork/defra-agent#9.
```

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/tool_call_lifecycle.rs
git commit -m "$(cat <<'EOF'
R2 final verification + maintenance tracker

Adds module-level documentation tracking R2's deferrals to R3, R4, R6,
and #9. All 27 R2 tasks landed; full Rust + Lean test suites pass; WASM
lens artifact builds cleanly.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review Notes

Spec sections each map to a task per the audit table in Task 27. Key non-coverage callouts:

- **Hook integration (`hook/persistence.rs`)**: explicitly deferred to R3 per the spec. Bucket 3 tests bypass the hook layer and call lifecycle methods directly.
- **`SubagentSource` registration in TriggerEngine**: deferred to R3.
- **Cross-reference validation** (`subagent_targets` resolution, `caused_by_parent_request_id` existence): deferred to R3.
- **The seven agent-facing tools** (`spawn_subagent`, etc.): deferred to R4.
- **Cross-principal delegation**: deferred to R6 (lands with #9).

These deferrals are consistent with R1's discipline of landing data plane before runtime wiring, and the data plane's API surface (especially `create_subagent_request`, `new_subagent`, the bridge methods, and `CascadeIntent`) is shaped to support R3's spawn flow without forcing changes back into R2.

The 27 tasks total ~5-7 days of focused engineering at sustainable cadence (matches R1's delivery profile of similar size).
