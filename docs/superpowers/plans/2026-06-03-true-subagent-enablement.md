# True Subagent Enablement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Take the already-built, Lean-verified subagent path over the line to testing — make it enable-able through `config apply`, surface allowed targets to the agent, converge the local/remote spawn into one document-write path, and validate it up a ladder (local → simulated fleet → real fleet).

**Architecture:** The runtime/server owns execution; views are read-only. Subagents are already wired (`SubagentSource`, the persistence hook, `Background.lean`). This plan fixes the enablement config gap, adds operator validation, makes statically-allowed targets self-describing, and unifies the spawn write-path. No new execution semantics; no new Lean modules; no new collections.

**Tech Stack:** Rust (workspace), DefraDB `EmbeddedNode`, rig-core, GraphQL (inline strings, always via `graphql::escape_graphql_string`), `cargo test` integration tests against a live embedded node, Lean 4 (untouched here).

**Spec:** `docs/superpowers/specs/2026-06-03-true-subagent-enablement-design.md`

---

## Conventions for the executor

- Run a single test: `cargo test -p <crate> -- <test_name> --nocapture`.
- Integration tests for the runtime live in `crates/defra-agent/tests/` and use the `support` harness (`test_db`, `boot_agent_*`, `create_runtime_request`, `MockModelEndpoint`). Two-node harnesses already exist: `tests/support/r5_conformance/runner.rs` and `tests/support/pairing_conformance/runner.rs` (both expose `start_two_nodes()`).
- Always interpolate user/config strings into GraphQL via `escape_graphql_string`.
- Commit after each task with the regression gate green: `cargo test -p defra-agent`, `cargo clippy --all-targets`, `cargo fmt --all`.
- The 17 tests in `tests/subagent_source_conformance.rs` and the `tests/r4_subagent_tools/` suite are the regression gate for anything touching the spawn path. They must stay green.

## File structure (what each task touches)

| File | Responsibility | Task |
|---|---|---|
| `crates/defra-agent-cli/src/config_writes/tool_selection.rs` | Serialize subagent fields on `config apply` | 1 |
| `crates/defra-agent-cli/src/desired_state/validate.rs` | Validate `subagent_targets` resolve to behaviors | 2 |
| `crates/defra-agent/tests/subagent_enablement_e2e.rs` (new) | Tier-1 local enable→spawn→list E2E | 3 |
| `crates/defra-agent-protocol/schemas/agent/agent_behavior.graphql` | Add `description`/`summary` fields | 4 |
| `crates/defra-agent/src/document_config/behavior.rs` | Parse new fields from the behavior document | 4 |
| `crates/defra-agent/src/prompt.rs` | Inject allowed targets + descriptions into preamble | 5 |
| `crates/defra-agent/src/hook/persistence/message_spawn.rs`, `subagent_bridge.rs` | Converge local/remote spawn write-path | 6 |
| `docs/superpowers/plans/...` (5–6) | Simulated-fleet + real-fleet validation (stubs) | 7, 8 |

---

## Task 1: Serialize subagent fields in the `config apply` write path (C1)

**Bug:** `write_tool_selection_document` builds field strings via `tool_selection_fields()` (`config_writes/tool_selection.rs:34-95`), which **omits** `subagent_targets`, `subagent_spawn_enabled`, `subagent_steering_enabled`, `subagent_background_enabled`, and `cross_deployment_spawn_timeout_seconds`. Applying a manifest silently drops them, so subagents can never be enabled via `config apply`.

**Files:**
- Modify: `crates/defra-agent-cli/src/config_writes/tool_selection.rs:34-95`
- Test: `crates/defra-agent-cli/src/config_writes/tool_selection.rs` (inline `#[cfg(test)] mod tests`) — or the crate's existing apply round-trip test module if present.

- [ ] **Step 1: Write the failing test** — a round-trip: write a `ToolSelectionDocument` with subagents enabled, read it back, assert the fields persisted.

```rust
#[cfg(test)]
mod subagent_field_tests {
    use super::*;
    use crate::test_support::config_access_for_test; // existing helper; if named differently, use the crate's standard one

    #[tokio::test]
    async fn apply_persists_subagent_enablement_fields() {
        let access = config_access_for_test("toolsel-subagent-fields").await;
        let selection = ToolSelectionDocument {
            selection_id: "sel-1".to_string(),
            agent_did: "did:key:test".to_string(),
            subagent_spawn_enabled: Some(true),
            subagent_targets: Some(vec!["amy-research".to_string()]),
            subagent_steering_enabled: Some(true),
            subagent_background_enabled: Some(true),
            cross_deployment_spawn_timeout_seconds: Some(90),
            ..Default::default()
        };

        write_tool_selection_document(&access, &selection).await.unwrap();

        let read = access
            .execute(r#"{ ToolSelection(filter: { selection_id: { _eq: "sel-1" } }) { subagent_spawn_enabled subagent_targets subagent_steering_enabled subagent_background_enabled cross_deployment_spawn_timeout_seconds } }"#)
            .await
            .unwrap();
        let row = &read.data.as_ref().unwrap()["ToolSelection"][0];
        assert_eq!(row["subagent_spawn_enabled"], serde_json::json!(true));
        assert_eq!(row["subagent_targets"], serde_json::json!(["amy-research"]));
        assert_eq!(row["subagent_steering_enabled"], serde_json::json!(true));
        assert_eq!(row["subagent_background_enabled"], serde_json::json!(true));
        assert_eq!(row["cross_deployment_spawn_timeout_seconds"], serde_json::json!(90));
    }
}
```

> If `config_access_for_test` does not exist under that name, locate the existing test helper that constructs a `ConfigAccess` over an embedded test node (grep `ConfigAccess` in `crates/defra-agent-cli/src`), and use it. Do not invent a new harness.

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p defra-agent-cli -- apply_persists_subagent_enablement_fields --nocapture`
Expected: FAIL — the read-back fields are null/absent because `tool_selection_fields()` never emits them.

- [ ] **Step 3: Add the field serialization to `tool_selection_fields()`**

In `tool_selection_fields()` (`config_writes/tool_selection.rs:34-95`), follow the **existing pattern in that function** for optional scalars and `[String]` fields (e.g. how `allowed_mcp_service_ids` / `backgroundable_tool_names` / `command_network_mode` are conditionally emitted). Add emission for each missing field:
- `subagent_spawn_enabled: Option<bool>` → emit `subagent_spawn_enabled: {bool}` when `Some`.
- `subagent_steering_enabled`, `subagent_background_enabled` → same boolean pattern.
- `subagent_targets: Option<Vec<String>>` → emit a GraphQL string array, escaping each element with `escape_graphql_string` (mirror the existing list-field emission already in this function).
- `cross_deployment_spawn_timeout_seconds: Option<u32>` → emit `cross_deployment_spawn_timeout_seconds: {n}` when `Some`.

Use the same `include_id`/add-vs-update split the function already uses (these are desired-state fields, so they belong in both the `add` and `update` field sets).

- [ ] **Step 4: Run the test and confirm it passes**

Run: `cargo test -p defra-agent-cli -- apply_persists_subagent_enablement_fields --nocapture`
Expected: PASS.

- [ ] **Step 5: Regression + commit**

```bash
cargo test -p defra-agent-cli && cargo clippy --all-targets && cargo fmt --all
git add crates/defra-agent-cli/src/config_writes/tool_selection.rs
git commit -m "fix(cli): persist subagent enablement fields on config apply (#377)"
```

---

## Task 2: Validate `subagent_targets` resolve to known behaviors (C1)

**Goal:** Apply-time validation so an operator enabling subagents with a typo'd target gets a clear error instead of silently-inert tools. There are two validation layers (`desired_state/validate.rs`): structural (lines 75-138, sync) and live (lines 444-562, async, queries the DB). Target resolution needs the live layer.

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/validate.rs` — structural section (~75-138) for empty-string checks; `validate_manifest_against_live` (~444-562) for behavior resolution.
- Test: existing validate test module in that file (grep `mod tests` / `validate_manifest`), or a new `#[cfg(test)] mod subagent_target_validation`.

- [ ] **Step 1: Write the failing structural test** — empty target rejected.

```rust
#[test]
fn empty_subagent_target_is_rejected() {
    let mut manifest = minimal_valid_manifest(); // existing test helper in this module
    manifest.tool_selections[0].subagent_spawn_enabled = Some(true);
    manifest.tool_selections[0].subagent_targets = Some(vec!["".to_string()]);
    let errors = validate_manifest_structural(&manifest); // existing structural entry point; match its real name
    assert!(errors.iter().any(|e| e.contains("subagent_targets")));
}
```

> Match the real names of the structural-validation entry point and the manifest test fixture used elsewhere in this file. If a `minimal_valid_manifest` helper does not exist, build the manifest inline following an existing test in this module.

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p defra-agent-cli -- empty_subagent_target_is_rejected --nocapture`
Expected: FAIL — no `subagent_targets` validation exists yet.

- [ ] **Step 3: Add structural validation** in the `for selection in &manifest.tool_selections` loop (~75-138), reusing the existing helper:

```rust
validate_non_empty_values(
    &selection.selection_id,
    "subagent_targets",
    selection.subagent_targets.as_deref().unwrap_or(&[]),
    errors,
);
if selection.subagent_spawn_enabled == Some(true)
    && selection.subagent_targets.as_deref().unwrap_or(&[]).is_empty()
{
    errors.push(format!(
        "tool selection {} sets subagent_spawn_enabled but has no subagent_targets; the tools would be inert",
        selection.selection_id
    ));
}
```

- [ ] **Step 4: Run it and confirm it passes**

Run: `cargo test -p defra-agent-cli -- empty_subagent_target_is_rejected --nocapture`
Expected: PASS.

- [ ] **Step 5: Write the failing live-resolution test** — an unknown target behavior is rejected against the live DB.

```rust
#[tokio::test]
async fn unknown_subagent_target_is_rejected_against_live_db() {
    let access = config_access_for_test("toolsel-target-resolve").await;
    // Seed one known behavior:
    seed_agent_behavior(&access, "did:key:test", "amy-research").await; // helper; create if absent following existing seeding
    let mut manifest = minimal_valid_manifest();
    manifest.tool_selections[0].subagent_spawn_enabled = Some(true);
    manifest.tool_selections[0].subagent_targets = Some(vec!["does-not-exist".to_string()]);

    let errors = validate_manifest_against_live(&access, &manifest).await;
    assert!(errors.iter().any(|e| e.contains("does-not-exist")));
}
```

- [ ] **Step 6: Run it and confirm it fails**

Run: `cargo test -p defra-agent-cli -- unknown_subagent_target_is_rejected_against_live_db --nocapture`
Expected: FAIL.

- [ ] **Step 7: Add live resolution** in `validate_manifest_against_live` (~444-562), following the existing live-probe pattern (EventTrigger filter probing at 459-476). For each tool selection with `subagent_targets`, query `AgentBehavior` for each target and push an error if absent:

```rust
for selection in &manifest.tool_selections {
    for target in selection.subagent_targets.as_deref().unwrap_or(&[]) {
        let q = format!(
            r#"{{ AgentBehavior(filter: {{ behavior_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID }} }}"#,
            escape_graphql_string(target)
        );
        let resp = access.execute(&q).await;
        let exists = resp.data.as_ref()
            .and_then(|d| d.get("AgentBehavior"))
            .and_then(|v| v.as_array())
            .is_some_and(|rows| !rows.is_empty());
        if !exists {
            errors.push(format!(
                "tool selection {} lists subagent_target '{}' which resolves to no AgentBehavior",
                selection.selection_id, target
            ));
        }
    }
}
```

> Note: a target may legitimately live on a remote deployment and only be present locally once `AgentBehavior` has replicated. Document this in the error text path and treat resolution as best-effort against locally-visible behaviors (it is operator guidance, not the security boundary — that is the static allowlist + future ACP).

- [ ] **Step 8: Run it and confirm it passes**

Run: `cargo test -p defra-agent-cli -- unknown_subagent_target_is_rejected_against_live_db --nocapture`
Expected: PASS.

- [ ] **Step 9: Regression + commit**

```bash
cargo test -p defra-agent-cli && cargo clippy --all-targets && cargo fmt --all
git add crates/defra-agent-cli/src/desired_state/validate.rs
git commit -m "feat(cli): validate subagent_targets at apply time (#377)"
```

---

## Task 3: Tier-1 local enable → spawn → list E2E (C2)

**Goal:** Prove, on a single node, that an enabled agent's running-subagent state is complete and queryable end-to-end. This both validates C2 and gives us the regression anchor the convergence (Task 6) will lean on. Reuse the proven harness in `tests/subagent_source_conformance.rs` (driving the spawn via `ToolCallLifecycle` directly, exactly as the existing tests do — do NOT script the model).

**Files:**
- Create: `crates/defra-agent/tests/subagent_enablement_e2e.rs`

- [ ] **Step 1: Write the test** — spawn a local child, then assert `list_subagents` reflects it as background-running, and the child materializes with correct lineage. Model it on `subagent_source_materializes_child_request_from_tool_call` (verbatim pattern available in `subagent_source_conformance.rs`) plus a call into the list-subagents handler.

```rust
mod support;

use std::sync::Arc;
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle};
// background_tools::handle_list_subagents + args — confirm the exact pub(crate)/pub path and
// re-export if needed; the handler signature is:
//   handle_list_subagents(node, caller_request_id, local_deployment_id, ListSubagentsArgs)

use support::interrupt::create_runtime_request;
use support::{test_db};

#[tokio::test]
async fn enabled_agent_spawns_local_child_and_list_reflects_it() {
    let db = test_db("t1-enable-spawn-list").await;
    // Boot an agent with spawn enabled and itself as an allowed target (same pattern as
    // boot_agent_with_targets in subagent_source_conformance.rs):
    let running = support_boot_agent_with_targets(&db, "t1-enable-spawn-list").await;

    let parent_request_id = "t1-parent";
    let parent_tool_call_id = "t1-tc";
    let child_request_id = "t1-child";
    create_runtime_request(
        db.node.as_ref(), &running.agent_did, &running.behavior_id,
        parent_request_id, "t1-session", "parent prompt",
    ).await;

    let args = serde_json::json!({
        "behavior_id": running.behavior_id,
        "prompt": "child work",
        "await_mode": "background"
    }).to_string();

    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        "t1-session".to_string(),
        parent_tool_call_id.to_string(),
        1,
        "spawn_subagent".to_string(),
        args,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Background,
        CancelPolicy::Cascade,
        child_request_id.to_string(),
    );
    lifecycle.start_running().await.unwrap();

    // Child should materialize (SubagentSource or local hook path):
    let child = support_wait_for_child_request(db.node.as_ref(), child_request_id).await;
    assert_eq!(child.behavior_id, running.behavior_id);

    // list_subagents must reflect the running child:
    let resp = defra_agent::background_tools::handle_list_subagents(
        db.node.as_ref(),
        parent_request_id,
        &running.agent_did,
        Default::default(),
    ).await.unwrap();
    assert!(resp.entries.iter().any(|e| e.child_request_id == child_request_id));
}
```

> Implementation reality: `boot_agent_with_targets`, `wait_for_child_request`, and the `RunningAgent`/`booted.agent_did` accessors live in `subagent_source_conformance.rs` and `support/`. Either (a) move the reusable helpers into `tests/support/` so both test files share them, or (b) copy the minimal helper bodies into this file. Prefer (a). `handle_list_subagents` is currently `pub(crate)` in `background_tools.rs` — add a thin `pub` re-export (e.g. `pub use background_tools::handle_list_subagents` behind a test-friendly path) or a `#[cfg(any(test, feature = "test-support"))]` export. Do not change its behavior.

- [ ] **Step 2: Run it and confirm it fails** (compile error / missing export first, then assertion)

Run: `cargo test -p defra-agent -- enabled_agent_spawns_local_child_and_list_reflects_it --nocapture`
Expected: FAIL — either the `handle_list_subagents` export is missing or the assertion does not yet hold.

- [ ] **Step 3: Make the minimal exports / helper moves** described in the note above. No behavior changes.

- [ ] **Step 4: Run it and confirm it passes**

Run: `cargo test -p defra-agent -- enabled_agent_spawns_local_child_and_list_reflects_it --nocapture`
Expected: PASS. If it fails on the assertion (not compile), that is a real C2 gap — record exactly what state is missing (this is the spec's open question) and open a follow-up before forcing the assertion.

- [ ] **Step 5: Regression + commit**

```bash
cargo test -p defra-agent && cargo clippy --all-targets && cargo fmt --all
git add crates/defra-agent/tests/subagent_enablement_e2e.rs crates/defra-agent/tests/support crates/defra-agent/src/background_tools.rs
git commit -m "test(runtime): tier-1 local enable->spawn->list e2e (#377)"
```

---

## Task 4: Make `AgentBehavior` self-describing (C4 — schema + parsing)

**Goal:** Add `description` and `summary` to `AgentBehavior` so an orchestrator's statically-allowed targets carry "what this does." Adding fields to an existing collection touches **no `Collection` enum variant and no Lean parity**.

**Files:**
- Modify: `crates/defra-agent-protocol/schemas/agent/agent_behavior.graphql`
- Modify: `crates/defra-agent/src/document_config/behavior.rs` (struct + query)

- [ ] **Step 1: Add the schema fields.** In `agent_behavior.graphql`, after `display_name`:

```graphql
    display_name: String
    description: String
    summary: String
```

- [ ] **Step 2: Write the failing parse test** — load a behavior with `description`/`summary` set and assert they round-trip. Use an existing behavior-doc test in `document_config/behavior.rs` (or its `tests.rs` submodule) as the template.

```rust
#[tokio::test]
async fn behavior_document_parses_description_and_summary() {
    let node = test_node("behavior-desc").await; // existing test node helper for this module
    upsert_agent_behavior_raw(&node, r#"{
        behavior_id: "amy-research", agent_did: "did:key:test", enabled: true,
        description: "Researches topics deeply", summary: "Deep research"
    }"#).await;
    let rec = load_agent_behavior_record(&node, "amy-research").await.unwrap().unwrap();
    assert_eq!(rec.description.as_deref(), Some("Researches topics deeply"));
    assert_eq!(rec.summary.as_deref(), Some("Deep research"));
}
```

> Match the real helper names used in this module's tests for creating a node and upserting a behavior document.

- [ ] **Step 3: Run it and confirm it fails**

Run: `cargo test -p defra-agent -- behavior_document_parses_description_and_summary --nocapture`
Expected: FAIL — fields not on the struct/query.

- [ ] **Step 4: Add the fields to the document struct and query.** In `document_config/behavior.rs`, add to the `AgentBehavior` document struct (after `display_name`):

```rust
    pub description: Option<String>,
    pub summary: Option<String>,
```

And add `description` and `summary` to the GraphQL selection set in `load_agent_behavior_record` (the query at ~lines 44-62), after `display_name`.

- [ ] **Step 5: Run it and confirm it passes**

Run: `cargo test -p defra-agent -- behavior_document_parses_description_and_summary --nocapture`
Expected: PASS.

- [ ] **Step 6: Regression + commit**

```bash
cargo test -p defra-agent && cargo clippy --all-targets && cargo fmt --all
git add crates/defra-agent-protocol/schemas/agent/agent_behavior.graphql crates/defra-agent/src/document_config/behavior.rs
git commit -m "feat(schema): add description/summary to AgentBehavior (#377)"
```

---

## Task 5: Inject allowed targets + descriptions into the preamble (C4 — surfacing)

**Goal:** Tell the orchestrator, in its frozen preamble, which targets it may spawn and what each does — using the static `subagent_targets` (no dynamic discovery). The preamble is assembled by `build_preamble` (`prompt.rs:192-215`) via `LayeredPromptBuilder::for_behavior` / `::new`.

**Files:**
- Modify: `crates/defra-agent/src/prompt.rs` (`build_preamble`, `for_behavior`, `new`)
- Modify: the async agent-init site that calls `LayeredPromptBuilder::new` (resolve descriptions there)

- [ ] **Step 1: Write the failing unit test for `build_preamble`** — when given allowed targets, the preamble contains a guidance block listing them.

```rust
#[test]
fn preamble_lists_allowed_subagent_targets() {
    let targets = vec![
        ("amy-research".to_string(), "Deep research".to_string()),
        ("amy-code".to_string(), "Writes code".to_string()),
    ];
    let preamble = build_preamble_with_targets(
        "You are helpful.", "amy", &["bash"], false, &targets,
    );
    assert!(preamble.contains("amy-research"));
    assert!(preamble.contains("Deep research"));
    assert!(preamble.contains("amy-code"));
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p defra-agent -- preamble_lists_allowed_subagent_targets --nocapture`
Expected: FAIL — `build_preamble_with_targets` does not exist.

- [ ] **Step 3: Extend the preamble builder.** Change `build_preamble` to accept allowed targets (keep a thin wrapper for existing callers), and append a guidance block before `parts.join("\n\n")`:

```rust
fn build_preamble_with_targets(
    system_prompt: &str,
    behavior_name: &str,
    tool_names: &[&str],
    include_meta_tool_guidance: bool,
    allowed_targets: &[(String, String)], // (behavior_id, description-or-summary)
) -> String {
    let mut parts = Vec::new();
    let system_prompt = strip_title_generation_suffix(system_prompt);
    if !system_prompt.is_empty() { parts.push(system_prompt.to_string()); }
    if !behavior_name.is_empty() { parts.push(format!("You are the {} agent.", behavior_name)); }
    if include_meta_tool_guidance { parts.push(TOOL_DISCOVERY_GUIDANCE.to_string()); }
    parts.push(direct_tool_guidance(tool_names));
    if !allowed_targets.is_empty() {
        let mut block = String::from("You may spawn these subagents (spawn_subagent behavior_id):");
        for (id, desc) in allowed_targets {
            block.push_str(&format!("\n- {id}: {desc}"));
        }
        parts.push(block);
    }
    parts.join("\n\n")
}

// Preserve the old signature as a wrapper:
fn build_preamble(system_prompt: &str, behavior_name: &str, tool_names: &[&str], include_meta_tool_guidance: bool) -> String {
    build_preamble_with_targets(system_prompt, behavior_name, tool_names, include_meta_tool_guidance, &[])
}
```

Thread an `allowed_targets: &[(String, String)]` parameter through `LayeredPromptBuilder::for_behavior` (overload or add the param) and `::new`. `::new` currently takes `(&AgentBehavior, &ToolSurface)`; the allowed target **ids** are already in the `ToolSurface`'s `SubagentToolConfig.targets`, but **descriptions require a DB lookup**, so descriptions must be resolved at the async init site (Step 4) and passed in. For `::new` (sync), accept an extra `allowed_targets: &[(String, String)]` arg.

- [ ] **Step 4: Resolve descriptions at the async init site.** Where the agent is built (the async path that constructs `LayeredPromptBuilder::new` — locate via `LayeredPromptBuilder::new(` usage), resolve each `subagent_targets` id to its description using the existing `load_agent_behavior(node, behavior_id)` (already used by `subagent_target_host`), falling back to `summary`, then empty string:

```rust
let mut allowed_targets = Vec::new();
for id in tool_surface.subagent_targets() { // add a getter on ToolSurface exposing SubagentToolConfig.targets
    let desc = match load_agent_behavior(node, id).await {
        Ok(Some(b)) => b.description.clone().or(b.summary.clone()).unwrap_or_default(),
        _ => String::new(),
    };
    allowed_targets.push((id.clone(), desc));
}
let prompt_builder = LayeredPromptBuilder::new(&behavior, &tool_surface, &allowed_targets);
```

> Add a `subagent_targets(&self) -> &[String]` getter on `ToolSurface` exposing the `SubagentToolConfig.targets`. `load_agent_behavior` returns the document type extended in Task 4, so `description`/`summary` are available.

- [ ] **Step 5: Run the unit test and confirm it passes**

Run: `cargo test -p defra-agent -- preamble_lists_allowed_subagent_targets --nocapture`
Expected: PASS.

- [ ] **Step 6: Regression + commit**

```bash
cargo test -p defra-agent && cargo clippy --all-targets && cargo fmt --all
git add crates/defra-agent/src/prompt.rs crates/defra-agent/src/tool_surface crates/defra-agent/src/agent.rs
git commit -m "feat(runtime): surface allowed subagent targets in preamble (#377)"
```

---

## Task 6: Converge the local/remote spawn write-path (C3)

**Goal:** Make a spawn one uniform "write the bridge; let `SubagentSource` create the child" path so locality is transparent. Today: `persist_spawn_subagent_tool_call` (`message_spawn.rs:217-543`) early-returns a receipt for `Remote` and **synchronously** creates the child for `Local` (`create_subagent_request_with_request_id`, 445-512); foreground blocking is a poll loop on the child edge (`subagent_bridge.rs:106-297`) that is already locality-agnostic; `SubagentSource` already dedups via `child_request_exists` (`subagent_source.rs:227-250, 396-399`). So the local synchronous create is redundant with `SubagentSource`.

**Approach:** characterization-first, then remove the local fast-path so both localities flow through `SubagentSource`, with foreground served by the existing poll loop.

**Files:**
- Modify: `crates/defra-agent/src/hook/persistence/message_spawn.rs` (remove the Local synchronous-create branch; always store the lifecycle + return receipt / enter foreground poll)
- Possibly modify: `crates/defra-agent/src/hook/persistence/subagent_bridge.rs` (ensure the foreground poll loop is entered for the unified path)
- Test: `crates/defra-agent/tests/subagent_convergence.rs` (new) + existing `subagent_source_conformance.rs` as the gate

- [ ] **Step 1: Characterization tests (capture current behavior before changing anything).** In a new `subagent_convergence.rs`, add tests asserting today's observable contract for both localities, so the refactor is provably behavior-preserving:
  - local background spawn → child materializes, `list_subagents` shows it, terminal projects to parent bridge;
  - local foreground spawn → call returns the child's completed result;
  - (cross-deployment is covered by the existing `r5_conformance` two-node harness — add a characterization assertion there if missing).

Use the `ToolCallLifecycle::new_subagent` + `create_runtime_request` patterns from `subagent_source_conformance.rs`. Run them green against the **unmodified** code first.

Run: `cargo test -p defra-agent -- subagent_convergence --nocapture`
Expected: PASS (against current code) — this is the safety net.

- [ ] **Step 2: Decide foreground-remote policy (explicit).** Today foreground+Remote is rejected (`message_spawn.rs:336-352`). The poll loop is locality-agnostic, so the unified path *could* support it. **For this task, preserve the existing rejection** (keep the guard) to bound scope; record a follow-up to enable foreground-remote once Tier-2 (Task 7) confirms terminal replication latency is acceptable. Add a test asserting the guard still returns the `ArgumentInvalid` "use await_mode=background" payload.

- [ ] **Step 3: Remove the local synchronous-create fast-path.** In `persist_spawn_subagent_tool_call`, delete the `Local` branch that calls `create_subagent_request_with_request_id` (445-512) and the subsequent local foreground/background split (514-542). Replace the whole tail (after the await-mode guards and lifecycle setup) with the **same** behavior the Remote branch uses, plus foreground poll entry:

```rust
// Unified: persist the bridge (start_running already wrote the AgentToolCall row),
// store the lifecycle, and let SubagentSource create the child (local or remote).
self.in_flight_lifecycles.lock().await.insert(internal_call_id.to_string(), lifecycle);

match await_mode {
    AwaitMode::Background => Ok(ToolCallHookAction::skip(
        background_receipt_payload(&child_request_id, None, behavior_id),
    )),
    AwaitMode::Foreground => {
        // Locality-agnostic poll loop on the child edge (existing mechanism):
        self.await_foreground_subagent(/* same args as today's local foreground call */).await
    }
}
```

> The exact argument list for `await_foreground_subagent` is whatever today's local foreground call site (526-542) passes — reuse it verbatim. Ensure `start_running()` (which writes the `AgentToolCall` bridge with `child_request_id`) is still called before this tail in all cases, so `SubagentSource` has a row to observe. `SubagentSource`'s `child_request_exists` dedup means there is no double-create even though the local node also runs a `SubagentSource`.

- [ ] **Step 4: Run the characterization tests + full conformance gate**

Run: `cargo test -p defra-agent -- subagent_convergence --nocapture`
Then: `cargo test -p defra-agent -- subagent_source_conformance --nocapture` and the `r4_subagent_tools` suite.
Expected: PASS — same observable behavior, now via one path. If foreground-local now races (child not yet created when the poll starts), confirm the poll loop tolerates "edge not yet present" (it polls with backoff per `subagent_bridge.rs:295`); if not, that is the one real fix this task needs.

- [ ] **Step 5: Confirm `subagent_target_host` is no longer needed for branching** (it may still be used to set the timeout/DID expectations). Remove dead code only if the compiler/clippy flags it; otherwise leave it.

- [ ] **Step 6: Regression + commit**

```bash
cargo test -p defra-agent && cargo clippy --all-targets && cargo fmt --all
git add crates/defra-agent/src/hook/persistence crates/defra-agent/tests/subagent_convergence.rs
git commit -m "refactor(runtime): unify local/remote subagent spawn write-path (#377)"
```

---

## Task 7 (STUB): Simulated-fleet validation (spec slice 5, Tier 2)

Not yet expanded into bite-sized steps — depends on findings from Tasks 3 and 6 and on the multi-node harness. Scope when starting:

- Use/extend `tests/support/r5_conformance/runner.rs` `start_two_nodes()` (and `pairing_conformance` for replication setup) to run the **unified** path across two in-process `EmbeddedNode`s with replication of `AgentRequest`, `AgentToolCall`, `AgentBehavior`, `PeerPairingDesired`.
- Cases: cross-deployment background spawn → child runs on node B → terminal replicates back → parent `wait_subagent` resolves; behavioral parity with the local case; lineage + terminal projection; a small fan-out (one orchestrator → N targets).
- Implement the **replication health check** (C6): assert required collections are live and converging between an orchestrator and its targets before running scenarios.
- Decision point from Task 6 Step 2: evaluate enabling foreground-remote here.

## Task 8 (STUB): Real-fleet validation (spec slice 6, Tier 3 — last)

Not yet expanded — requires deploying binaries to the 14-node fleet and replication transport enabled across nodes (depends on #363 / defradb.rs#1012/#1013). Scope when starting:

- Author `PeerPairingDesired` docs pairing the orchestrator (`amy`) with target steward DIDs; enable replication of the required collections across the fleet.
- Enable subagents on `amy` via `config apply` (Tasks 1–2) with steward behaviors as `subagent_targets`; give stewards `description`/`summary` (Task 4).
- Run complex scenarios on real hardware/network: `amy` → a steward (foreground-local / background-remote), then fan-out `amy` → 3 stewards; verify lineage, completion, and the read-only views (Codex shim / desktop) projecting the same state.

---

## Self-review notes

- **Spec coverage:** C1→Tasks 1–2; C2→Task 3; C3→Task 6; C4→Tasks 4–5; C5→already-present static allowlist (validated by Tasks 1–2, no new code); C6→Tasks 7–8 (stubs) + the health check called out there. Formal-methods posture: no Lean changes — confirmed (no new collection; Task 6 is behavior-preserving, gated by conformance tests).
- **Sequencing risk:** Task 6 is the only intricate one and is deliberately characterization-first with the conformance suite as the gate. Tasks 1, 2, 4 are independent and low-risk; Task 5 depends on Task 4; Task 3 should land before Task 6 (it is the local regression anchor).
- **Naming to confirm against the tree (the executor must verify, not assume):** `config_access_for_test`, `minimal_valid_manifest`, the structural-validation entry point name, `wait_for_child_request`/`boot_agent_with_targets` helper locations, the `LayeredPromptBuilder::new` call site, and the `await_foreground_subagent` argument list. These are cited with their real files; match the real symbols.
