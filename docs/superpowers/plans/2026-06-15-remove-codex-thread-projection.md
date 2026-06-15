# Remove CodexThreadProjection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the `CodexThreadProjection` collection and derive every Codex-shim thread field from `AgentSession`/`AgentConversation`, runtime state, in-process maps, and real `InferenceCall` token usage.

**Architecture:** The Codex shim stops reading/writing a dedicated DefraDB collection. A server-scoped in-process sidecar on `ShimState` holds ephemeral UI toggles (cwd override, loaded, archived, memory_mode, settings, goal). Thread records are assembled from the eager `AgentSession` spine (scoped by `agent_did + behavior_id`), left-joined with `AgentConversation` for title/preview/timestamps, with git info derived from cwd. Token usage is read from `InferenceCall` and surfaced via `ThreadTokenUsageUpdated` plus the thread goal.

**Tech Stack:** Rust, DefraDB GraphQL, `codex-app-server-protocol` (v2), tokio.

**Spec:** `docs/superpowers/specs/2026-06-15-remove-codex-thread-projection-design.md`

**Reference issue:** #494. Follow-up for deeper token fidelity: #498.

---

## Conventions for every task

- **GraphQL safety:** interpolated strings MUST go through `defra_agent::graphql::escape_graphql_string`. Never emit `[]` in a mutation — use `null`.
- **Logging:** `tracing`, never `println`.
- **Mid-refactor verification:** Tasks 1–8 verify with `cargo build -p defra-agent-cli` plus the named targeted tests. The **full** gate `cargo test -p defra-agent && cargo test -p defra-agent-cli` runs at Tasks 9–10. No `lake build` is required (no Lean / ApplyReconcile touched).
- **Commit after each task.**

## File map

| File | Responsibility | Tasks |
|---|---|---|
| `crates/defra-agent-cli/src/commands/codex_shim.rs` | `ShimState` + `CodexSidecar` struct + async accessors | 1 |
| `crates/defra-agent-cli/src/commands/codex_shim/turn.rs`, `turn/stream.rs`, `handlers/thread.rs` | cwd reads move to sidecar; token emission | 1, 8 |
| `crates/defra-agent-cli/src/commands/codex_shim/host_runtime/git.rs` | `thread_git_info(cwd)` helper | 2 |
| `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/storage.rs` | `ensure_agent_session` (+`agent_did`), scoped session load/list, record assembly | 3, 5 |
| `crates/defra-agent-cli/src/commands/codex_shim/thread_projection.rs` | record assembly from session/conversation/sidecar | 5 |
| `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/json.rs` | git derive + `AgentSession.started` timestamp fallback | 5 |
| `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/mutations.rs` | setters → sidecar; name → conversation upsert | 6 |
| `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/goal.rs` | goal → in-process + real tokens | 7 |
| `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/usage.rs` (new) | `session_token_usage` / `requests_token_usage` | 4 |
| `crates/defra-agent-cli/src/commands/codex_shim/thread_routes.rs` | list/fork/search use derived records | 5 |
| `crates/defra-agent-schemas/`, `crates/defra-agent-protocol/src/schemas.rs`, `crates/defra-agent/src/{schema.rs,lib.rs,agent/p2p_reconcile/*}`, `crates/defra-agent-cli/src/main.rs` | collection deletion | 9 |
| `crates/defra-agent-cli/tests/cli_codex_shim.rs` | test migration + new behavior tests | 10 |

---

## Task 1: Server-scoped sidecar on ShimState (move cwd off ConnectionState)

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_shim.rs`
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/turn.rs:53-58`
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/turn/stream.rs` (cwd reads, if any)
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/handlers/thread.rs` (the four `connection.thread_cwds...insert(...)` sites + the `ThreadSettingsUpdate` insert at ~279)

- [ ] **Step 1: Add the `CodexSidecar` struct and field to `ShimState`**

In `codex_shim.rs`, near the `ShimState` struct, add the struct below. The `goal` map is deliberately **not** here yet — `StoredGoal` becomes shareable only in Task 7, which adds the `goal` field then. (Ensure `use std::collections::{BTreeMap, BTreeSet};`, `std::path::PathBuf`, `std::sync::Arc`, and `tokio::sync::Mutex` are in scope — most already are.)

```rust
/// Server-scoped, per-thread Codex UI sidecar state. Replaces the
/// `CodexThreadProjection` collection. Keyed by `session_id` (thread id).
/// Lives on `ShimState` (not `ConnectionState`) so derived-record APIs that
/// only receive `&ShimState` can read it and so toggles survive TUI reconnects
/// within one server run.
#[derive(Default)]
pub(crate) struct CodexSidecar {
    pub(crate) cwd: BTreeMap<String, PathBuf>,
    pub(crate) loaded: BTreeSet<String>,
    pub(crate) archived: BTreeSet<String>,
    pub(crate) memory_mode: BTreeMap<String, String>,
    pub(crate) settings: BTreeMap<String, String>,
}
```

Add the field to `ShimState`:

```rust
    sidecar: Arc<Mutex<CodexSidecar>>,
```

And in the `ShimState { ... }` construction (`codex_shim.rs:145`), add:

```rust
        sidecar: Arc::new(Mutex::new(CodexSidecar::default())),
```

- [ ] **Step 2: Add async accessors on `ShimState`**

In the `impl ShimState` block (same file):

```rust
    pub(crate) async fn thread_cwd(&self, thread_id: &str) -> PathBuf {
        self.sidecar
            .lock()
            .await
            .cwd
            .get(thread_id)
            .cloned()
            .unwrap_or_else(|| self.cwd.clone())
    }

    pub(crate) async fn set_thread_cwd(&self, thread_id: &str, cwd: PathBuf) {
        self.sidecar.lock().await.cwd.insert(thread_id.to_string(), cwd);
    }

    pub(crate) async fn is_thread_loaded(&self, thread_id: &str) -> bool {
        self.sidecar.lock().await.loaded.contains(thread_id)
    }

    pub(crate) async fn set_thread_loaded(&self, thread_id: &str, loaded: bool) {
        let mut guard = self.sidecar.lock().await;
        if loaded {
            guard.loaded.insert(thread_id.to_string());
        } else {
            guard.loaded.remove(thread_id);
        }
    }

    pub(crate) async fn loaded_thread_ids(&self) -> Vec<String> {
        // Mirrors the old query's `loaded == true && archived == false`.
        let guard = self.sidecar.lock().await;
        guard
            .loaded
            .iter()
            .filter(|id| !guard.archived.contains(*id))
            .cloned()
            .collect()
    }

    pub(crate) async fn is_thread_archived(&self, thread_id: &str) -> bool {
        self.sidecar.lock().await.archived.contains(thread_id)
    }

    pub(crate) async fn set_thread_archived(&self, thread_id: &str, archived: bool) {
        let mut guard = self.sidecar.lock().await;
        if archived {
            guard.archived.insert(thread_id.to_string());
            guard.loaded.remove(thread_id);
        } else {
            guard.archived.remove(thread_id);
        }
    }

    pub(crate) async fn thread_memory_mode(&self, thread_id: &str) -> String {
        self.sidecar
            .lock()
            .await
            .memory_mode
            .get(thread_id)
            .cloned()
            .unwrap_or_else(|| "disabled".to_string())
    }

    pub(crate) async fn set_thread_memory_mode(&self, thread_id: &str, mode: &str) {
        self.sidecar
            .lock()
            .await
            .memory_mode
            .insert(thread_id.to_string(), mode.to_string());
    }

    pub(crate) async fn thread_settings(&self, thread_id: &str) -> String {
        self.sidecar
            .lock()
            .await
            .settings
            .get(thread_id)
            .cloned()
            .unwrap_or_else(|| "{}".to_string())
    }

    pub(crate) async fn set_thread_settings(&self, thread_id: &str, settings: &str) {
        self.sidecar
            .lock()
            .await
            .settings
            .insert(thread_id.to_string(), settings.to_string());
    }
```

- [ ] **Step 3: Remove `thread_cwds` from `ConnectionState`**

In `codex_shim.rs`, delete the `thread_cwds: Arc<Mutex<BTreeMap<String, PathBuf>>>` field from `ConnectionState` (line ~65) and its initializer (line ~196).

- [ ] **Step 4: Repoint every `connection.thread_cwds` reader/writer to the sidecar**

- `turn.rs:53-58`: replace
  ```rust
      let cwd = connection
          .thread_cwds
          .lock()
          .await
          .get(&thread_id)
          .cloned()
          .unwrap_or_else(|| state.cwd.clone());
  ```
  with
  ```rust
      let cwd = state.thread_cwd(&thread_id).await;
  ```
- `handlers/thread.rs` `ThreadStart` (~38): replace the `connection.thread_cwds.lock().await.insert(thread_id.clone(), cwd.clone());` with `state.set_thread_cwd(&thread_id, cwd.clone()).await;`
- `handlers/thread.rs` `ThreadResume` (~68): replace with `state.set_thread_cwd(&record.session_id, record.cwd.clone()).await;`
- `handlers/thread.rs` `ThreadFork` (~109): replace with `state.set_thread_cwd(&record.session_id, record.cwd.clone()).await;`
- `handlers/thread.rs` `ThreadSettingsUpdate` (~277-283): replace the insert with `state.set_thread_cwd(&params.thread_id, cwd).await;`
- Search the shim subtree for any remaining `thread_cwds` and repoint:
  ```bash
  grep -rn "thread_cwds" crates/defra-agent-cli/src/commands/codex_shim/
  ```
  Repoint each. (Known sites: `turn.rs:54,170`, `handlers/thread.rs`. `turn.rs:170` is a second turn-entry path — same replacement pattern.)

- [ ] **Step 5: Build**

Run: `cargo build -p defra-agent-cli`
Expected: compiles. (`grep -rn "thread_cwds" crates/defra-agent-cli/src/commands/codex_shim/` returns nothing.)

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim.rs crates/defra-agent-cli/src/commands/codex_shim/
git commit -m "refactor(codex): server-scoped sidecar on ShimState; move thread cwd off ConnectionState"
```

---

## Task 2: `thread_git_info(cwd)` helper

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/host_runtime/git.rs`
- Test: `crates/defra-agent-cli/tests/cli_codex_shim.rs` (added later in Task 10; logic verified by build here)

- [ ] **Step 1: Add a non-fatal git-info helper**

Append to `host_runtime/git.rs`. It reuses the existing private `run_git`/`run_git_output` helpers in that file (which already shell out to `git` with a cwd). Returns `None` for non-git dirs or any failure — never errors.

```rust
/// Lightweight git metadata for a thread cwd: sha/branch/origin.
/// Returns `None` for non-git directories or any git failure — callers must
/// treat absence as "no gitInfo", never a hard error.
pub(in crate::commands::codex_shim) async fn thread_git_info(
    cwd: &std::path::Path,
) -> Option<serde_json::Value> {
    let sha = run_git(cwd, &["rev-parse", "HEAD"]).await.ok()?;
    let sha = sha.trim();
    if sha.is_empty() {
        return None;
    }
    let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty() && b != "HEAD");
    let origin = run_git(cwd, &["config", "--get", "remote.origin.url"])
        .await
        .ok()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty());
    Some(serde_json::json!({
        "sha": sha,
        "branch": branch,
        "originUrl": origin,
    }))
}
```

If `run_git` is not visible from the helper's position, move the new function below `run_git`'s definition in the same module (it is `async fn run_git(cwd, args)` at `git.rs:46`).

- [ ] **Step 2: Build**

Run: `cargo build -p defra-agent-cli`
Expected: compiles. (Helper is unused until Task 5 — that is fine; `#[allow(dead_code)]` is not needed because Task 5 lands in the same PR, but if a warning-as-error config complains, add `#[allow(dead_code)]` and remove it in Task 5.)

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim/host_runtime/git.rs
git commit -m "feat(codex): add non-fatal thread_git_info(cwd) helper"
```

---

## Task 3: `AgentSession` writes `agent_did`; scoped session load/list helpers

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/storage.rs`

- [ ] **Step 1: Write `agent_did` in `ensure_agent_session`**

In `storage.rs` `ensure_agent_session` (`:89`), add the agent_did to the `add` block. `agent_did` is `@immutable`, so it belongs only in `add`, never `update`. The shim's DID is `state.agent_did`.

Add near the other escaped values:
```rust
    let escaped_agent_did = escape_graphql_string(state.agent_did.as_ref());
```
Then in the mutation's `add: { ... }` block, add the line:
```rust
                    agent_did: "{escaped_agent_did}",
```
(Place it after `agent_name`. Do NOT add it to the `update` block.)

- [ ] **Step 2: Add scoped session load + list helpers**

These back the derived record assembly in Task 5. Both filter on the shim's `agent_did` + `behavior_id`. Add to `storage.rs`:

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub(super) struct SessionRow {
    pub(super) session_id: String,
    #[serde(default)]
    pub(super) started: Option<String>,
}

/// Load a single AgentSession scoped to the shim's identity. Returns `None`
/// for unknown ids OR sessions owned by another (agent_did, behavior_id) —
/// including pre-upgrade sessions written without an agent_did (accepted loss).
pub(super) async fn load_scoped_session(
    state: &ShimState,
    session_id: &str,
) -> Result<Option<SessionRow>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(state.agent_did.as_ref());
    let escaped_behavior_id = escape_graphql_string(state.behavior_id.as_ref());
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    behavior_id: {{ _eq: "{escaped_behavior_id}" }}
                }},
                limit: 1
            ) {{ session_id started }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    response
        .pointer("/data/AgentSession/0")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("decoding AgentSession row")
}

/// List all AgentSessions scoped to the shim's identity, newest first.
pub(super) async fn list_scoped_sessions(state: &ShimState) -> Result<Vec<SessionRow>> {
    let escaped_agent_did = escape_graphql_string(state.agent_did.as_ref());
    let escaped_behavior_id = escape_graphql_string(state.behavior_id.as_ref());
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    behavior_id: {{ _eq: "{escaped_behavior_id}" }}
                }},
                order: {{ started: DESC }}
            ) {{ session_id started }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    Ok(response
        .pointer("/data/AgentSession")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| serde_json::from_value(row).ok())
        .collect())
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p defra-agent-cli`
Expected: compiles (new helpers unused until Task 5 — acceptable within this PR; add `#[allow(dead_code)]` if the build denies warnings, remove in Task 5).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim/thread_projection/storage.rs
git commit -m "feat(codex): write agent_did on AgentSession; add identity-scoped session helpers"
```

---

## Task 4: Token usage helpers (`usage.rs`)

**Files:**
- Create: `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/usage.rs`
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/thread_projection.rs` (add `mod usage;`)

- [ ] **Step 1: Create the usage module**

`InferenceCall` carries real provider tokens (`prompt_tokens`/`completion_tokens`) keyed by `request_id`. `AgentResponse` carries the per-request word-count proxy `token_count` + `session_id`. Strategy: gather the session's `(request_id, proxy_output)` from `AgentResponse`, then real `(request_id, prompt, completion)` from `InferenceCall`; per request prefer real, else proxy output.

Create `thread_projection/usage.rs`:

```rust
use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
use serde_json::Value;

use crate::commands::codex_shim::store::query_node_json;
use crate::commands::codex_shim::ShimState;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::commands::codex_shim) struct TokenTotals {
    pub(in crate::commands::codex_shim) input_tokens: i64,
    pub(in crate::commands::codex_shim) output_tokens: i64,
}

impl TokenTotals {
    pub(in crate::commands::codex_shim) fn total(&self) -> i64 {
        self.input_tokens + self.output_tokens
    }
}

/// Per-request token contribution: real InferenceCall usage when present,
/// else the AgentResponse word-count proxy (output only).
struct RequestUsage {
    proxy_output: i64,
    real_input: Option<i64>,
    real_output: Option<i64>,
}

async fn gather_request_usage(
    state: &ShimState,
    request_ids: &[String],
) -> Result<std::collections::BTreeMap<String, RequestUsage>> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, RequestUsage> = BTreeMap::new();
    if request_ids.is_empty() {
        return Ok(map);
    }
    let id_list = request_ids
        .iter()
        .map(|id| format!("\"{}\"", escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");

    // Proxy output per request from AgentResponse.
    let response_query = format!(
        r#"{{
            AgentResponse(filter: {{ request_id: {{ _in: [{id_list}] }} }}) {{
                request_id token_count
            }}
        }}"#
    );
    let resp = query_node_json(&state.node, &response_query).await?;
    for row in resp
        .pointer("/data/AgentResponse")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(rid) = row.get("request_id").and_then(Value::as_str) else {
            continue;
        };
        let proxy = row.get("token_count").and_then(Value::as_i64).unwrap_or(0);
        map.entry(rid.to_string())
            .or_insert(RequestUsage {
                proxy_output: 0,
                real_input: None,
                real_output: None,
            })
            .proxy_output += proxy;
    }

    // Real usage per request from InferenceCall (summed across attempts).
    let call_query = format!(
        r#"{{
            InferenceCall(filter: {{ request_id: {{ _in: [{id_list}] }} }}) {{
                request_id prompt_tokens completion_tokens
            }}
        }}"#
    );
    let calls = query_node_json(&state.node, &call_query).await?;
    for row in calls
        .pointer("/data/InferenceCall")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(rid) = row.get("request_id").and_then(Value::as_str) else {
            continue;
        };
        let entry = map.entry(rid.to_string()).or_insert(RequestUsage {
            proxy_output: 0,
            real_input: None,
            real_output: None,
        });
        if let Some(p) = row.get("prompt_tokens").and_then(Value::as_i64) {
            *entry.real_input.get_or_insert(0) += p;
        }
        if let Some(c) = row.get("completion_tokens").and_then(Value::as_i64) {
            *entry.real_output.get_or_insert(0) += c;
        }
    }
    Ok(map)
}

fn fold(map: &std::collections::BTreeMap<String, RequestUsage>) -> TokenTotals {
    let mut totals = TokenTotals::default();
    for usage in map.values() {
        totals.input_tokens += usage.real_input.unwrap_or(0);
        // Prefer real output; fall back to the proxy when no real usage exists.
        totals.output_tokens += match usage.real_output {
            Some(real) => real,
            None => usage.proxy_output,
        };
    }
    totals
}

/// Token totals for an explicit set of request ids (one Codex turn's chain).
pub(in crate::commands::codex_shim) async fn requests_token_usage(
    state: &ShimState,
    request_ids: &[String],
) -> Result<TokenTotals> {
    Ok(fold(&gather_request_usage(state, request_ids).await?))
}

/// Cumulative token totals across all of a session's requests.
pub(in crate::commands::codex_shim) async fn session_token_usage(
    state: &ShimState,
    session_id: &str,
) -> Result<TokenTotals> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}) {{
                request_id
            }}
        }}"#
    );
    let resp = query_node_json(&state.node, &query)
        .await
        .context("listing session request ids for token usage")?;
    let request_ids: Vec<String> = resp
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            row.get("request_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect();
    requests_token_usage(state, &request_ids).await
}

/// Build the Codex token-usage notification payload. input/output only;
/// cached/reasoning breakdown and context window deferred to #498.
pub(in crate::commands::codex_shim) fn thread_token_usage(
    total: TokenTotals,
    last: TokenTotals,
) -> codex_app_server_protocol::ThreadTokenUsage {
    use codex_app_server_protocol as codex;
    let breakdown = |t: TokenTotals| codex::TokenUsageBreakdown {
        total_tokens: t.total(),
        input_tokens: t.input_tokens,
        cached_input_tokens: 0,
        output_tokens: t.output_tokens,
        reasoning_output_tokens: 0,
    };
    codex::ThreadTokenUsage {
        total: breakdown(total),
        last: breakdown(last),
        model_context_window: None,
    }
}
```

- [ ] **Step 2: Register the module**

In `thread_projection.rs`, add to the `mod` list:
```rust
mod usage;
```
And re-export what later tasks need:
```rust
pub(super) use usage::{session_token_usage, thread_token_usage, requests_token_usage, TokenTotals};
```

- [ ] **Step 3: Build**

Run: `cargo build -p defra-agent-cli`
Expected: compiles. Confirm the `ThreadTokenUsage`/`TokenUsageBreakdown` field names match the protocol (verified: `total_tokens, input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens`; `total, last, model_context_window`).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim/thread_projection/usage.rs crates/defra-agent-cli/src/commands/codex_shim/thread_projection.rs
git commit -m "feat(codex): real InferenceCall token usage helpers with proxy fallback"
```

---

## Task 5: Derive the thread record (stop reading the collection)

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/thread_projection.rs` (record assembly)
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/storage.rs` (drop projection row/IO)
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/json.rs` (git derive + `started` fallback)

- [ ] **Step 1: Drop the projection write/list IO no longer used after this task**

In `storage.rs` delete: `ProjectionUpdate` (struct + impl), `upsert_projection`, `update_projection_loaded_cwd`, `list_projection_rows`. Keep: `ensure_agent_session`, `ensure_agent_session_pinning`, `load_conversation`, `load_scoped_session`, `list_scoped_sessions`, `SessionRow`.

> **Do NOT delete `load_projection`, `ProjectionRow`, `default_memory_mode`, or `empty_json_object` here** — `goal.rs` still imports `load_projection` (`goal.rs:11`) and would fail to compile. Those are deleted in Task 7 (Goal), the last consumer, after `goal.rs` stops using them. `ProjectionRow` keeps `default_memory_mode`/`empty_json_object` alive via its serde defaults until then.

> This is where the dead `rollback_user_turn` field disappears — it existed only on `ProjectionUpdate` being deleted here. No other code references it (verified in the spec audit).

- [ ] **Step 2: Rebuild `CodexThreadRecord` assembly in `thread_projection.rs`**

The public record keeps its shape so callers are unchanged, but fields now come from session/conversation/sidecar/git. Replace `load_codex_thread`, `create_codex_thread`, `resume_codex_thread`, `list_codex_threads_by_archived`, `store_forked_codex_thread` bodies.

Add an internal assembler (`thread_projection.rs` is the module root, so import its own `storage` submodule with `use storage::{...}`; the git helper is reached by its full path):

```rust
use storage::{list_scoped_sessions, load_conversation, load_scoped_session};
use crate::commands::codex_shim::host_runtime::git::thread_git_info;

async fn assemble_record(
    state: &ShimState,
    session_id: &str,
    started: Option<String>,
    conversation: Option<ConversationRow>,
) -> CodexThreadRecord {
    let cwd = state.thread_cwd(session_id).await;
    let git_info = thread_git_info(&cwd).await;
    CodexThreadRecord {
        session_id: session_id.to_string(),
        cwd,
        archived: state.is_thread_archived(session_id).await,
        loaded: state.is_thread_loaded(session_id).await,
        memory_mode: state.thread_memory_mode(session_id).await,
        name: String::new(), // name derives from conversation.title in json.rs
        settings_json: state.thread_settings(session_id).await,
        git_info, // NEW field — see Step 3
        projection_started: started,
        conversation,
    }
}
```

Adjust the `CodexThreadRecord` struct in `thread_projection.rs`:
- Remove fields: `goal_json` (goal now serves from the sidecar in `goal.rs`; no reader uses `record.goal_json`), `git_info_json` (replaced by structured `git_info: Option<Value>`), `projection_created_at`, `projection_updated_at`.
- Add: `git_info: Option<serde_json::Value>`, `projection_started: Option<String>` (the `AgentSession.started` timestamp fallback).
- Keep: `session_id`, `cwd`, `archived`, `loaded`, `memory_mode`, `name`, `settings_json`, `conversation`.

> Decision note: `name` and `memory_mode`/`settings_json` are still record fields because `json.rs`/handlers read them. `name` stays `String::new()` here — `json.rs` already falls back to `conversation.title`, which is now the canonical name. `set_codex_thread_name` (Task 6) writes the conversation, so the title carries the name.

`load_codex_thread` — **the scoped session is the gate**. A conversation-only or pre-upgrade (`agent_did`-less) row must NOT load; only when a scoped `AgentSession` exists do we left-join the conversation:
```rust
pub(super) async fn load_codex_thread(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<CodexThreadRecord>> {
    let Some(session) = load_scoped_session(state, thread_id).await? else {
        return Ok(None);
    };
    let conversation = load_conversation(state, thread_id).await?;
    Ok(Some(
        assemble_record(state, thread_id, session.started, conversation).await,
    ))
}
```

`create_codex_thread`:
```rust
pub(super) async fn create_codex_thread(
    state: &ShimState,
    thread_id: &str,
    cwd: &Path,
) -> Result<CodexThreadRecord> {
    ensure_agent_session(state, thread_id).await?;
    state.set_thread_cwd(thread_id, cwd.to_path_buf()).await;
    state.set_thread_loaded(thread_id, true).await;
    state.set_thread_memory_mode(thread_id, "disabled").await;
    load_codex_thread(state, thread_id)
        .await?
        .with_context(|| format!("loading newly-created Codex thread {thread_id}"))
}
```

`resume_codex_thread` — **returns `Option`** so the handler can answer a scoped miss with a clean JSON-RPC error instead of unwinding (which closes the websocket). `ensure_agent_session` creates a scoped session for genuinely-new ids; a pre-upgrade (`agent_did`-less) id takes the upsert update-path (no `agent_did` added, since it is `@immutable`), so `load_codex_thread` then returns `None`:
```rust
pub(super) async fn resume_codex_thread(
    state: &ShimState,
    thread_id: &str,
    cwd_override: Option<&str>,
) -> Result<Option<CodexThreadRecord>> {
    let cwd = match cwd_override.filter(|v| !v.trim().is_empty()) {
        Some(v) => PathBuf::from(v),
        None => state.thread_cwd(thread_id).await,
    };
    ensure_agent_session(state, thread_id).await?;
    state.set_thread_cwd(thread_id, cwd).await;
    state.set_thread_loaded(thread_id, true).await;
    let record = load_codex_thread(state, thread_id).await?;
    if record.is_none() {
        // Scoped miss (pre-upgrade / foreign id): don't leave a ghost in the
        // loaded set / ThreadLoadedList.
        state.set_thread_loaded(thread_id, false).await;
    }
    Ok(record)
}
```
(`set_thread_loaded(.., true)` is called before the load so the returned record reflects `loaded: true`; it is reverted only on a scoped miss.)

`list_codex_threads_by_archived`:
```rust
pub(super) async fn list_codex_threads_by_archived(
    state: &ShimState,
    archived: bool,
) -> Result<Vec<CodexThreadRecord>> {
    let sessions = list_scoped_sessions(state).await?;
    let mut records = Vec::with_capacity(sessions.len());
    for session in sessions {
        if state.is_thread_archived(&session.session_id).await != archived {
            continue;
        }
        let conversation = load_conversation(state, &session.session_id).await?;
        records.push(assemble_record(state, &session.session_id, session.started, conversation).await);
    }
    Ok(records)
}
```

`store_forked_codex_thread`:
```rust
pub(super) async fn store_forked_codex_thread(
    state: &ShimState,
    source: &CodexThreadRecord,
    child_session_id: &str,
    cwd: &Path,
) -> Result<CodexThreadRecord> {
    ensure_agent_session(state, child_session_id).await?;
    state.set_thread_cwd(child_session_id, cwd.to_path_buf()).await;
    state.set_thread_loaded(child_session_id, true).await;
    state
        .set_thread_memory_mode(child_session_id, &source.memory_mode)
        .await;
    state
        .set_thread_settings(child_session_id, &source.settings_json)
        .await;
    load_codex_thread(state, child_session_id)
        .await?
        .with_context(|| format!("loading forked Codex thread {child_session_id}"))
}
```

- [ ] **Step 3: Update `json.rs` — git from record, timestamp fallback to `started`**

In `json.rs`:
- Replace `if let Some(git_info) = codex_git_info_json(&record.git_info_json)` with:
  ```rust
      if let Some(git_info) = record.git_info.clone() {
          object.insert("gitInfo".to_string(), git_info);
      }
  ```
  and delete the now-unused `codex_git_info_json` function.
- In `thread_created_at`, replace `.or(record.projection_created_at.as_deref())` with `.or(record.projection_started.as_deref())`.
- In `thread_updated_at`, replace both `record.projection_updated_at`/`record.projection_created_at` references with `record.projection_started.as_deref()` (conversation timestamps still take precedence; `started` is the zero-turn fallback).

- [ ] **Step 4: Update the other readers of the removed record fields**

The struct field rename ripples beyond `json.rs`:

- `thread_routes.rs` `thread_sort_timestamp` (`:282-295`): replace both `record.projection_created_at.clone()` and `record.projection_updated_at.clone()` with `record.projection_started.clone()`. Final form:
  ```rust
  fn thread_sort_timestamp(record: &CodexThreadRecord, sort_key: codex::ThreadSortKey) -> String {
      let conversation = record.conversation.as_ref();
      match sort_key {
          codex::ThreadSortKey::CreatedAt => conversation
              .and_then(|c| c.created_at.clone())
              .or_else(|| record.projection_started.clone()),
          codex::ThreadSortKey::UpdatedAt => conversation
              .and_then(|c| c.updated_at.clone())
              .or_else(|| conversation.and_then(|c| c.created_at.clone()))
              .or_else(|| record.projection_started.clone()),
      }
      .unwrap_or_default()
  }
  ```
- `history_projection.rs` summary (`:406`): replace `"gitInfo": codex_git_info_summary_json(&record.git_info_json),` with `"gitInfo": record.git_info.clone(),`. Delete the now-unused `codex_git_info_summary_json` function (or, if its shaping differs from `thread_git_info`'s output and a summary-specific shape is required, have it take `&Option<serde_json::Value>` — but the derived `git_info` already has `sha`/`branch`/`originUrl`, so direct use is correct).
- `history_projection.rs` fallback record construction (`:826-836`): this builds a `CodexThreadRecord` literal with the old fields. Replace `git_info_json: "{}".to_string(),`, `projection_created_at: None,`, `projection_updated_at: None,` with `git_info: None,` and `projection_started: None,`; remove `goal_json: ...` if present.

- [ ] **Step 5: Handle the `resume_codex_thread` Option in callers**

`resume_codex_thread` now returns `Option`. Update both call sites in `handlers/thread.rs` to answer a scoped miss with `JSONRPC_INVALID_PARAMS` instead of `?`-unwinding:

- `ThreadResume` (~:66):
  ```rust
  let Some(record) = resume_codex_thread(state, &params.thread_id, params.cwd.as_deref()).await?
  else {
      return send_error(
          outbound,
          request_id,
          JSONRPC_INVALID_PARAMS,
          format!("unknown Codex thread `{}`", params.thread_id),
      )
      .await;
  };
  ```
- `ThreadUnarchive` (~:252):
  ```rust
  let Some(record) = resume_codex_thread(state, &params.thread_id, None).await? else {
      return send_error(
          outbound,
          request_id,
          JSONRPC_INVALID_PARAMS,
          format!("unknown Codex thread `{}`", params.thread_id),
      )
      .await;
  };
  ```

- [ ] **Step 6: Build**

Run: `cargo build -p defra-agent-cli`
Expected: compiles. Fix any caller that referenced removed fields (`git_info_json`, `projection_created_at`, `projection_updated_at`, `goal_json`) — search:
```bash
grep -rn "git_info_json\|projection_created_at\|projection_updated_at\|\.goal_json" crates/defra-agent-cli/src/commands/codex_shim/
```

- [ ] **Step 7: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim/thread_projection.rs crates/defra-agent-cli/src/commands/codex_shim/thread_projection/ crates/defra-agent-cli/src/commands/codex_shim/thread_routes.rs crates/defra-agent-cli/src/commands/codex_shim/history_projection.rs crates/defra-agent-cli/src/commands/codex_shim/handlers/thread.rs
git commit -m "refactor(codex): derive thread record from AgentSession/AgentConversation + sidecar + git"
```

---

## Task 6: Setters write the sidecar; `set_codex_thread_name` upserts the conversation

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/mutations.rs`

- [ ] **Step 1: Repoint the toggle setters to the sidecar**

Rewrite each setter in `mutations.rs` to call the `ShimState` accessor instead of a GraphQL mutation:

```rust
pub(in crate::commands::codex_shim) async fn loaded_codex_thread_ids(
    state: &ShimState,
) -> Result<Vec<String>> {
    Ok(state.loaded_thread_ids().await)
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_loaded(
    state: &ShimState,
    thread_id: &str,
    loaded: bool,
) -> Result<()> {
    state.set_thread_loaded(thread_id, loaded).await;
    Ok(())
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_archived(
    state: &ShimState,
    thread_id: &str,
    archived: bool,
) -> Result<()> {
    state.set_thread_archived(thread_id, archived).await;
    Ok(())
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_memory_mode(
    state: &ShimState,
    thread_id: &str,
    mode: codex::ThreadMemoryMode,
) -> Result<()> {
    state.set_thread_memory_mode(thread_id, mode.as_str()).await;
    Ok(())
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_settings(
    state: &ShimState,
    thread_id: &str,
    settings: &codex::ThreadSettingsUpdateParams,
) -> Result<()> {
    let settings_json =
        serde_json::to_string(settings).context("encoding Codex thread settings")?;
    state.set_thread_settings(thread_id, &settings_json).await;
    if let Some(cwd) = settings.cwd.as_deref() {
        let cwd = if cwd.is_absolute() {
            cwd.to_path_buf()
        } else {
            state.cwd.join(cwd)
        };
        state.set_thread_cwd(thread_id, cwd).await;
    }
    Ok(())
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_git_info(
    state: &ShimState,
    thread_id: &str,
    _git_info: &Option<codex::ThreadMetadataGitInfoUpdateParams>,
) -> Result<Option<CodexThreadRecord>> {
    // git info is now derived from cwd at read time; the client-supplied value
    // is ignored. Return the freshly-derived record.
    load_codex_thread(state, thread_id).await
}
```
Remove now-unused imports (`escape_graphql_string`, `query_node_json`, `absolute_path`, `Value`) if they become dead — let the compiler guide.

- [ ] **Step 2: `set_codex_thread_name` upserts the conversation (create-if-absent, scoped)**

A freshly-started thread has no `AgentConversation` row; the runtime's update-only title helper would no-op. Upsert directly from the shim with full identity so an early rename persists — but **only if a scoped `AgentSession` exists**, so an unknown or pre-upgrade (foreign/`agent_did`-less) id cannot create a durable orphan conversation. Returns `Ok(false)` on a scoped miss so the handler can answer `JSONRPC_INVALID_PARAMS` (consistent with `ThreadResume`); fresh zero-turn renames still work because `ThreadStart` eagerly created the scoped `AgentSession`.

```rust
pub(in crate::commands::codex_shim) async fn set_codex_thread_name(
    state: &ShimState,
    thread_id: &str,
    name: &str,
) -> Result<bool> {
    if super::storage::load_scoped_session(state, thread_id)
        .await?
        .is_none()
    {
        return Ok(false);
    }
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(thread_id);
    let escaped_name = escape_graphql_string(name.trim());
    let escaped_agent_did = escape_graphql_string(state.agent_did.as_ref());
    let escaped_behavior_id = escape_graphql_string(state.behavior_id.as_ref());
    let mutation = format!(
        r#"mutation {{
            upsert_AgentConversation(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                add: {{
                    session_id: "{escaped_session_id}",
                    agent_name: "{escaped_behavior_id}",
                    agent_did: "{escaped_agent_did}",
                    behavior_id: "{escaped_behavior_id}",
                    title: "{escaped_name}",
                    title_source: "user",
                    status: "active",
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    title: "{escaped_name}",
                    title_source: "user",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(true)
}
```
Keep the `escape_graphql_string` and `query_node_json` imports for this function.

- [ ] **Step 3: Handle the `set_codex_thread_name` bool in the `ThreadSetName` handler**

`handlers/thread.rs` `ThreadSetName` (~:263) currently does `set_codex_thread_name(...).await?;` then unconditionally sends success. Update to surface a scoped miss:

```rust
            if set_codex_thread_name(state, &params.thread_id, &params.name).await? {
                send_result(outbound, request_id, codex::ThreadSetNameResponse {}).await
            } else {
                send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    format!("unknown Codex thread `{}`", params.thread_id),
                )
                .await
            }
```

- [ ] **Step 4: Build**

Run: `cargo build -p defra-agent-cli`
Expected: compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim/thread_projection/mutations.rs crates/defra-agent-cli/src/commands/codex_shim/handlers/thread.rs
git commit -m "refactor(codex): thread setters write sidecar; scoped rename upserts AgentConversation.title"
```

---

## Task 7: Goal in-process + real tokens/time

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/goal.rs`
- Modify: `crates/defra-agent-cli/src/commands/codex_shim.rs` (add `goal` field to `CodexSidecar` + accessors)
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/thread_projection.rs` (export `StoredGoal`)

- [ ] **Step 1: Make `StoredGoal` shareable and add it to the sidecar**

In `goal.rs`, change `struct StoredGoal` to `pub(in crate::commands::codex_shim) struct StoredGoal` and re-export it from `thread_projection.rs`:
```rust
pub(in crate::commands::codex_shim) use goal::StoredGoal;
```
In `codex_shim.rs`, add to `CodexSidecar`:
```rust
    pub(crate) goal: BTreeMap<String, crate::commands::codex_shim::thread_projection::StoredGoal>,
```
Add accessors on `ShimState`:
```rust
    pub(crate) async fn thread_goal(
        &self,
        thread_id: &str,
    ) -> Option<crate::commands::codex_shim::thread_projection::StoredGoal> {
        self.sidecar.lock().await.goal.get(thread_id).cloned()
    }

    pub(crate) async fn set_thread_goal(
        &self,
        thread_id: &str,
        goal: crate::commands::codex_shim::thread_projection::StoredGoal,
    ) {
        self.sidecar.lock().await.goal.insert(thread_id.to_string(), goal);
    }

    pub(crate) async fn clear_thread_goal(&self, thread_id: &str) -> bool {
        self.sidecar.lock().await.goal.remove(thread_id).is_some()
    }
```
Ensure `StoredGoal` derives `Clone` (it already derives `Debug, Clone, Deserialize, Serialize`).

- [ ] **Step 2: Rewrite goal read/write against the sidecar with real tokens**

In `goal.rs`, replace `load_projection`-based reads and `update_goal_json` writes with the sidecar, and compute `tokens_used`/`time_used_seconds` dynamically at read:

```rust
use crate::commands::codex_shim::thread_projection::session_token_usage;

pub(in crate::commands::codex_shim) async fn set_codex_thread_goal(
    state: &ShimState,
    params: &codex::ThreadGoalSetParams,
) -> Result<codex::ThreadGoal> {
    let existing = state.thread_goal(&params.thread_id).await;
    let now = now_seconds_i64();
    let status = params
        .status
        .as_ref()
        .copied()
        .or_else(|| existing.as_ref().map(|g| g.status.clone()))
        .unwrap_or(codex::ThreadGoalStatus::Active);
    let token_budget = match &params.token_budget {
        Some(value) => *value,
        None => existing.as_ref().and_then(|g| g.token_budget),
    };
    let created_at = existing.as_ref().map(|g| g.created_at).unwrap_or(now);
    let goal = StoredGoal {
        thread_id: params.thread_id.clone(),
        objective: params
            .objective
            .clone()
            .or_else(|| existing.as_ref().map(|g| g.objective.clone()))
            .unwrap_or_default(),
        status,
        token_budget,
        created_at,
        updated_at: now,
    };
    state.set_thread_goal(&params.thread_id, goal.clone()).await;
    enrich(state, goal).await
}

pub(in crate::commands::codex_shim) async fn get_codex_thread_goal(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<codex::ThreadGoal>> {
    match state.thread_goal(thread_id).await {
        Some(goal) => Ok(Some(enrich(state, goal).await?)),
        None => Ok(None),
    }
}

pub(in crate::commands::codex_shim) async fn clear_codex_thread_goal(
    state: &ShimState,
    thread_id: &str,
) -> Result<bool> {
    Ok(state.clear_thread_goal(thread_id).await)
}

/// Fill the live, derived fields (`tokens_used`, `time_used_seconds`) from real
/// usage + wall clock. These are not stored.
async fn enrich(state: &ShimState, goal: StoredGoal) -> Result<codex::ThreadGoal> {
    let tokens_used = session_token_usage(state, &goal.thread_id).await?.total();
    let time_used_seconds = (now_seconds_i64() - goal.created_at).max(0);
    Ok(codex::ThreadGoal {
        thread_id: goal.thread_id,
        objective: goal.objective,
        status: goal.status,
        token_budget: goal.token_budget,
        tokens_used,
        time_used_seconds,
        created_at: goal.created_at,
        updated_at: goal.updated_at,
    })
}
```

Trim `StoredGoal` to the persisted-only fields (drop `tokens_used`/`time_used_seconds`, now derived):
```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::commands::codex_shim) struct StoredGoal {
    pub(in crate::commands::codex_shim) thread_id: String,
    pub(in crate::commands::codex_shim) objective: String,
    pub(in crate::commands::codex_shim) status: codex::ThreadGoalStatus,
    pub(in crate::commands::codex_shim) token_budget: Option<i64>,
    pub(in crate::commands::codex_shim) created_at: i64,
    pub(in crate::commands::codex_shim) updated_at: i64,
}
```
Delete `into_codex`, `decode_stored_goal`, `update_goal_json`, and the `load_projection` import (no longer used).

- [ ] **Step 3: Delete the now-unused projection read storage (deferred from Task 5)**

`goal.rs` was the last consumer of `load_projection`. Now delete from `storage.rs`: `load_projection`, `ProjectionRow`, `default_memory_mode`, `empty_json_object`. After this, `storage.rs` contains only `ensure_agent_session`, `ensure_agent_session_pinning`, `load_conversation`, `load_scoped_session`, `list_scoped_sessions`, `SessionRow` (+ their imports). Drop any now-unused imports (`absolute_path`, `serde::Deserialize` if unreferenced — let the compiler guide).

- [ ] **Step 4: Build**

Run: `cargo build -p defra-agent-cli`
Expected: compiles. (`grep -rn "load_projection\|ProjectionRow" crates/defra-agent-cli/src/commands/codex_shim/` returns nothing.)

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim.rs crates/defra-agent-cli/src/commands/codex_shim/thread_projection.rs crates/defra-agent-cli/src/commands/codex_shim/thread_projection/goal.rs crates/defra-agent-cli/src/commands/codex_shim/thread_projection/storage.rs
git commit -m "refactor(codex): goal served from sidecar with real token/time usage; drop projection read storage"
```

---

## Task 8: Emit `ThreadTokenUsageUpdated` (turn completion + resume replay)

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/turn/stream.rs`
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/handlers/thread.rs` (`ThreadResume` replay)

- [ ] **Step 1: Track the turn's request chain in the stream loop**

In `stream.rs` `stream_defra_turn`, near where `current` is first set up (before the main loop), introduce a chain accumulator seeded with the initial turn request id:

```rust
    let mut turn_request_ids: Vec<String> = vec![current.request_id.clone()];
```
At the steering-advance site (`stream.rs:~243`, right after `current.request_id = next_request.request_id;`), push the new id:
```rust
                    current.request_id = next_request.request_id;
                    turn_request_ids.push(current.request_id.clone());
```

- [ ] **Step 2: Emit at terminal turn completion**

Immediately before the final `projection.finish_turn(...)` call (`stream.rs:~250`), emit the notification. `outbound` and `state` are in scope.

```rust
            // Token-usage visibility (#494/#498): last = this turn's request
            // chain delta, total = session cumulative.
            {
                use crate::commands::codex_shim::thread_projection::{
                    requests_token_usage, session_token_usage, thread_token_usage,
                };
                let last = requests_token_usage(state, &turn_request_ids)
                    .await
                    .unwrap_or_default();
                let total = session_token_usage(state, &current.session_id)
                    .await
                    .unwrap_or_default();
                send_notification(
                    outbound,
                    state,
                    codex::ServerNotification::ThreadTokenUsageUpdated(
                        codex::ThreadTokenUsageUpdatedNotification {
                            thread_id: projection.thread_id.to_string(),
                            turn_id: projection.turn_id.to_string(),
                            token_usage: thread_token_usage(total, last),
                        },
                    ),
                )
                .await?;
            }
            projection
                .finish_turn(outbound, turn_status, error_message)
                .await?;
```
Confirm `send_notification` is imported in `stream.rs` (it is used elsewhere in the shim; add `use super::...send_notification` if missing — match the existing import path used by `turn_projection.rs`). `TokenTotals::default()` provides the `unwrap_or_default()` fallback.

- [ ] **Step 3: Replay on `ThreadResume`**

In `handlers/thread.rs` `ThreadResume`, after `send_typed_json_result::<codex::ThreadResumeResponse>(...)` succeeds, emit the session total as a `last == total` snapshot so a resumed historical thread is not dark:

```rust
            // Replay token usage so resumed historical threads show a counter.
            {
                use super::super::thread_projection::{session_token_usage, thread_token_usage};
                let total = session_token_usage(state, &record.session_id)
                    .await
                    .unwrap_or_default();
                send_notification(
                    outbound,
                    state,
                    codex::ServerNotification::ThreadTokenUsageUpdated(
                        codex::ThreadTokenUsageUpdatedNotification {
                            thread_id: record.session_id.clone(),
                            turn_id: String::new(),
                            token_usage: thread_token_usage(total, total),
                        },
                    ),
                )
                .await?;
            }
```
Place this as the final action of the `ThreadResume` arm (after the result send). If the arm currently ends by returning the `send_typed_json_result(...).await` expression, bind it to a `let _ = ...?;` first, then emit, then `Ok(())`-style return matching the function's signature.

- [ ] **Step 4: Build**

Run: `cargo build -p defra-agent-cli`
Expected: compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim/turn/stream.rs crates/defra-agent-cli/src/commands/codex_shim/handlers/thread.rs
git commit -m "feat(codex): emit ThreadTokenUsageUpdated on turn completion and resume replay"
```

---

## Task 9: Delete the collection from every registry

**Files:**
- Delete: `crates/defra-agent-schemas/schemas/agent/codex_thread_projection.graphql`
- Modify: `crates/defra-agent-schemas/src/lib.rs`
- Modify: `crates/defra-agent-protocol/src/schemas.rs`
- Modify: `crates/defra-agent/src/schema.rs`, `crates/defra-agent/src/lib.rs`
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/profiles.rs`, `.../templates.rs`
- Modify: `crates/defra-agent-cli/src/main.rs`
- Modify: `crates/defra-agent-desktop-core/src/client/{schema.rs,collection_resolver.rs}` (only if they enumerate the name)

- [ ] **Step 1: defra-agent-schemas**

- Delete `schemas/agent/codex_thread_projection.graphql`.
- In `src/lib.rs`: delete the `CODEX_THREAD_PROJECTION_NAME` and `CODEX_THREAD_PROJECTION` consts (lines ~31-33); remove `CODEX_THREAD_PROJECTION` from `ALL`, `CODEX_THREAD_PROJECTION_NAME` from `ALL_COLLECTION_NAMES`, and the name from `BRANCHABLE_COLLECTION_NAMES`.
- Update the test `all_contains_every_agent_schema` (`src/lib.rs:137`): `assert_eq!(ALL.len(), 22);` (was 23).

- [ ] **Step 2: defra-agent-protocol**

In `src/schemas.rs`: remove `CODEX_THREAD_PROJECTION` and `CODEX_THREAD_PROJECTION_NAME` from the `defra_agent_schemas::{ ... }` re-export (lines ~17-18); remove `CODEX_THREAD_PROJECTION` from the protocol `ALL` (line ~65) and `CODEX_THREAD_PROJECTION_NAME` from `ALL_COLLECTION_NAMES` (line ~94); update `all_contains_every_schema` (`:118`): `assert_eq!(ALL.len(), 26, ...)` (was 27).

- [ ] **Step 3: defra-agent re-exports**

- `src/schema.rs`: remove the `CODEX_THREAD_PROJECTION as ..._SCHEMA` re-export (line ~13 region).
- `src/lib.rs:151`: remove `CODEX_THREAD_PROJECTION_SCHEMA` from the re-export list.

- [ ] **Step 4: reconcile profiles + templates**

- `agent/p2p_reconcile/profiles.rs`: remove `"CodexThreadProjection"` from `RUNTIME_COLLECTIONS` (line ~98) and `CHAT_REQUEST_COLLECTIONS` (line ~134).
- `agent/p2p_reconcile/templates.rs`: remove the assertion `assert!(!t.collections.contains(&"CodexThreadProjection"));` (line ~182) and the explanatory comment referencing it (line ~84).

- [ ] **Step 5: main.rs + desktop-core**

- `crates/defra-agent-cli/src/main.rs`: remove the `("CodexThreadProjection", "session_id"),` entry from `SCHEMA_COLLECTION_CHECKS` (line ~360).
- Check desktop-core:
  ```bash
  grep -rn "CodexThreadProjection\|CODEX_THREAD_PROJECTION" crates/defra-agent-desktop-core/
  ```
  Remove any enumeration found.

- [ ] **Step 6: grep gate**

Run:
```bash
grep -rn "CodexThreadProjection\|CODEX_THREAD_PROJECTION" crates/
```
Expected: **no output**. (Anything left is a missed reference — fix before continuing.)

- [ ] **Step 7: Build the workspace**

Run: `cargo build`
Expected: compiles across all crates.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: delete CodexThreadProjection collection and all registry references (#494)"
```

---

## Task 10: Test migration + new behavior coverage

**Files:**
- Modify: `crates/defra-agent-cli/tests/cli_codex_shim.rs`

- [ ] **Step 1: Migrate the two direct-collection queries**

`cli_codex_shim.rs` queries `CodexThreadProjection` directly (~line 187 and ~line 1812). Replace each with an assertion through observable behavior. For the name/title case, query `AgentConversation`:

```rust
// was: CodexThreadProjection(filter: { session_id ... }) { name ... }
// now: assert the rename landed on the conversation title
let conversation_response = run_graphql(
    &node,
    &format!(
        r#"{{ AgentConversation(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1) {{ title title_source }} }}"#,
        session_id
    ),
)
.await;
let row = first_graphql_row(&conversation_response, "AgentConversation")?;
assert_eq!(row["title"], serde_json::json!("My Thread"));
assert_eq!(row["title_source"], serde_json::json!("user"));
```
Adapt variable names to the surrounding test. For any assertion that checked `cwd`/`memory_mode`/`archived` via the projection, assert via the thread get/list RPC response instead (those fields appear in `codex_thread_json`).

- [ ] **Step 1b: Migrate the `ThreadMetadataUpdate` git-echo assertions**

Two existing tests send `ThreadMetadataUpdate` with a fake client git sha and assert the response echoes it back (`cli_codex_shim.rs:405` → `git.sha == Some("abc123")`; `:1787` → `git.sha == Some(git_sha)`). Task 6 makes `set_codex_thread_git_info` **ignore** client-supplied git and derive from cwd, so these assertions will fail.

Migrate each: run the thread under a temp git repo with a known HEAD sha (init + commit), then assert the response's `git_info.sha` equals the **real** repo sha (and `branch` the real branch), not the client-sent value. Concretely, for each test:
- Set the thread's cwd (via `ThreadStart`/`ThreadSettingsUpdate` `cwd`) to a `tempfile::TempDir` where you've run `git init`, `git commit --allow-empty -m x`, and captured `git rev-parse HEAD`.
- Keep the `ThreadMetadataUpdate` call (it still returns the record) but change the assertion to compare against the captured real sha.
- The `:1787` test also asserts `thread.name == thread_name`; keep that assertion (name now comes from `AgentConversation.title` via the rename path — unaffected, since that test sets the name through `ThreadSetName`/metadata name, not git).

If isolating a temp git repo per test is heavy, the alternative is to drop the sha-echo assertion entirely and assert only that `ThreadMetadataUpdate` succeeds (the derive path is covered by the dedicated test in Step 5). Prefer the real-repo assertion.

- [ ] **Step 2: Add — zero-turn thread is listed**

```rust
#[tokio::test]
async fn codex_zero_turn_thread_is_listed() -> anyhow::Result<()> {
    // Start a thread, do NOT run a turn, then list: it must appear.
    // (Regression guard for AgentSession-spine listing.)
    // ... harness: ThreadStart -> ThreadList(archived:false) ...
    // assert the started thread_id is present in the list response.
}
```
Model the harness on the existing `ThreadStart` + `ThreadList` tests in the same file (reuse their websocket/shim setup helpers). Assert the new `thread_id` is present in the `ThreadListResponse` `data` array (the response field is `data: Vec<Thread>`, not `threads`).

- [ ] **Step 3: Add — early rename persists**

```rust
#[tokio::test]
async fn codex_rename_before_first_turn_persists() -> anyhow::Result<()> {
    // ThreadStart -> ThreadSetName("My Thread") (no turn yet)
    // -> ThreadGet (or AgentConversation query) shows name "My Thread".
}
```

- [ ] **Step 4: Add — memory_mode defaults to disabled (unit test on the accessor)**

`memory_mode` is **not** serialized into the thread JSON and there is no get-memory-mode RPC, so the default is not observable through the protocol. Cover it with a unit test on the `ShimState` accessor instead. Add a `#[cfg(test)] mod` near the `CodexSidecar`/`ShimState` accessors in `codex_shim.rs` (or extend an existing test module there):

```rust
#[tokio::test]
async fn thread_memory_mode_defaults_disabled() {
    // A sidecar with no entry for the thread returns "disabled".
    let sidecar = std::sync::Arc::new(tokio::sync::Mutex::new(CodexSidecar::default()));
    let mode = sidecar.lock().await.memory_mode.get("t1").cloned()
        .unwrap_or_else(|| "disabled".to_string());
    assert_eq!(mode, "disabled");
}
```
If a full `ShimState` is cheap to construct in tests, prefer asserting `state.thread_memory_mode("t1").await == "disabled"` directly. Either way the assertion is the same default.

- [ ] **Step 5: Add — git info derived; non-git yields none**

```rust
#[tokio::test]
async fn codex_git_info_derived_from_cwd() -> anyhow::Result<()> {
    // Start a thread with cwd = a temp git repo (init + commit); ThreadGet
    // shows gitInfo.sha/branch without any ThreadMetadataUpdate call.
    // Start another with a non-git temp dir; ThreadGet has no gitInfo and no error.
}
```

- [ ] **Step 6: Add — token replay on resume**

```rust
#[tokio::test]
async fn codex_resume_replays_token_usage() -> anyhow::Result<()> {
    // Run a turn on a thread (so AgentResponse/InferenceCall rows exist),
    // then ThreadResume; assert a ThreadTokenUsageUpdated notification arrives
    // with token_usage.total.total_tokens > 0.
}
```
The normal `MockChatEndpoint::start` path records usage, so assert `total_tokens > 0` (or the exact mock total if the mock is deterministic) — `>= 0` would pass even with a still-dark counter, defeating the test. Use the same `MockChatEndpoint::start(&model_name, &expected_reply)` setup as the existing turn tests (e.g. `cli_codex_shim.rs:83`).

- [ ] **Step 7: Run the full gate**

Run:
```bash
cargo test -p defra-agent && cargo test -p defra-agent-cli
```
Expected: PASS. Investigate and fix any failure (do not mark flaky — flaky tests are defects per project policy).

- [ ] **Step 8: Commit**

```bash
git add crates/defra-agent-cli/tests/cli_codex_shim.rs
git commit -m "test(codex): migrate off CodexThreadProjection; cover derive + token usage paths"
```

---

## Final verification

- [ ] `grep -rn "CodexThreadProjection\|CODEX_THREAD_PROJECTION" crates/` → empty
- [ ] `cargo test -p defra-agent && cargo test -p defra-agent-cli` → green
- [ ] Spot-check `thread/list`, `thread/setName` before first turn, `thread/goal`, and the token counter against the spec's accepted tradeoffs.
