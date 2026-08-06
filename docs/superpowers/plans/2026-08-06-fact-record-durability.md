# Fact Record Durability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the render-contributing configuration versioned and pin it per completion, so any provider input can be reconstructed byte-exactly from persisted facts instead of being stored.

**Architecture:** Three config collections become `@branchable`, giving them commit history. At render time we stamp the head commit CID of each contributing config document into a constant-size `RenderedRequest` envelope alongside the existing `prompt_hash`/`tools_hash`. Reconstruction reads those documents back at their pinned CIDs, replays the same sanitizer the owned loop uses, and verifies the result against the stored hash.

**Tech Stack:** Rust, DefraDB via `defra-node` (pinned `f9e21c6`), GraphQL string queries built inline, SHA-256 over canonical JSON.

## Global Constraints

- Always wrap interpolated GraphQL string values in `crate::graphql::escape_graphql_string()`.
- Never emit `[]` in a DefraDB mutation — an empty list literal types as `JsonArray` and corrupts nillable array columns. Emit `null`.
- Gate with `cargo test -p gents` (full package), not `--lib`.
- Compile the workspace before pushing: `cargo check --workspace --all-targets`.
- Use `tracing`, never `println`.
- Schema migration mechanics are explicitly out of scope for this plan.

## Reference Facts (verified 2026-08-06)

- `_commits(docID: "…") { cid height }` returns a document's commit history. The composite commit at `height` 1 is the document-level commit.
- CID time-travel reads are ACP-filtered by DefraDB — an unauthorized identity gets an empty result even with an exact CID. Regression-guarded upstream in `tools/integration-test/tests/acp/audit.rs`.
- Schemas are registered as `include_str!` consts in `crates/gents-schemas/src/lib.rs`.
- The sanitizer entry point is `crate::compaction::sanitize_history_for_provider(Vec<Message>) -> Vec<Message>` (`crates/gents/src/compaction.rs:231`).
- The capture seam is invoked at `crates/gents/src/agent/daemon/inference.rs:198-223`; the factory defaults to `None` at `crates/gents/src/agent.rs:200`.
- `build_rendered_completion_request` and the canonical-JSON hashers live in `crates/gents/src/rendered_request.rs:96-159`.

## File Structure

| File | Responsibility |
| --- | --- |
| `crates/gents-schemas/schemas/agent/agent_behavior.graphql` | add `@branchable` |
| `crates/gents-schemas/schemas/agent/tool_selection.graphql` | add `@branchable` |
| `crates/gents-schemas/schemas/agent/skill.graphql` | add `@branchable` |
| `crates/gents-schemas/schemas/agent/rendered_request.graphql` | new envelope collection |
| `crates/gents-schemas/src/lib.rs` | register the new schema const |
| `crates/gents/src/rendered_request.rs` | DTO gains `config_cids` |
| `crates/gents/src/rendered_request/cids.rs` | resolve head CIDs for config docs |
| `crates/gents/src/rendered_request/sink.rs` | persist the envelope |
| `crates/gents/src/rendered_request/reconstruct.rs` | replay + verify |
| `crates/gents/src/agent.rs` | default the capture factory on |
| `crates/gents/src/run_timeline_fetch.rs` | add token + lineage fields |

---

### Task 1: Make render-contributing config collections branchable

**Files:**
- Modify: `crates/gents-schemas/schemas/agent/agent_behavior.graphql:1`
- Modify: `crates/gents-schemas/schemas/agent/tool_selection.graphql:1`
- Modify: `crates/gents-schemas/schemas/agent/skill.graphql:1`
- Test: `crates/gents-schemas/src/lib.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `AgentBehavior`, `ToolSelection`, and `Skill` documents now accumulate commit history, making `_commits` queries against them non-empty. Task 3 depends on this.

- [ ] **Step 1: Write the failing test**

Add to `crates/gents-schemas/src/lib.rs`:

```rust
#[cfg(test)]
mod branchable_tests {
    #[test]
    fn render_contributing_configs_are_branchable() {
        for (name, schema) in [
            ("AgentBehavior", super::AGENT_BEHAVIOR),
            ("ToolSelection", super::TOOL_SELECTION),
            ("Skill", super::SKILL),
        ] {
            assert!(
                schema.contains("@branchable"),
                "{name} must be @branchable so its versions can be pinned at render time"
            );
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p gents-schemas render_contributing_configs_are_branchable`
Expected: FAIL — `AgentBehavior must be @branchable`.

(If `TOOL_SELECTION` or `SKILL` are named differently in `lib.rs`, use the actual const names; the file is a flat list of `pub const NAME: &str = include_str!(...)`.)

- [ ] **Step 3: Add the directive to each schema**

In `agent_behavior.graphql`, change line 1 from `type AgentBehavior {` to:

```graphql
type AgentBehavior @branchable {
```

Apply the same edit to `tool_selection.graphql` and `skill.graphql`, preserving each type's existing name and body.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p gents-schemas render_contributing_configs_are_branchable`
Expected: PASS

- [ ] **Step 5: Verify nothing else broke**

Run: `cargo test -p gents`
Expected: PASS. Startup schema registration exercises these collections; a failure here means an existing test asserts non-branchable behavior and must be updated rather than worked around.

- [ ] **Step 6: Commit**

```bash
git add crates/gents-schemas/schemas/agent/agent_behavior.graphql \
        crates/gents-schemas/schemas/agent/tool_selection.graphql \
        crates/gents-schemas/schemas/agent/skill.graphql \
        crates/gents-schemas/src/lib.rs
git commit -m "feat(schemas): make render-contributing config collections branchable"
```

---

### Task 2: Add the RenderedRequest envelope collection

**Files:**
- Create: `crates/gents-schemas/schemas/agent/rendered_request.graphql`
- Modify: `crates/gents-schemas/src/lib.rs`
- Test: `crates/gents-schemas/src/lib.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub const RENDERED_REQUEST: &str`, and a `RenderedRequest` collection with fields consumed by Task 4's sink and Task 5's reconstructor.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn rendered_request_schema_is_registered_and_branchable() {
    assert!(super::RENDERED_REQUEST.contains("type RenderedRequest @branchable"));
    for field in [
        "request_id", "session_id", "agent_did", "behavior_id",
        "turn_index", "attempt", "model_name", "source",
        "prompt_hash", "tools_hash", "sampling_json", "tool_choice_json",
        "config_cids", "created_at",
    ] {
        assert!(
            super::RENDERED_REQUEST.contains(field),
            "RenderedRequest must persist {field}"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p gents-schemas rendered_request_schema_is_registered_and_branchable`
Expected: FAIL — `RENDERED_REQUEST` not found.

- [ ] **Step 3: Create the schema**

`crates/gents-schemas/schemas/agent/rendered_request.graphql`:

```graphql
type RenderedRequest @branchable {
    request_id: String @index
    session_id: String @index
    agent_did: String @index @immutable
    behavior_id: String @index
    turn_index: Int
    attempt: Int
    model_name: String
    source: String
    prompt_hash: String @index
    tools_hash: String @index
    sampling_json: String
    tool_choice_json: String
    config_cids: String
    created_at: String @index
}
```

`config_cids` is a JSON object serialized to a string — `{"behavior": "<cid>", "tool_selection": "<cid>", "skills": {"<skill_id>": "<cid>"}}`. It is a string, not a list, so there is no empty-list hazard; absence is `null`.

- [ ] **Step 4: Register the const**

Add to `crates/gents-schemas/src/lib.rs`, following the existing `pub const NAME: &str = include_str!(...)` pattern:

```rust
pub const RENDERED_REQUEST: &str = include_str!("../schemas/agent/rendered_request.graphql");
```

Then add `RENDERED_REQUEST` to whatever aggregate list the crate exposes for startup registration, alongside `AGENT_MESSAGE` and its peers.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p gents-schemas rendered_request_schema_is_registered_and_branchable`
Expected: PASS

- [ ] **Step 6: Verify the collection registers against a live node**

Run: `cargo test -p gents`
Expected: PASS. Startup registration will now create the collection; a schema syntax error surfaces here.

- [ ] **Step 7: Commit**

```bash
git add crates/gents-schemas/schemas/agent/rendered_request.graphql crates/gents-schemas/src/lib.rs
git commit -m "feat(schemas): add RenderedRequest envelope collection"
```

---

### Task 3: Resolve and stamp config CIDs at render time

**Files:**
- Create: `crates/gents/src/rendered_request/cids.rs`
- Modify: `crates/gents/src/rendered_request.rs:78-126`
- Test: `crates/gents/src/rendered_request/cids.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: branchable config collections from Task 1.
- Produces:
  - `pub(crate) async fn head_cid(node: &EmbeddedNode, collection: &str, doc_id: &str) -> Result<Option<String>>`
  - `pub struct ConfigCids { pub behavior: Option<String>, pub tool_selection: Option<String>, pub skills: BTreeMap<String, String> }` with `ConfigCids::to_json_string(&self) -> String`
  - `RenderedCompletionRequest.config_cids: ConfigCids`, and `build_rendered_completion_request` gains a `config_cids: ConfigCids` parameter appended after `sampling_json`.

- [ ] **Step 1: Write the failing test for CID serialization**

In `crates/gents/src/rendered_request/cids.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_cids_serialize_stably_and_omit_absent_entries() {
        let mut skills = std::collections::BTreeMap::new();
        skills.insert("skill-b".to_string(), "cid-b".to_string());
        skills.insert("skill-a".to_string(), "cid-a".to_string());

        let cids = ConfigCids {
            behavior: Some("cid-behavior".to_string()),
            tool_selection: None,
            skills,
        };

        let json = cids.to_json_string();
        assert!(json.contains("\"behavior\":\"cid-behavior\""));
        assert!(!json.contains("tool_selection"));
        // BTreeMap ordering makes the string stable across runs.
        assert!(json.find("skill-a").unwrap() < json.find("skill-b").unwrap());
    }

    #[test]
    fn empty_config_cids_serialize_to_an_empty_object_not_a_list() {
        let cids = ConfigCids::default();
        assert_eq!(cids.to_json_string(), "{}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p gents config_cids_serialize_stably`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement `ConfigCids` and `head_cid`**

```rust
use std::collections::BTreeMap;

use anyhow::Result;
use defra_node::EmbeddedNode;
use serde_json::{json, Map, Value};

use crate::graphql::escape_graphql_string;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigCids {
    pub behavior: Option<String>,
    pub tool_selection: Option<String>,
    pub skills: BTreeMap<String, String>,
}

impl ConfigCids {
    /// Serializes to a stable JSON object. Absent entries are omitted rather
    /// than emitted as null, and an all-absent value is `{}` — never `[]`.
    pub fn to_json_string(&self) -> String {
        let mut map = Map::new();
        if let Some(cid) = &self.behavior {
            map.insert("behavior".to_string(), Value::String(cid.clone()));
        }
        if let Some(cid) = &self.tool_selection {
            map.insert("tool_selection".to_string(), Value::String(cid.clone()));
        }
        if !self.skills.is_empty() {
            let skills: Map<String, Value> = self
                .skills
                .iter()
                .map(|(id, cid)| (id.clone(), Value::String(cid.clone())))
                .collect();
            map.insert("skills".to_string(), Value::Object(skills));
        }
        Value::Object(map).to_string()
    }
}

/// Returns the head commit CID for a document, or `None` when the document has
/// no commits — which is the pre-branchable epoch for that collection.
pub(crate) async fn head_cid(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<String>> {
    let doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"query {{ _commits(docID: "{doc_id}") {{ cid height }} }}"#
    );
    let resp = node.execute(&query).await;
    let commits = resp
        .data
        .as_ref()
        .and_then(|data| data.get("_commits"))
        .and_then(Value::as_array);

    let Some(commits) = commits else {
        return Ok(None);
    };

    // Highest height is the current head. Ties cannot occur for a single
    // document's composite chain.
    let head = commits
        .iter()
        .filter_map(|commit| {
            let cid = commit.get("cid")?.as_str()?.to_string();
            let height = commit.get("height")?.as_i64()?;
            Some((height, cid))
        })
        .max_by_key(|(height, _)| *height)
        .map(|(_, cid)| cid);

    Ok(head)
}
```

Drop the `json` import from the `use serde_json::{json, Map, Value};` line if you do not end up using the macro.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p gents config_cids_serialize`
Expected: PASS (both tests)

- [ ] **Step 5: Thread `config_cids` into the DTO**

In `crates/gents/src/rendered_request.rs`, add to `RenderedCompletionRequest` (after `tools_hash`):

```rust
    pub config_cids: crate::rendered_request::cids::ConfigCids,
```

Add `pub mod cids;` at the top of `rendered_request.rs`. Add a `config_cids: ConfigCids` parameter to `build_rendered_completion_request`, appended after `sampling_json`, and set the field in the constructed struct.

Update the existing test `rendered_completion_request_hashes_prompt_and_tools`
(`rendered_request.rs:210-250`) to pass `ConfigCids::default()` as the new argument.

- [ ] **Step 6: Run the rendered_request tests**

Run: `cargo test -p gents rendered_request`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/gents/src/rendered_request.rs crates/gents/src/rendered_request/cids.rs
git commit -m "feat(rendered-request): stamp config commit CIDs into the envelope"
```

---

### Task 4: Persist the envelope, capture on by default

**Files:**
- Create: `crates/gents/src/rendered_request/sink.rs`
- Modify: `crates/gents/src/agent.rs:200`
- Modify: `crates/gents/src/agent/daemon/inference.rs:198-223`
- Test: `crates/gents/tests/e2e_runtime/rendered_request_capture.rs`

**Interfaces:**
- Consumes: `ConfigCids` and the extended DTO from Task 3; the `RenderedRequest` collection from Task 2.
- Produces: `pub(crate) fn defra_rendered_request_sink(node: Arc<EmbeddedNode>) -> RenderedRequestCaptureFactory`, installed as the default so every completion writes one `RenderedRequest` row per (request, turn, attempt).

- [ ] **Step 1: Write the failing integration test**

`crates/gents/tests/e2e_runtime/rendered_request_capture.rs`:

```rust
/// A completion must persist exactly one RenderedRequest envelope carrying the
/// prompt hash, with no explicit capture wiring by the caller.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_persists_a_rendered_request_envelope_by_default() {
    let harness = crate::support::runtime_harness().await;

    harness.submit_prompt("say READY and stop").await;
    harness.wait_for_terminal_response().await;

    let rows = harness
        .query(r#"query { RenderedRequest { request_id turn_index attempt prompt_hash tools_hash config_cids } }"#)
        .await;
    let rows = rows["RenderedRequest"].as_array().expect("RenderedRequest rows");

    assert_eq!(rows.len(), 1, "one envelope per completion turn");
    assert_eq!(rows[0]["turn_index"].as_i64(), Some(0));
    assert_eq!(rows[0]["attempt"].as_i64(), Some(0));
    assert_eq!(
        rows[0]["prompt_hash"].as_str().map(str::len),
        Some(64),
        "prompt_hash is a hex sha256"
    );
    assert!(
        rows[0]["config_cids"].as_str().unwrap().starts_with('{'),
        "config_cids is a JSON object, never a list"
    );
}
```

Use the existing runtime-harness helper in `crates/gents/tests/support/`; match the constructor name and prompt/wait helpers already used by neighbouring `e2e_runtime` tests rather than inventing new ones.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p gents completion_persists_a_rendered_request_envelope_by_default`
Expected: FAIL — zero `RenderedRequest` rows, because the factory still defaults to `None`.

- [ ] **Step 3: Implement the sink**

`crates/gents/src/rendered_request/sink.rs`:

```rust
use std::sync::Arc;

use defra_node::EmbeddedNode;

use crate::graphql::escape_graphql_string;
use crate::rendered_request::{RenderedRequestCaptureFactory, RenderedCompletionRequest};
use crate::session::{execute_mutation_with_retry, requester_did_create_field};

/// Persists one constant-size envelope per rendered completion. The payload is
/// deliberately not stored: it is reconstructible from canonical messages plus
/// the pinned config CIDs, and verified against `prompt_hash`.
pub(crate) fn defra_rendered_request_sink(
    node: Arc<EmbeddedNode>,
) -> RenderedRequestCaptureFactory {
    Arc::new(move |_context| {
        let node = node.clone();
        Arc::new(move |rendered: RenderedCompletionRequest| {
            let node = node.clone();
            Box::pin(async move {
                let mutation = build_mutation(&rendered);
                execute_mutation_with_retry(&node, &mutation, "persist_rendered_request").await?;
                Ok(())
            })
        })
    })
}

fn build_mutation(rendered: &RenderedCompletionRequest) -> String {
    let now = chrono::Utc::now().to_rfc3339();
    let source = serde_json::to_value(rendered.source)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();

    format!(
        r#"mutation {{
            create_RenderedRequest(input: {{
                request_id: "{request_id}",
                session_id: "{session_id}",
                agent_did: "{agent_did}",
                behavior_id: "{behavior_id}",
                turn_index: {turn_index},
                attempt: {attempt},
                model_name: "{model_name}",
                source: "{source}",
                prompt_hash: "{prompt_hash}",
                tools_hash: "{tools_hash}",
                sampling_json: "{sampling_json}",
                tool_choice_json: "{tool_choice_json}",
                config_cids: "{config_cids}",
                created_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(&rendered.request_id),
        session_id = escape_graphql_string(&rendered.session_id),
        agent_did = escape_graphql_string(&rendered.agent_did),
        behavior_id = escape_graphql_string(&rendered.behavior_id),
        turn_index = rendered.turn_index,
        attempt = rendered.attempt,
        model_name = escape_graphql_string(&rendered.model_name),
        source = escape_graphql_string(&source),
        prompt_hash = escape_graphql_string(&rendered.prompt_hash),
        tools_hash = escape_graphql_string(&rendered.tools_hash),
        sampling_json = escape_graphql_string(&rendered.sampling_json.to_string()),
        tool_choice_json = escape_graphql_string(&rendered.tool_choice_json.to_string()),
        config_cids = escape_graphql_string(&rendered.config_cids.to_json_string()),
    )
}
```

`requester_did_create_field` is imported for parity with the spill path; drop the import if the collection does not carry `requester_did`.

- [ ] **Step 4: Default the factory on**

At `crates/gents/src/agent.rs:200`, replace `rendered_request_capture_factory: None,` with the default sink. The node handle is already available in that constructor — use the same handle the other persistence paths take:

```rust
            rendered_request_capture_factory: Some(
                crate::rendered_request::sink::defra_rendered_request_sink(node.clone()),
            ),
```

Add `pub(crate) mod sink;` alongside `pub mod cids;` in `rendered_request.rs`. Leave `builder.rs:112` in place so tests can still override the sink.

- [ ] **Step 5: Populate `config_cids` at the capture site**

In `crates/gents/src/agent/daemon/inference.rs`, inside the `on_rendered_request` closure at lines 211-223, resolve the CIDs before building the rendered request and pass them into `build_rendered_completion_request`:

```rust
let config_cids = crate::rendered_request::cids::ConfigCids {
    behavior: crate::rendered_request::cids::head_cid(&node, &behavior_doc_id).await?,
    tool_selection: match tool_selection_doc_id.as_deref() {
        Some(id) => crate::rendered_request::cids::head_cid(&node, id).await?,
        None => None,
    },
    skills: Default::default(),
};
```

Populate `skills` from the behavior's resolved skill documents in the same shape once the skill doc ids are in scope at this site; if they are not, leave the map empty and record why in a comment — an empty map serializes to an omitted key, not a false claim.

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p gents completion_persists_a_rendered_request_envelope_by_default`
Expected: PASS

- [ ] **Step 7: Run the full package suite**

Run: `cargo test -p gents`
Expected: PASS. Capture is now on for every test that runs a completion, so this catches sink errors that would otherwise only appear in production.

- [ ] **Step 8: Commit**

```bash
git add crates/gents/src/rendered_request/sink.rs crates/gents/src/rendered_request.rs \
        crates/gents/src/agent.rs crates/gents/src/agent/daemon/inference.rs \
        crates/gents/tests/e2e_runtime/rendered_request_capture.rs
git commit -m "feat(rendered-request): persist envelopes by default"
```

---

### Task 5: Reconstruct and verify

**Files:**
- Create: `crates/gents/src/rendered_request/reconstruct.rs`
- Test: `crates/gents/tests/e2e_runtime/rendered_request_reconstruct.rs`

**Interfaces:**
- Consumes: envelopes written by Task 4; branchable configs from Task 1.
- Produces:
  - `pub enum ReconstructOutcome { Verified { messages_json: Value }, HashMismatch { expected: String, actual: String }, Unreconstructible { reason: String } }`
  - `pub async fn reconstruct(node: &EmbeddedNode, request_id: &str, turn_index: usize, attempt: u32) -> Result<ReconstructOutcome>`

- [ ] **Step 1: Write the failing round-trip test**

`crates/gents/tests/e2e_runtime/rendered_request_reconstruct.rs`:

```rust
/// Reconstruction must reproduce the exact bytes that were sent, proven by the
/// stored hash.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn captured_request_reconstructs_to_a_matching_hash() {
    let harness = crate::support::runtime_harness().await;
    harness.submit_prompt("say READY and stop").await;
    harness.wait_for_terminal_response().await;

    let request_id = harness.last_request_id().await;
    let outcome = gents::rendered_request::reconstruct::reconstruct(
        harness.node(), &request_id, 0, 0,
    )
    .await
    .expect("reconstruct");

    assert!(
        matches!(outcome, gents::rendered_request::reconstruct::ReconstructOutcome::Verified { .. }),
        "expected Verified, got {outcome:?}"
    );
}

/// Editing a behavior after the fact must not corrupt history: the pinned CID
/// still yields the configuration that actually produced the bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_edit_does_not_invalidate_earlier_reconstruction() {
    let harness = crate::support::runtime_harness().await;
    harness.submit_prompt("say READY and stop").await;
    harness.wait_for_terminal_response().await;
    let request_id = harness.last_request_id().await;

    harness.update_behavior_system_prompt("a completely different prompt").await;

    let outcome = gents::rendered_request::reconstruct::reconstruct(
        harness.node(), &request_id, 0, 0,
    )
    .await
    .expect("reconstruct");

    assert!(
        matches!(outcome, gents::rendered_request::reconstruct::ReconstructOutcome::Verified { .. }),
        "a later config edit must not change history; got {outcome:?}"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p gents rendered_request_reconstruct`
Expected: FAIL — `reconstruct` module does not exist.

- [ ] **Step 3: Implement reconstruction**

`crates/gents/src/rendered_request/reconstruct.rs`:

```rust
use anyhow::Result;
use defra_node::EmbeddedNode;
use serde_json::Value;

#[derive(Debug)]
pub enum ReconstructOutcome {
    Verified { messages_json: Value },
    HashMismatch { expected: String, actual: String },
    Unreconstructible { reason: String },
}

/// Replays the provider input for one captured completion and verifies it
/// against the stored `prompt_hash`.
///
/// A mismatch is a real finding, not a tolerable difference: PromptAssembly's
/// `render_determined` proves the render depends only on the variables actually
/// read, so disagreement means either the sanitizer is nondeterministic or a
/// contributing input was never pinned.
pub async fn reconstruct(
    node: &EmbeddedNode,
    request_id: &str,
    turn_index: usize,
    attempt: u32,
) -> Result<ReconstructOutcome> {
    let Some(envelope) = load_envelope(node, request_id, turn_index, attempt).await? else {
        return Ok(ReconstructOutcome::Unreconstructible {
            reason: "no rendered-request envelope for this turn".to_string(),
        });
    };

    if envelope.config_cids_json == "{}" {
        return Ok(ReconstructOutcome::Unreconstructible {
            reason: "captured before config collections became branchable".to_string(),
        });
    }

    let messages = load_canonical_messages(node, request_id, turn_index).await?;
    let sanitized = crate::compaction::sanitize_history_for_provider(messages);
    let messages_json = serde_json::to_value(&sanitized)?;
    let actual = crate::rendered_request::sha256_canonical_json(&messages_json)?;

    if actual == envelope.prompt_hash {
        Ok(ReconstructOutcome::Verified { messages_json })
    } else {
        Ok(ReconstructOutcome::HashMismatch {
            expected: envelope.prompt_hash,
            actual,
        })
    }
}
```

`sha256_canonical_json` is currently private in `rendered_request.rs:140`; change it to `pub(crate)` so the reconstructor uses the identical hasher rather than a copy. A second implementation would defeat the verification.

Implement `load_envelope` (query `RenderedRequest` filtered by `request_id`, `turn_index`, `attempt`) and `load_canonical_messages` (query `AgentMessage` for the request, bounded by the `CompactionEntry` state that applied at that turn) following the query-building patterns in `run_timeline_fetch.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p gents rendered_request_reconstruct`
Expected: PASS (both)

- [ ] **Step 5: Run the full package suite**

Run: `cargo test -p gents`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/rendered_request/reconstruct.rs crates/gents/src/rendered_request.rs \
        crates/gents/tests/e2e_runtime/rendered_request_reconstruct.rs
git commit -m "feat(rendered-request): reconstruct and verify provider inputs"
```

---

### Task 6: Surface token usage and trigger lineage in the run timeline

**Files:**
- Modify: `crates/gents/src/run_timeline_fetch.rs:88-113` (request selection)
- Modify: `crates/gents/src/run_timeline_fetch.rs:347-370` (inference-call selection)
- Modify: `crates/gents/src/run_timeline.rs` (row structs)
- Test: `crates/gents/tests/e2e_runtime/rendered_request_capture.rs` (extend)

**Interfaces:**
- Consumes: nothing new.
- Produces: `TimelineInferenceCallRow` gains `prompt_tokens`, `completion_tokens`, `cached_input_tokens`; the request row gains `caused_by_trigger_id`, `caused_by_trigger_kind`, `execution_origin`.

- [ ] **Step 1: Write the failing test**

```rust
/// The run timeline must carry the token counts that are already persisted on
/// InferenceCall — without them a trace cannot be costed or used as a sample.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_timeline_includes_inference_token_usage() {
    let harness = crate::support::runtime_harness().await;
    harness.submit_prompt("say READY and stop").await;
    harness.wait_for_terminal_response().await;

    let timeline = harness.run_timeline().await;
    let call = timeline
        .inference_calls
        .first()
        .expect("at least one inference call");

    assert!(
        call.prompt_tokens.is_some() || call.completion_tokens.is_some(),
        "timeline must expose persisted token usage"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p gents run_timeline_includes_inference_token_usage`
Expected: FAIL — no such field on the row struct.

- [ ] **Step 3: Add the fields to the row structs**

In `run_timeline.rs`, add to the inference-call row:

```rust
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
```

and to the request row:

```rust
    pub caused_by_trigger_id: Option<String>,
    pub caused_by_trigger_kind: Option<String>,
    pub execution_origin: Option<String>,
```

- [ ] **Step 4: Select the fields in the queries**

In `run_timeline_fetch.rs`, add `prompt_tokens completion_tokens cached_input_tokens` to the `InferenceCall` selection set at lines 347-370, and `caused_by_trigger_id caused_by_trigger_kind execution_origin` to the `AgentRequest` selection at lines 88-113. Map them through the existing deserialization for those rows.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p gents run_timeline_includes_inference_token_usage`
Expected: PASS

- [ ] **Step 6: Verify projections still validate**

Run: `cargo test -p gents adapter_projection`
Expected: PASS. Adapter projections validate against a contract before serializing; new timeline fields must not break that.

- [ ] **Step 7: Commit**

```bash
git add crates/gents/src/run_timeline.rs crates/gents/src/run_timeline_fetch.rs \
        crates/gents/tests/e2e_runtime/rendered_request_capture.rs
git commit -m "feat(timeline): expose inference token usage and trigger lineage"
```

---

## Final Verification

- [ ] Run the full package suite: `cargo test -p gents`
- [ ] Compile the whole workspace: `cargo check --workspace --all-targets`
- [ ] Build the proofs: `cd crates/gents/proofs && lake build`

If any transcript or tool-call invariant changed, update Lean first per the foundation flow and emit conformance witnesses before landing.

## Soft Spots — resolve before executing these steps

Three places where this plan describes intent without literal code. Each needs
a short investigation pass first; do not improvise past them.

1. **Task 5, Step 3 — `load_canonical_messages`.** Loading the messages *as
   they stood at that turn* requires applying the `CompactionEntry` state that
   applied then, not the current one. `drop_compacted_prefix`
   (`agent/daemon/request.rs:373-380`) is the production analogue. Read it and
   the `CompactionEntry` schema before writing this; a naive "all messages for
   the request" load will hash-mismatch on every compacted session and look
   like a capture bug.
2. **Task 4, Step 5 — skill CIDs.** Whether skill document ids are in scope at
   the capture site is unverified. If they are not, leave the map empty; the
   `{}`-means-unreconstructible rule in Task 5 keeps that honest.
3. **Encryption.** The spec requires the envelope be no more widely readable
   than the documents it references, and leaves the mechanism here. This plan
   does not implement it. Confirm what DefraDB document encryption is already
   applied to `AgentMessage`, and match it — or record that the collections are
   equally readable and no additional step is needed.

## Known Gaps Handed to Plan 2

- `tool_call_key` on `AgentToolResult`, the `read_tool_result` action, executable stubs, and the live multi-turn e2e are Plan 2 (spec Part 1).
- Skill CID population in Task 4 Step 5 may be left empty if skill document ids are not in scope at the capture site. If so, that is a tracked gap, not a silent omission — reconstruction of skill-bearing preambles will report `Unreconstructible` rather than a false `Verified`.
