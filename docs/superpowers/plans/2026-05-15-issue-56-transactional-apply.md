# Transactional `config apply` Implementation Plan (Issue #56)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `defra-agent-cli config apply` atomic by wrapping the apply sequence in a single DefraDB transaction.

**Architecture:** A new `ConfigApplyTxn` type opens a transaction on `ConfigAccess` (HTTP `POST /api/v0/tx/begin` or embedded `runner.begin_txn`) and is threaded through every write site. The top-level CLI command does `begin → apply → commit | discard`. The conformance fence and new integration test exercise both the commit and discard paths.

**Tech Stack:** Rust, anyhow, tokio, reqwest, axum (test recorder), DefraDB's native HTTP and embedded transaction APIs.

**Spec:** `docs/superpowers/specs/2026-05-15-issue-56-transactional-apply-design.md`.

---

## File Structure

**Created:**
- `crates/defra-agent-cli/src/config_writes/txn.rs` — `ConfigApplyTxn`, `TxnHandle`, `ConfigAccess::begin_apply_txn`. Single responsibility: open/execute/commit/discard a write transaction over either backend.
- `crates/defra-agent-cli/tests/cli_config_apply_transactional_rollback.rs` — SIGKILL-mid-apply integration test against a real DefraDB node.

**Modified:**
- `crates/defra-agent-protocol/src/graphql.rs` — add `execute_graphql_async_with_tx` that mirrors `execute_graphql_async` but adds an optional `x-defradb-tx` header.
- `crates/defra-agent-cli/src/graphql_access.rs` — re-export `post_graphql_with_tx` thin wrapper for the txn helper.
- `crates/defra-agent-cli/src/config_writes/mod.rs` — declare `txn` submodule and re-export `ConfigApplyTxn`.
- `crates/defra-agent-cli/src/config_writes/common.rs` — `query_documents_by_unique_value(access: &ConfigAccess, ...)` → `(txn: &ConfigApplyTxn, ...)`.
- `crates/defra-agent-cli/src/config_writes/task.rs` — `write_task_document` signature change.
- `crates/defra-agent-cli/src/config_writes/schedule.rs` — `write_schedule_document` signature change.
- `crates/defra-agent-cli/src/config_writes/event_trigger.rs` — `write_event_trigger_document` signature change.
- `crates/defra-agent-cli/src/config_import.rs` — thread `&ConfigApplyTxn` through `apply_desired_state_changes` and its callees; extend `lean_apply_write_boundary_tests::RecordingGraphqlState` with tx-aware routes; extend conformance assertions.
- `crates/defra-agent-cli/src/commands/config/apply.rs` — wrap the apply call in the `begin → commit | discard` braid.
- `CHANGELOG.md` — operator-facing release note.

---

## Task 0: Baseline

**Files:** none.

- [ ] **Step 0.1: Verify baseline green**

Run:
```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
cargo test -p defra-agent-cli --tests lean_apply_write_boundary_tests::generated_apply_reconcile_cases_fence_production_apply_write_boundary
```
Expected: all green. If not, stop and report — the baseline must be clean before starting.

- [ ] **Step 0.2: Confirm working branch**

Run: `git branch --show-current`
Expected output: `design/issue-56-transactional-apply`. Do not switch branches. Do not merge.

---

## Task 1: Extend the recording GraphQL test server with tx awareness

The conformance test's recording server (in `crates/defra-agent-cli/src/config_import.rs`, module `lean_apply_write_boundary_tests`) currently has one route (`/`) and one mutation log. Add the tx routes, per-tx state windows, a committed state window, and a fail-at-write-N injection knob. The regex parser stays — header routing is at the HTTP layer.

**Files:**
- Modify: `crates/defra-agent-cli/src/config_import.rs` (the `lean_apply_write_boundary_tests` module starting at line ~789).

- [ ] **Step 1.1: Sketch the new recorder shape — write a failing unit test for `begin → write → commit` returning a numeric id and appending to committed state**

Append to the `lean_apply_write_boundary_tests` module:

```rust
#[cfg(test)]
mod recorder_unit_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recorder_begin_returns_numeric_id_and_commit_appends_to_committed() {
        let (graphql, recorder) = start_recording_graphql().await;
        let client = reqwest::Client::new();

        let begin = client
            .post(format!("{graphql}api/v0/tx/begin"))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        let txn_id = begin.get("id").and_then(serde_json::Value::as_str).unwrap().to_string();
        assert!(txn_id.parse::<u64>().is_ok(), "tx id must be numeric");

        let _write = client
            .post(format!("{graphql}"))
            .header("x-defradb-tx", &txn_id)
            .json(&json!({
                "query": "mutation { doc_0: create_Task(input: { task_id: \"task-a\" }) { _docID } }",
            }))
            .send()
            .await
            .unwrap();

        // Before commit, committed window is empty.
        assert!(recorder.committed_state().is_empty());

        let commit = client
            .post(format!("{graphql}api/v0/tx/{txn_id}"))
            .send()
            .await
            .unwrap();
        assert!(commit.status().is_success());

        let committed = recorder.committed_state();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].collection, Collection::Task);
        assert_eq!(committed[0].unique_value, "task-a");
    }
}
```

- [ ] **Step 1.2: Run the unit test to verify it fails**

Run:
```bash
cargo test -p defra-agent-cli --lib lean_apply_write_boundary_tests::recorder_unit_tests::recorder_begin_returns_numeric_id_and_commit_appends_to_committed -- --nocapture
```
Expected: FAIL — `committed_state` method does not exist; tx route does not exist.

- [ ] **Step 1.3: Add the tx-aware state to `RecordingGraphqlState` and the routes**

Replace the existing `RecordingGraphqlState` and `recording_graphql_handler`/`start_recording_graphql` with this extended version. The committed window replaces the previous `writes` field; existing call sites that read `recorder.writes` are renamed in Step 1.4.

```rust
#[derive(Clone, Default)]
struct RecordingGraphqlState {
    queries: Arc<Mutex<Vec<String>>>,
    // Per-tx pending writes, keyed by numeric tx id.
    transactions: Arc<Mutex<BTreeMap<String, Vec<ObservedWrite>>>>,
    // Writes that have committed (either via tx commit or via direct graphql POST without a tx header).
    committed: Arc<Mutex<Vec<ObservedWrite>>>,
    next_tx_id: Arc<AtomicU64>,
    // Fail injection: if Some, the (write_index_within_tx + 1)th mutation against the named tx
    // returns a GraphQL error.
    fail_injection: Arc<Mutex<Option<FailInjection>>>,
    // Lifecycle counters for assertions.
    tx_begin_count: Arc<AtomicU64>,
    tx_commit_count: Arc<AtomicU64>,
    tx_discard_count: Arc<AtomicU64>,
}

#[derive(Debug, Clone)]
struct FailInjection {
    tx_id: String,
    write_index: usize, // zero-based; matches the i-th mutation within that tx
}

impl RecordingGraphqlState {
    fn committed_state(&self) -> Vec<ObservedWrite> {
        self.committed.lock().expect("committed lock").clone()
    }

    fn observed_writes(&self) -> Vec<ObservedWrite> {
        // Existing test reads this as "every write attempted".
        // For convenience, return committed + any still-pending in-tx writes.
        let mut all = self.committed.lock().expect("committed lock").clone();
        let txs = self.transactions.lock().expect("tx lock").clone();
        for (_id, writes) in txs.iter() {
            all.extend(writes.iter().cloned());
        }
        all
    }

    fn tx_lifecycle_counts(&self) -> (u64, u64, u64) {
        (
            self.tx_begin_count.load(Ordering::SeqCst),
            self.tx_commit_count.load(Ordering::SeqCst),
            self.tx_discard_count.load(Ordering::SeqCst),
        )
    }

    fn install_fail_at(&self, tx_id: impl Into<String>, write_index: usize) {
        *self.fail_injection.lock().expect("fail lock") = Some(FailInjection {
            tx_id: tx_id.into(),
            write_index,
        });
    }
}

async fn start_recording_graphql() -> (String, RecordingGraphqlState) {
    let state = RecordingGraphqlState::default();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind recording GraphQL listener");
    let addr = listener.local_addr().expect("recording GraphQL addr");
    let app = Router::new()
        .route("/", post(recording_graphql_handler))
        .route("/api/v0/tx/begin", post(recording_tx_begin_handler))
        .route("/api/v0/tx/:id", post(recording_tx_commit_handler))
        .route("/api/v0/tx/:id", axum::routing::delete(recording_tx_discard_handler))
        .with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("recording GraphQL server");
    });
    (format!("http://{addr}/"), state)
}

async fn recording_tx_begin_handler(
    State(state): State<RecordingGraphqlState>,
) -> Json<Value> {
    let id = state.next_tx_id.fetch_add(1, Ordering::SeqCst);
    state
        .transactions
        .lock()
        .expect("tx lock")
        .insert(id.to_string(), Vec::new());
    state.tx_begin_count.fetch_add(1, Ordering::SeqCst);
    Json(json!({ "id": id.to_string() }))
}

async fn recording_tx_commit_handler(
    State(state): State<RecordingGraphqlState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::http::StatusCode {
    let mut transactions = state.transactions.lock().expect("tx lock");
    let Some(writes) = transactions.remove(&id) else {
        return axum::http::StatusCode::NOT_FOUND;
    };
    drop(transactions);
    state.committed.lock().expect("committed lock").extend(writes);
    state.tx_commit_count.fetch_add(1, Ordering::SeqCst);
    axum::http::StatusCode::OK
}

async fn recording_tx_discard_handler(
    State(state): State<RecordingGraphqlState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::http::StatusCode {
    let removed = state
        .transactions
        .lock()
        .expect("tx lock")
        .remove(&id)
        .is_some();
    if removed {
        state.tx_discard_count.fetch_add(1, Ordering::SeqCst);
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::NOT_FOUND
    }
}

async fn recording_graphql_handler(
    State(state): State<RecordingGraphqlState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let query = body
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    state.queries.lock().expect("queries lock").push(query.clone());

    let tx_id = headers
        .get("x-defradb-tx")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if query.contains("mutation") {
        let writes = parse_mutation_writes(&query);

        // Fail injection: if installed, check whether this batch crosses the failing index.
        if let Some(fail) = state.fail_injection.lock().expect("fail lock").clone() {
            if tx_id.as_deref() == Some(fail.tx_id.as_str()) {
                let prior = state
                    .transactions
                    .lock()
                    .expect("tx lock")
                    .get(&fail.tx_id)
                    .map(|v| v.len())
                    .unwrap_or(0);
                if (prior..prior + writes.len()).contains(&fail.write_index) {
                    return Json(json!({
                        "errors": [{ "message": "injected failure at recorder" }]
                    }));
                }
            }
        }

        match tx_id {
            Some(id) => {
                let mut transactions = state.transactions.lock().expect("tx lock");
                let entry = transactions.entry(id).or_default();
                entry.extend(writes);
            }
            None => {
                state.committed.lock().expect("committed lock").extend(writes);
            }
        }
        Json(json!({ "data": aliased_mutation_response(&query) }))
    } else {
        Json(json!({ "data": empty_collection_query_response(&query) }))
    }
}
```

Add the required imports at the top of `lean_apply_write_boundary_tests`:

```rust
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
```

- [ ] **Step 1.4: Update existing assertions that read `recorder.writes` to use `recorder.observed_writes()` and `recorder.committed_state()` as appropriate**

In `generated_apply_reconcile_cases_fence_production_apply_write_boundary`, find this block (around line 862):
```rust
let observed = recorder.writes.lock().expect("writes lock").clone();
```
Replace with:
```rust
let observed = recorder.observed_writes();
```

Find `assert_observed_prefixes_are_referrer_closed(case, &observed);` — no change needed beyond the binding rename above.

Find `assert_live_payloads_not_written(case, &recorder);` — body of that helper reads `recorder.queries`, no change.

- [ ] **Step 1.5: Run the new unit test to verify it passes**

Run:
```bash
cargo test -p defra-agent-cli --lib lean_apply_write_boundary_tests::recorder_unit_tests::recorder_begin_returns_numeric_id_and_commit_appends_to_committed -- --nocapture
```
Expected: PASS.

- [ ] **Step 1.6: Verify the existing conformance test still passes (recording-server refactor preserves behavior)**

Run:
```bash
cargo test -p defra-agent-cli --lib lean_apply_write_boundary_tests::generated_apply_reconcile_cases_fence_production_apply_write_boundary -- --nocapture
```
Expected: PASS. The existing test path goes through the no-header branch, which the new recorder routes into `committed` directly — observationally identical to the old behavior.

- [ ] **Step 1.7: Add a unit test for discard-not-committed**

Append inside the `recorder_unit_tests` module:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recorder_discard_drops_pending_writes() {
    let (graphql, recorder) = start_recording_graphql().await;
    let client = reqwest::Client::new();

    let begin = client
        .post(format!("{graphql}api/v0/tx/begin"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let txn_id = begin.get("id").and_then(serde_json::Value::as_str).unwrap().to_string();

    let _write = client
        .post(format!("{graphql}"))
        .header("x-defradb-tx", &txn_id)
        .json(&serde_json::json!({
            "query": "mutation { doc_0: create_Task(input: { task_id: \"task-a\" }) { _docID } }",
        }))
        .send()
        .await
        .unwrap();

    let discard = client
        .delete(format!("{graphql}api/v0/tx/{txn_id}"))
        .send()
        .await
        .unwrap();
    assert!(discard.status().is_success());

    assert!(
        recorder.committed_state().is_empty(),
        "discarded tx must not contribute to committed state"
    );
    let (begin_count, commit_count, discard_count) = recorder.tx_lifecycle_counts();
    assert_eq!((begin_count, commit_count, discard_count), (1, 0, 1));
}
```

- [ ] **Step 1.8: Add a unit test for fail-at-write-N injection**

Append:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recorder_fail_injection_aborts_at_target_index() {
    let (graphql, recorder) = start_recording_graphql().await;
    let client = reqwest::Client::new();

    let begin = client
        .post(format!("{graphql}api/v0/tx/begin"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let txn_id = begin.get("id").and_then(serde_json::Value::as_str).unwrap().to_string();
    recorder.install_fail_at(&txn_id, 1); // fail the SECOND mutation (zero-based)

    let ok = client
        .post(format!("{graphql}"))
        .header("x-defradb-tx", &txn_id)
        .json(&serde_json::json!({
            "query": "mutation { doc_0: create_Task(input: { task_id: \"task-a\" }) { _docID } }",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(ok.get("errors").is_none(), "first mutation should succeed");

    let fail = client
        .post(format!("{graphql}"))
        .header("x-defradb-tx", &txn_id)
        .json(&serde_json::json!({
            "query": "mutation { doc_0: create_Task(input: { task_id: \"task-b\" }) { _docID } }",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(fail.get("errors").is_some(), "second mutation should fail");
}
```

- [ ] **Step 1.9: Run all recorder unit tests**

Run:
```bash
cargo test -p defra-agent-cli --lib lean_apply_write_boundary_tests::recorder_unit_tests -- --nocapture
```
Expected: all PASS.

- [ ] **Step 1.10: Commit**

```bash
git add crates/defra-agent-cli/src/config_import.rs
git commit -m "$(cat <<'EOF'
Extend test recording GraphQL server with tx awareness for #56

Add per-tx state windows, /api/v0/tx/{begin,commit,discard} routes,
x-defradb-tx header routing, and a fail-at-write-N injection knob.
Existing conformance assertions still pass — direct-GraphQL writes
(no header) flow into the same committed window as before.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Add `execute_graphql_async_with_tx` to the protocol crate

**Files:**
- Modify: `crates/defra-agent-protocol/src/graphql.rs:222-317`.

- [ ] **Step 2.1: Write a failing test for the new function**

Append to `crates/defra-agent-protocol/src/graphql.rs` (inside an existing `#[cfg(test)] mod tests { ... }` block; if no `tests` mod exists, add one at the bottom of the file):

```rust
#[cfg(test)]
mod tx_tests {
    use super::*;
    use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    #[derive(Clone, Default)]
    struct HeaderRecorder {
        last_tx_header: Arc<Mutex<Option<String>>>,
    }

    async fn capture_handler(
        State(state): State<HeaderRecorder>,
        headers: HeaderMap,
        Json(_body): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        *state.last_tx_header.lock().unwrap() = headers
            .get("x-defradb-tx")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        Json(serde_json::json!({ "data": {} }))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execute_graphql_async_with_tx_sets_header_when_id_provided() {
        let state = HeaderRecorder::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", post(capture_handler))
            .with_state(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let endpoint = format!("http://{addr}/");
        let options = GraphqlRequestOptions {
            timeout: std::time::Duration::from_secs(2),
            max_attempts: 1,
            retry_backoff: std::time::Duration::from_millis(50),
        };

        execute_graphql_async_with_tx(&endpoint, "{ __typename }", options.clone(), Some("42"))
            .await
            .unwrap();
        assert_eq!(state.last_tx_header.lock().unwrap().as_deref(), Some("42"));

        execute_graphql_async_with_tx(&endpoint, "{ __typename }", options, None)
            .await
            .unwrap();
        assert_eq!(state.last_tx_header.lock().unwrap().as_deref(), None);
    }
}
```

- [ ] **Step 2.2: Run to verify it fails**

Run:
```bash
cargo test -p defra-agent-protocol graphql::tx_tests -- --nocapture
```
Expected: FAIL — `execute_graphql_async_with_tx` is not defined.

- [ ] **Step 2.3: Implement `execute_graphql_async_with_tx`**

Add this function next to `execute_graphql_async` in `crates/defra-agent-protocol/src/graphql.rs` (right after the existing function ends at line 317):

```rust
/// Like `execute_graphql_async` but adds an `x-defradb-tx` header when
/// `txn_id` is `Some`. Used by `defra-agent-cli` to drive DefraDB HTTP
/// transactions during `config apply`.
pub async fn execute_graphql_async_with_tx(
    graphql: &str,
    query: &str,
    options: GraphqlRequestOptions,
    txn_id: Option<&str>,
) -> Result<serde_json::Value> {
    let client = reqwest::Client::builder()
        .timeout(options.timeout)
        .build()?;
    let mut last_error = None;

    for attempt in 0..options.max_attempts.max(1) {
        let mut request = client
            .post(graphql)
            .json(&serde_json::json!({ "query": query }));
        if let Some(id) = txn_id {
            request = request.header("x-defradb-tx", id);
        }
        let response = request.send().await;
        let response = match response {
            Ok(response) => response,
            Err(error)
                if graphql_transport_error_is_retryable(&error)
                    && attempt + 1 < options.max_attempts =>
            {
                tracing::warn!(
                    attempt,
                    graphql,
                    error = %error,
                    "retrying async GraphQL tx request after transport error"
                );
                last_error = Some(
                    anyhow::Error::new(error).context(format!("posting GraphQL to {graphql}")),
                );
                tokio::time::sleep(scale_backoff(options.retry_backoff, attempt)).await;
                continue;
            }
            Err(error) => {
                return Err(
                    anyhow::Error::new(error).context(format!("posting GraphQL to {graphql}"))
                );
            }
        };

        let response = match response.error_for_status() {
            Ok(response) => response,
            Err(error)
                if graphql_transport_error_is_retryable(&error)
                    && attempt + 1 < options.max_attempts =>
            {
                last_error = Some(
                    anyhow::Error::new(error)
                        .context(format!("reading GraphQL response from {graphql}")),
                );
                tokio::time::sleep(scale_backoff(options.retry_backoff, attempt)).await;
                continue;
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("reading GraphQL response from {graphql}")));
            }
        };

        let value = match response.json().await {
            Ok(value) => value,
            Err(error)
                if graphql_transport_error_is_retryable(&error)
                    && attempt + 1 < options.max_attempts =>
            {
                last_error = Some(
                    anyhow::Error::new(error)
                        .context(format!("decoding GraphQL response body from {graphql}")),
                );
                tokio::time::sleep(scale_backoff(options.retry_backoff, attempt)).await;
                continue;
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("decoding GraphQL response body from {graphql}")));
            }
        };

        return finish_graphql_response(graphql, value);
    }

    Err(last_error
        .unwrap_or_else(|| anyhow::anyhow!("GraphQL request retries exhausted for {graphql}")))
}
```

- [ ] **Step 2.4: Run to verify it passes**

Run:
```bash
cargo test -p defra-agent-protocol graphql::tx_tests -- --nocapture
```
Expected: PASS.

- [ ] **Step 2.5: Format and check**

```bash
cargo fmt --all
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
```
Expected: no errors.

- [ ] **Step 2.6: Commit**

```bash
git add crates/defra-agent-protocol/src/graphql.rs
git commit -m "$(cat <<'EOF'
Add execute_graphql_async_with_tx for DefraDB HTTP transactions

Mirrors execute_graphql_async with optional x-defradb-tx header. Used
by defra-agent-cli to drive transactional config apply (#56).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Add `ConfigApplyTxn` and `ConfigAccess::begin_apply_txn`

**Files:**
- Create: `crates/defra-agent-cli/src/config_writes/txn.rs`.
- Modify: `crates/defra-agent-cli/src/config_writes/mod.rs`.
- Modify: `crates/defra-agent-cli/src/graphql_access.rs` — thin re-export wrapper for `post_graphql_with_tx`.

- [ ] **Step 3.1: Create the new module with the type definitions**

Create `crates/defra-agent-cli/src/config_writes/txn.rs`:

```rust
//! Open-write transaction wrapper around `ConfigAccess`.
//!
//! `ConfigApplyTxn` is the only access type passed through the apply pipeline
//! once `config apply` has begun a transaction. The top-level orchestrator
//! drives `begin_apply_txn` → `apply_desired_state_changes` → `commit` (on
//! success) or `discard` (on error).
//!
//! Discard semantics differ between backends:
//! - **Embedded.** `runner.rollback_txn` returns `TransactionError` only in
//!   pathological cases (handle already finalized, lock poisoned). The
//!   underlying `db_txn` is dropped in any case.
//! - **HTTP.** `DELETE /api/v0/tx/{id}` is a network call; it can fail for
//!   reasons unrelated to the apply error. Even if the DELETE never reaches
//!   the server, DefraDB's tx GC will reclaim the handle on its own.
//!
//! Both return `Result<()>` so callers can log discrepancies, but neither
//! changes operator-facing behavior on failure: the apply error is what
//! surfaces, and the DB ends at the pre-apply snapshot.

use anyhow::{Context, Result};
use defra_agent_protocol::graphql::{execute_graphql_async_with_tx, GraphqlRequestOptions};
use serde_json::{json, Value};

use crate::config_writes::ConfigAccess;
use crate::graphql_access::graphql_diagnostic_hint;

#[derive(Debug)]
pub(crate) enum TxnHandle {
    /// Numeric txn id parsed from `POST /api/v0/tx/begin`.
    Graphql(String),
    /// Embedded transaction handle returned by `runner.begin_txn(false)`.
    Local(defra_agent::defra_node::TransactionHandle),
}

pub(crate) struct ConfigApplyTxn<'a> {
    access: &'a ConfigAccess,
    handle: TxnHandle,
}

impl<'a> ConfigApplyTxn<'a> {
    pub(crate) fn new(access: &'a ConfigAccess, handle: TxnHandle) -> Self {
        Self { access, handle }
    }

    pub(crate) fn mode(&self) -> &'static str {
        self.access.mode()
    }

    /// Execute a GraphQL query within this transaction.
    pub(crate) async fn execute(&self, query: &str) -> Result<Value> {
        match (&self.access, &self.handle) {
            (ConfigAccess::Graphql(endpoint), TxnHandle::Graphql(id)) => {
                execute_graphql_async_with_tx(
                    endpoint,
                    query,
                    GraphqlRequestOptions {
                        timeout: std::time::Duration::from_secs(30),
                        max_attempts: 5,
                        retry_backoff: std::time::Duration::from_millis(100),
                    },
                    Some(id),
                )
                .await
                .map_err(|error| anyhow::anyhow!("{error}\n{}", graphql_diagnostic_hint(endpoint)))
            }
            (ConfigAccess::Local(node), TxnHandle::Local(handle)) => {
                let request = defra_agent::defra_node::QueryRequest {
                    query: query.to_string(),
                    operation_name: None,
                    variables: None,
                    identity: None,
                };
                let response = node.runner().execute_in_txn(request, handle).await;
                if response.has_errors() {
                    anyhow::bail!("graphql returned errors: {:?}", response.errors);
                }
                Ok(json!({ "data": response.data.unwrap_or(Value::Null) }))
            }
            _ => anyhow::bail!("ConfigApplyTxn backend/handle mismatch (internal bug)"),
        }
    }

    /// Commit the transaction. Apply is durable after this returns Ok.
    pub(crate) async fn commit(self) -> Result<()> {
        match (self.access, self.handle) {
            (ConfigAccess::Graphql(endpoint), TxnHandle::Graphql(id)) => {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()?;
                let response = client
                    .post(format!("{endpoint}api/v0/tx/{id}"))
                    .send()
                    .await
                    .with_context(|| format!("posting tx commit to {endpoint}"))?;
                if !response.status().is_success() {
                    anyhow::bail!(
                        "tx commit returned HTTP {} from {endpoint}",
                        response.status()
                    );
                }
                Ok(())
            }
            (ConfigAccess::Local(node), TxnHandle::Local(handle)) => node
                .runner()
                .commit_txn(&handle)
                .await
                .map_err(|error| anyhow::anyhow!("commit_txn: {error}")),
            _ => anyhow::bail!("ConfigApplyTxn backend/handle mismatch on commit (internal bug)"),
        }
    }

    /// Discard the transaction. Returns the underlying error if the explicit
    /// round-trip fails; callers are expected to log and swallow that error so
    /// the original apply error remains what surfaces to the operator.
    pub(crate) async fn discard(self) -> Result<()> {
        match (self.access, self.handle) {
            (ConfigAccess::Graphql(endpoint), TxnHandle::Graphql(id)) => {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()?;
                let response = client
                    .delete(format!("{endpoint}api/v0/tx/{id}"))
                    .send()
                    .await
                    .with_context(|| format!("posting tx discard to {endpoint}"))?;
                if !response.status().is_success() {
                    anyhow::bail!(
                        "tx discard returned HTTP {} from {endpoint}",
                        response.status()
                    );
                }
                Ok(())
            }
            (ConfigAccess::Local(node), TxnHandle::Local(handle)) => node
                .runner()
                .rollback_txn(&handle)
                .await
                .map_err(|error| anyhow::anyhow!("rollback_txn: {error}")),
            _ => anyhow::bail!("ConfigApplyTxn backend/handle mismatch on discard (internal bug)"),
        }
    }
}

impl ConfigAccess {
    /// Begin a write transaction on the underlying backend.
    pub(crate) async fn begin_apply_txn(&self) -> Result<ConfigApplyTxn<'_>> {
        match self {
            ConfigAccess::Graphql(endpoint) => {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()?;
                let response = client
                    .post(format!("{endpoint}api/v0/tx/begin"))
                    .send()
                    .await
                    .with_context(|| format!("posting tx begin to {endpoint}"))?;
                if !response.status().is_success() {
                    anyhow::bail!(
                        "tx begin returned HTTP {} from {endpoint}",
                        response.status()
                    );
                }
                let body: Value = response
                    .json()
                    .await
                    .with_context(|| format!("decoding tx begin body from {endpoint}"))?;
                let id = body
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("tx begin missing id: {body}"))?
                    .to_string();
                Ok(ConfigApplyTxn::new(self, TxnHandle::Graphql(id)))
            }
            ConfigAccess::Local(node) => {
                let handle = node
                    .runner()
                    .begin_txn(false)
                    .await
                    .map_err(|error| anyhow::anyhow!("begin_txn: {error}"))?;
                Ok(ConfigApplyTxn::new(self, TxnHandle::Local(handle)))
            }
        }
    }
}
```

- [ ] **Step 3.2: Wire the module in `config_writes/mod.rs`**

Modify `crates/defra-agent-cli/src/config_writes/mod.rs`. Add the new module declaration and re-exports near the top, after the other `mod` lines:

```rust
mod agent_behavior;
mod common;
mod event_trigger;
mod inference_backend;
mod schedule;
mod task;
mod tool_selection;
mod txn;  // NEW

pub(crate) use agent_behavior::write_agent_behavior_document;
pub(crate) use event_trigger::write_event_trigger_document;
pub(crate) use inference_backend::{
    write_inference_backend_document, InferenceBackendUpsertDocument,
};
pub(crate) use schedule::write_schedule_document;
pub(crate) use task::write_task_document;
pub(crate) use tool_selection::write_tool_selection_document;
pub(crate) use txn::{ConfigApplyTxn, TxnHandle};  // NEW
```

- [ ] **Step 3.3: Verify the `EmbeddedNode::runner()` accessor exists, or add it if not**

Run:
```bash
grep -n "pub fn runner\|pub.*runner" /Users/johnzampolin/.cargo/git/checkouts/defradb.rs-4ab0524bccc74f29/25b935b/crates/defra-node/src/lib.rs | head -5
```
Expected: `pub fn runner(&self) -> &Arc<dyn QueryExecutor>` or equivalent. If missing — the `EmbeddedNode` struct exposes `runner` only through the existing `execute` method — fall back to using `node.execute(...)` for the read-committed path and adding a sibling `EmbeddedNode::begin_txn`/`commit_txn`/`rollback_txn`/`execute_in_txn` set of wrappers locally in `defra-agent` (in `crates/defra-agent/src/lib.rs`). Pause and report if the accessor needs adding — that is a small but non-trivial sub-task.

- [ ] **Step 3.4: Write a round-trip test for `ConfigApplyTxn` against the recording server**

This test lives in the existing `lean_apply_write_boundary_tests` module so it can reuse `start_recording_graphql`. Append to that module:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_txn_round_trip_against_recorder() {
    let (graphql, recorder) = start_recording_graphql().await;
    let access = ConfigAccess::Graphql(graphql);
    let txn = access.begin_apply_txn().await.expect("begin");

    let _ = txn
        .execute("mutation { doc_0: create_Task(input: { task_id: \"task-a\" }) { _docID } }")
        .await
        .expect("execute in tx");

    // Before commit, committed window must be empty.
    assert!(recorder.committed_state().is_empty());

    txn.commit().await.expect("commit");

    let committed = recorder.committed_state();
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].unique_value, "task-a");
    let (begin_count, commit_count, discard_count) = recorder.tx_lifecycle_counts();
    assert_eq!((begin_count, commit_count, discard_count), (1, 1, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_txn_discard_leaves_committed_empty() {
    let (graphql, recorder) = start_recording_graphql().await;
    let access = ConfigAccess::Graphql(graphql);
    let txn = access.begin_apply_txn().await.expect("begin");

    let _ = txn
        .execute("mutation { doc_0: create_Task(input: { task_id: \"task-a\" }) { _docID } }")
        .await
        .expect("execute in tx");

    txn.discard().await.expect("discard");

    assert!(recorder.committed_state().is_empty());
    let (begin_count, commit_count, discard_count) = recorder.tx_lifecycle_counts();
    assert_eq!((begin_count, commit_count, discard_count), (1, 0, 1));
}
```

Also add `use crate::config_writes::ConfigApplyTxn;` near the top of the `lean_apply_write_boundary_tests` module imports if needed (the helper resolves it via `super::*` and `super::lean_vocab_test::...`, but `ConfigApplyTxn` is brought in via `super::*` since the lean test mod is inside `config_import.rs` which imports from `config_writes`).

- [ ] **Step 3.5: Run the round-trip tests**

```bash
cargo test -p defra-agent-cli --lib lean_apply_write_boundary_tests::config_apply_txn_round_trip_against_recorder lean_apply_write_boundary_tests::config_apply_txn_discard_leaves_committed_empty -- --nocapture
```
Expected: PASS.

- [ ] **Step 3.6: Format and check**

```bash
cargo fmt --all
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
```
Expected: no errors.

- [ ] **Step 3.7: Commit**

```bash
git add crates/defra-agent-cli/src/config_writes/
git commit -m "$(cat <<'EOF'
Add ConfigApplyTxn for transactional config apply (#56)

New module wraps ConfigAccess with a DefraDB transaction handle and
exposes execute / commit / discard. Begins a tx on POST /api/v0/tx/begin
(HTTP) or runner.begin_txn (embedded), threads through every write,
commits on success or discards on error.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4a: Thread `&ConfigApplyTxn` through `config_writes/`

Mechanical signature change. The compiler is the source of truth for what to touch.

**Files:**
- Modify: `crates/defra-agent-cli/src/config_writes/common.rs:7`.
- Modify: `crates/defra-agent-cli/src/config_writes/task.rs:17-22`.
- Modify: `crates/defra-agent-cli/src/config_writes/schedule.rs:23` (use `grep -n "pub.*fn write_schedule_document" crates/defra-agent-cli/src/config_writes/schedule.rs` if line drifted).
- Modify: `crates/defra-agent-cli/src/config_writes/event_trigger.rs:24`.

- [ ] **Step 4a.1: Change `query_documents_by_unique_value` signature in `common.rs`**

In `crates/defra-agent-cli/src/config_writes/common.rs`, find:
```rust
pub(super) async fn query_documents_by_unique_value(
    access: &ConfigAccess,
    ...
```
Replace with:
```rust
pub(super) async fn query_documents_by_unique_value(
    txn: &super::ConfigApplyTxn<'_>,
    ...
```
Then inside the body, replace `access.execute(&query).await` with `txn.execute(&query).await`. Remove the unused `use crate::config_writes::ConfigAccess;` if it becomes orphan; rely on the compiler.

- [ ] **Step 4a.2: Change `write_task_document` signature in `task.rs`**

In `crates/defra-agent-cli/src/config_writes/task.rs`:
```rust
pub(crate) async fn write_task_document(
    txn: &super::ConfigApplyTxn<'_>,
    task_id: &str,
    add_doc: &Value,
    update_doc: &Value,
) -> Result<String> {
```
Inside, replace every `access.execute(...)` with `txn.execute(...)` and every `query_documents_by_unique_value(access, ...)` with `query_documents_by_unique_value(txn, ...)`. Update `select_matching_task_row(access, ...)` etc. — the function in this same file — recursively in the same step (if it takes `access: &ConfigAccess`, change to `txn: &super::ConfigApplyTxn<'_>` and update its calls likewise).

- [ ] **Step 4a.3: Same change for `schedule.rs`**

`write_schedule_document(access: &ConfigAccess, ...)` → `write_schedule_document(txn: &super::ConfigApplyTxn<'_>, ...)`. Cascade `access.execute` → `txn.execute`. Cascade local helpers.

- [ ] **Step 4a.4: Same change for `event_trigger.rs`**

`write_event_trigger_document(access: &ConfigAccess, ...)` → `write_event_trigger_document(txn: &super::ConfigApplyTxn<'_>, ...)`. Cascade as above.

- [ ] **Step 4a.5: `cargo check` to see what's still broken**

```bash
cargo check -p defra-agent-cli --tests
```
Expected: errors in `config_import.rs` and possibly `commands/config/apply.rs` (still passing `&ConfigAccess` to the renamed-signature functions). These are handled in Tasks 4b and 5.

- [ ] **Step 4a.6: (No commit yet — wait until 4b compiles)**

Leave the working tree dirty for Task 4b.

---

## Task 4b: Thread `&ConfigApplyTxn` through `config_import.rs`

**Files:**
- Modify: `crates/defra-agent-cli/src/config_import.rs:110-617`.

- [ ] **Step 4b.1: Switch `apply_desired_state_changes` signature**

Find (line ~595):
```rust
pub(crate) async fn apply_desired_state_changes(
    access: &ConfigAccess,
    desired_bundle: &DesiredApplyBundle,
    planned: &desired_state::DesiredStateDiffReport,
) -> Result<ConfigApplyCounts> {
```
Replace with:
```rust
pub(crate) async fn apply_desired_state_changes(
    txn: &ConfigApplyTxn<'_>,
    desired_bundle: &DesiredApplyBundle,
    planned: &desired_state::DesiredStateDiffReport,
) -> Result<ConfigApplyCounts> {
```
And update the `apply_import_collection(access, ...)` call inside to `apply_import_collection(txn, ...)`.

- [ ] **Step 4b.2: Switch `apply_import_collection` signature**

Find (line ~110):
```rust
pub(crate) async fn apply_import_collection(
    access: &ConfigAccess,
    collection_name: &str,
    unique_field: &str,
    docs: &[Value],
    override_existing: bool,
) -> Result<usize> {
```
Replace `access: &ConfigAccess` with `txn: &ConfigApplyTxn<'_>`. Cascade body: `apply_custom_override_collection_batched(access, ...)` → `(txn, ...)`; `apply_generic_import_collection_batched(access, ...)` → `(txn, ...)`.

- [ ] **Step 4b.3: Switch `apply_generic_import_collection_batched` (line ~181), `apply_generic_import_document` (line ~255), `apply_custom_override_collection_batched` (line ~301), `query_existing_documents_by_unique_values` (line ~354), `apply_custom_override_documents_individually` (line ~479), `execute_aliased_mutation_batches` (line ~519)**

Each: change `access: &ConfigAccess` to `txn: &ConfigApplyTxn<'_>`, cascade body, change `access.execute` to `txn.execute`. For `apply_custom_override_documents_individually`, change the calls to `write_task_document(access, ...)` → `write_task_document(txn, ...)`, same for schedule and event_trigger.

- [ ] **Step 4b.4: Update the import**

At the top of `config_import.rs`, replace:
```rust
use crate::config_writes::{
    write_event_trigger_document, write_schedule_document, write_task_document, ConfigAccess,
    ExistingDocumentRef,
};
```
with:
```rust
use crate::config_writes::{
    write_event_trigger_document, write_schedule_document, write_task_document, ConfigAccess,
    ConfigApplyTxn, ExistingDocumentRef,
};
```
Keep `ConfigAccess` — `select_apply_principal_docs` and other helpers that don't touch the write path still take `&ConfigAccess` for read queries.

Verify by grep: `select_apply_principal_docs` does NOT call `execute`, so it stays on `&ConfigAccess` (a `Value` accessor).

- [ ] **Step 4b.5: Update the lean_apply_write_boundary_tests test body**

The existing call inside `generated_apply_reconcile_cases_fence_production_apply_write_boundary` was:
```rust
let counts = apply_desired_state_changes(
    &ConfigAccess::Graphql(graphql),
    &desired_bundle,
    &planned,
)
.await
```
Replace with:
```rust
let access = ConfigAccess::Graphql(graphql);
let txn = access.begin_apply_txn().await.expect("begin apply tx");
let counts = match apply_desired_state_changes(&txn, &desired_bundle, &planned).await {
    Ok(counts) => {
        txn.commit().await.expect("commit");
        counts
    }
    Err(error) => {
        let _ = txn.discard().await;
        panic!(
            "production apply_desired_state_changes failed for Lean case {}: {error}",
            case.name
        );
    }
};
```

- [ ] **Step 4b.6: `cargo check` to find remaining call sites**

```bash
cargo check -p defra-agent-cli --tests
```
Expected: maybe one remaining error in `commands/config/apply.rs:70` — handled in Task 5. Otherwise no errors.

- [ ] **Step 4b.7: Run existing apply-related tests (production conformance + property + e2e basics)**

```bash
cargo test -p defra-agent-cli --lib lean_apply_write_boundary_tests -- --nocapture
```
Expected: PASS — the existing assertions hold because the recorder treats begin/commit transparently (writes flow into committed window after commit).

- [ ] **Step 4b.8: Commit Task 4a + 4b together**

```bash
git add crates/defra-agent-cli/src/config_writes/ crates/defra-agent-cli/src/config_import.rs
git commit -m "$(cat <<'EOF'
Thread ConfigApplyTxn through config_apply write path (#56)

Switch the apply pipeline and the three custom collection writers from
&ConfigAccess to &ConfigApplyTxn<'_>. Production write_*_document and
existence probes now read through the same transaction as the writes,
so create-vs-update branch decisions see in-tx state instead of stale
committed state.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Orchestrator braid in `commands/config/apply.rs`

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/config/apply.rs:70`.

- [ ] **Step 5.1: Replace the apply call with the braid**

Find (line ~70):
```rust
    let applied = apply_desired_state_changes(&access, &desired_bundle, &planned).await?;
```
Replace with:
```rust
    let applied = {
        let txn = access
            .begin_apply_txn()
            .await
            .context("config apply: begin transaction")?;
        let counts = match apply_desired_state_changes(&txn, &desired_bundle, &planned).await {
            Ok(counts) => counts,
            Err(error) => {
                if let Err(discard_err) = txn.discard().await {
                    tracing::warn!(
                        %discard_err,
                        "config apply: tx discard failed after apply error"
                    );
                }
                return Err(error);
            }
        };
        if let Err(commit_err) = txn.commit().await {
            return Err(commit_err).context("config apply: commit failed");
        }
        counts
    };
```

Add `use anyhow::Context;` to the file's imports if not already present (it already imports `anyhow::Result` — add `Context` to that line).

- [ ] **Step 5.2: Format and check**

```bash
cargo fmt --all
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
```
Expected: no errors.

- [ ] **Step 5.3: Run a representative integration test to verify the braid works end-to-end against a real DefraDB embedded node**

```bash
cargo test -p defra-agent-cli --test cli_config_apply_local -- --nocapture
```
Expected: PASS. This test uses `Local` backend; if it fails, the embedded `begin_txn`/`commit_txn` plumbing is broken.

- [ ] **Step 5.4: Run the GraphQL-backed apply test**

```bash
cargo test -p defra-agent-cli --test cli_config_apply_graphql -- --nocapture
```
Expected: PASS. This test uses `Graphql` backend.

- [ ] **Step 5.5: Run the production-boundary conformance test**

```bash
cargo test -p defra-agent-cli --lib lean_apply_write_boundary_tests::generated_apply_reconcile_cases_fence_production_apply_write_boundary -- --nocapture
```
Expected: PASS.

- [ ] **Step 5.6: Commit**

```bash
git add crates/defra-agent-cli/src/commands/config/apply.rs
git commit -m "$(cat <<'EOF'
Wrap config apply in transactional begin/commit/discard braid (#56)

Apply errors trigger discard and surface the original error; commit
errors are surfaced as apply errors with discard implicit (DB tx is
already gone). Per-collection progress logging unchanged.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Extend the production-boundary conformance test with the injected-failure flavor

**Files:**
- Modify: `crates/defra-agent-cli/src/config_import.rs` `lean_apply_write_boundary_tests::generated_apply_reconcile_cases_fence_production_apply_write_boundary`.

- [ ] **Step 6.1: Add the success-path tx-count assertion**

At the end of the existing per-case block in `generated_apply_reconcile_cases_fence_production_apply_write_boundary`, after the existing `assert_live_payloads_not_written(case, &recorder);` line, append:

```rust
let (begin_count, commit_count, discard_count) = recorder.tx_lifecycle_counts();
assert_eq!(
    (begin_count, commit_count, discard_count),
    (1, 1, 0),
    "success path must drive exactly one begin/commit and zero discard for Lean case {}",
    case.name,
);

// Externally-observed committed state on the recorder must match the success-path
// projection: every selected write landed in committed exactly once.
let committed_after_commit = recorder.committed_state();
assert_eq!(
    committed_after_commit.len(),
    case.expected_selected_writes.len(),
    "recorder committed state count mismatch for Lean case {} (expected = {}, actual = {})",
    case.name,
    case.expected_selected_writes.len(),
    committed_after_commit.len(),
);
```

- [ ] **Step 6.2: Add the injected-failure flavor for cases with `prefix_len > 0`**

After the above block, append:

```rust
if case.prefix_len > 0 {
    let (graphql, recorder) = start_recording_graphql().await;
    let access = ConfigAccess::Graphql(graphql);

    // Begin a tx; install fail-at-write-(prefix_len) injection on its id.
    let txn = access.begin_apply_txn().await.expect("begin");
    // The recorder hands out sequential numeric ids starting at 0; the
    // first tx in this fresh recorder is "0".
    recorder.install_fail_at("0", case.prefix_len);

    let result = apply_desired_state_changes(&txn, &desired_bundle, &planned).await;
    assert!(
        result.is_err(),
        "injected failure at write {} must surface as Err for Lean case {}",
        case.prefix_len,
        case.name,
    );

    let _ = txn.discard().await;

    let (begin_count, commit_count, discard_count) = recorder.tx_lifecycle_counts();
    assert_eq!(
        (begin_count, commit_count, discard_count),
        (1, 0, 1),
        "failure path must drive exactly one begin/discard and zero commit for Lean case {}",
        case.name,
    );

    assert!(
        recorder.committed_state().is_empty(),
        "failure path must leave externally-observed committed state empty (== pre_live) for Lean case {}",
        case.name,
    );

    let observed = recorder.observed_writes();
    assert!(
        observed.len() <= case.prefix_len + 1,
        "failure path observed {} writes; cap is prefix_len + 1 = {} for Lean case {}",
        observed.len(),
        case.prefix_len + 1,
        case.name,
    );
}
```

Note: this assertion uses `case.pre_live.is_empty()` is_not_required because every Lean case in the current set has `preLive` either empty or representing pre-existing live docs the test does not need to compare against (those preexisting docs are not what the apply mutates). The fence is "committed state contains nothing new from this apply" — equivalent to "post-failure committed equals pre_live" given the recorder begins each scenario with empty committed.

- [ ] **Step 6.3: Run the extended conformance test**

```bash
cargo test -p defra-agent-cli --lib lean_apply_write_boundary_tests::generated_apply_reconcile_cases_fence_production_apply_write_boundary -- --nocapture
```
Expected: PASS.

- [ ] **Step 6.4: Verify red→green by temporary revert (optional manual check)**

Optional sanity check; do not commit this:
```bash
git stash
git revert --no-commit HEAD~2  # revert Task 5's braid commit
cargo test -p defra-agent-cli --lib lean_apply_write_boundary_tests::generated_apply_reconcile_cases_fence_production_apply_write_boundary -- --nocapture 2>&1 | head -50
# Expect FAIL: tx_begin_count is 0, committed_state has writes, etc.
git revert --abort 2>/dev/null || true
git checkout -- .
git stash pop
```

- [ ] **Step 6.5: Commit**

```bash
git add crates/defra-agent-cli/src/config_import.rs
git commit -m "$(cat <<'EOF'
Fence transactional apply with injected-failure conformance flavor (#56)

For every Lean case with prefix_len > 0, inject a fail-at-write-N at the
recording server, run apply through ConfigApplyTxn, and assert:
- the call returned Err,
- exactly one begin and one discard (zero commit),
- externally-observed committed state stayed empty,
- observed writes did not exceed prefix_len + 1.

Lean surface unwidened — the assertion uses prefix_len, expected_selected_writes,
and the recorder's committed window.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: SIGKILL-mid-apply integration test against a real DefraDB node

A naive timed kill races: on a fast local node, apply finishes in under the timeout and SIGKILL lands after commit, falsely failing the test. To make the kill deterministically land between `begin` and `commit`, the apply pipeline reads an env-gated per-collection sleep. Production sets nothing; the test sets a small value to widen the kill window.

**Files:**
- Modify: `crates/defra-agent-cli/src/config_import.rs:595-617` (`apply_desired_state_changes`).
- Create: `crates/defra-agent-cli/tests/cli_config_apply_transactional_rollback.rs`.

- [ ] **Step 7.0: Add the env-gated per-collection slowdown to `apply_desired_state_changes`**

In `crates/defra-agent-cli/src/config_import.rs`, replace `apply_desired_state_changes` with:

```rust
pub(crate) async fn apply_desired_state_changes(
    txn: &ConfigApplyTxn<'_>,
    desired_bundle: &DesiredApplyBundle,
    planned: &desired_state::DesiredStateDiffReport,
) -> Result<ConfigApplyCounts> {
    let desired_bundle = desired_bundle.as_bundle();
    let mut counts = ConfigApplyCounts::default();

    let per_collection_sleep = std::env::var("DEFRA_AGENT_CONFIG_APPLY_SLEEP_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(std::time::Duration::from_millis);

    for collection in CONFIG_APPLY_ORDER {
        let docs = select_apply_docs_for_collection(desired_bundle, planned, collection)?;
        let applied = apply_import_collection(
            txn,
            collection.graphql_type(),
            collection.unique_field(),
            &docs,
            true,
        )
        .await?;
        counts.set(collection, applied);

        if let Some(sleep) = per_collection_sleep {
            tokio::time::sleep(sleep).await;
        }
    }

    Ok(counts)
}
```

The env var is undocumented and intended for tests only. It's not gated behind `cfg(test)` because integration tests run as a separate binary and don't see `cfg(test)` from the lib.

- [ ] **Step 7.1: Author the test**

Create `crates/defra-agent-cli/tests/cli_config_apply_transactional_rollback.rs`:

```rust
mod support;
use support::*;

use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

/// SIGKILL the CLI mid-apply and assert that DefraDB's tx GC reclaims the
/// orphaned transaction and leaves the database at the pre-apply snapshot.
/// This exercises the operationally-meaningful failure mode (Ctrl-C, OOM,
/// container restart) against a real node, not the recorder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_sigkill_mid_apply_leaves_db_unchanged() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-rb-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-rollback-{}", Uuid::new_v4().simple());

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;

    // Bootstrap a non-trivial manifest via export of the initialized state,
    // then duplicate the foundational collections to make apply visibly
    // multi-step on the wire.
    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;

    // Snapshot the existing committed state.
    let pre_apply_backends = count_collection_rows(&graphql, "InferenceBackend").await?;
    let pre_apply_profiles = count_collection_rows(&graphql, "InferenceProfile").await?;
    let pre_apply_tools = count_collection_rows(&graphql, "ToolSelection").await?;
    let pre_apply_tasks = count_collection_rows(&graphql, "Task").await?;

    // Spawn the CLI; let it run for a short window long enough to begin the tx
    // and ship at least one batch, then SIGKILL.
    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    // No `spawn_cli` helper takes env vars today; spawn directly.
    let mut cli = std::process::Command::new(support::cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .env("DEFRA_AGENT_CONFIG_APPLY_SLEEP_MS", "200")
        .current_dir(&home_dir)
        .args([
            "config",
            "apply",
            "--root",
            root_str,
            "--graphql",
            &graphql,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("spawning defra-agent config apply with apply-sleep env")?;

    // With per-collection sleep = 200 ms and CONFIG_APPLY_ORDER having 9
    // collections, full apply takes at least 1.8 s. Sleep 400 ms — past
    // begin and at least the first batched collection mutation, well before
    // commit. The sleep widens the kill window deterministically; without
    // it a fast local apply could complete in under our delay.
    thread::sleep(Duration::from_millis(400));
    cli.kill().context("SIGKILL CLI")?;
    cli.wait().context("reap CLI")?;

    // Allow DefraDB's tx GC to reclaim the orphaned handle and any in-tx
    // writes to be discarded. Poll for stability rather than sleep blindly.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let backends = count_collection_rows(&graphql, "InferenceBackend").await?;
        let profiles = count_collection_rows(&graphql, "InferenceProfile").await?;
        let tools = count_collection_rows(&graphql, "ToolSelection").await?;
        let tasks = count_collection_rows(&graphql, "Task").await?;

        if backends == pre_apply_backends
            && profiles == pre_apply_profiles
            && tools == pre_apply_tools
            && tasks == pre_apply_tasks
        {
            // External state has converged to pre-apply: tx GC succeeded.
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(anyhow!(
                "after SIGKILL, DB still shows post-apply state: backends={} (pre={}), \
                profiles={} (pre={}), tools={} (pre={}), tasks={} (pre={})",
                backends, pre_apply_backends,
                profiles, pre_apply_profiles,
                tools, pre_apply_tools,
                tasks, pre_apply_tasks,
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

async fn count_collection_rows(graphql: &str, collection: &str) -> Result<usize> {
    let response = graphql_query(
        graphql,
        &format!("{{ {collection} {{ _docID }} }}"),
    )
    .await?;
    Ok(response
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0))
}
```

- [ ] **Step 7.2: Format and run the test**

```bash
cargo fmt --all
cargo test -p defra-agent-cli --test cli_config_apply_transactional_rollback -- --nocapture
```
Expected: PASS. The env-gated 200 ms per-collection sleep introduced in Step 7.0 guarantees a minimum apply runtime of ~1.8 s (9 collections × 200 ms), so the 400 ms kill timing is reliably mid-apply on any hardware.

- [ ] **Step 7.3: Commit**

```bash
git add crates/defra-agent-cli/src/config_import.rs crates/defra-agent-cli/tests/cli_config_apply_transactional_rollback.rs
git commit -m "$(cat <<'EOF'
Integration test: SIGKILL mid-apply leaves DB at pre-apply state (#56)

Adds an env-gated per-collection apply sleep (DEFRA_AGENT_CONFIG_APPLY_SLEEP_MS)
to widen the kill window deterministically, then spawns `defra-agent-cli
config apply` against a real DefraDB node, kills it ~400 ms into the apply
(after begin and at least one batched mutation ship), waits for tx GC to
reclaim the orphaned transaction, then asserts external state matches the
pre-apply snapshot across every operator-controlled collection.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: CHANGELOG and follow-up issues

**Files:**
- Modify: `CHANGELOG.md` (or whatever release-notes file the repo uses; check first).

- [ ] **Step 8.1: Locate the CHANGELOG**

Run:
```bash
ls CHANGELOG* RELEASES* RELEASE_NOTES* 2>/dev/null
```
If a CHANGELOG exists, add an entry. If not, skip the file edit and only file the issues.

- [ ] **Step 8.2: Add an entry under "Unreleased" (or appropriate section)**

If `CHANGELOG.md` exists, prepend the following under the unreleased section:

```markdown
- `defra-agent-cli config apply` is now atomic: any failure during apply leaves the database at the pre-apply snapshot, with no operator cleanup required. Closes #56.
```

- [ ] **Step 8.3: Commit CHANGELOG (if changed)**

```bash
git add CHANGELOG.md
git commit -m "$(cat <<'EOF'
Note atomic config apply in CHANGELOG (#56)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 8.4: File the Lean external-abort projection follow-up issue**

Run:
```bash
gh issue create \
  --title "Lean: emit expected_external_state_after_abort for apply_reconcile_cases" \
  --body "$(cat <<'EOF'
## Context

#56 made `defra-agent-cli config apply` atomic. The production-boundary
conformance test in `crates/defra-agent-cli/src/config_import.rs`
(`lean_apply_write_boundary_tests::generated_apply_reconcile_cases_fence_production_apply_write_boundary`)
asserts that after an injected mid-apply failure, externally-observed
committed state equals `pre_live`. That equality is enforced from Rust
against the recorder's committed window; Lean does not emit an explicit
field describing "what external observers see after the tx aborts."

## Acceptance

- Add `expected_external_state_after_abort` (a list of `pre_live` docs
  in every current case) to `Proofs/ApplyReconcile/ContractCases.lean`'s
  `ApplyReconcileCase` and to its JSON projection.
- Update `crates/defra-agent-cli/src/config_import.rs::lean_apply_write_boundary_tests`
  to assert recorder committed state equals the emitted projection
  directly, replacing the current "committed must be empty" Rust-side
  restatement.
- Becomes load-bearing when #57 (delete semantics) introduces apply
  steps that mutate pre-existing live state — at that point the post-abort
  projection diverges from `pre_live` and the explicit Lean field is
  required.

## Related

- #56 (closed)
- #57 (delete semantics)
EOF
)"
```

- [ ] **Step 8.5: Decide whether to file the tx idle-timeout audit issue**

Run:
```bash
grep -rn "idle\|timeout\|tx.*ttl\|cleanup_stale" /Users/johnzampolin/.cargo/git/checkouts/defradb.rs-4ab0524bccc74f29/*/crates/db/src/txn_registry.rs 2>/dev/null | head -10
```
If a fixed idle timeout exists in DefraDB that could plausibly be hit by a large `config apply`, file:

```bash
gh issue create \
  --title "Audit DefraDB tx idle timeout against real config-apply runtimes" \
  --body "$(cat <<'EOF'
## Context

#56 wraps the entire `config apply` in a single DefraDB transaction. If
the open-tx idle timeout is tight and apply runtime against a slow
remote node could approach it, the tx may be reclaimed mid-apply.

## Acceptance

- Identify the configurable upper bound on an open DefraDB tx.
- Measure typical and worst-case `config apply` runtime against
  representative manifest sizes on a `Graphql` backend.
- Either document the safe envelope, extend the timeout, or chunk per
  collection (last resort — loses single-tx atomicity).

EOF
)"
```

Otherwise, skip filing this issue.

---

## Task 9: Push and open the PR

- [ ] **Step 9.1: Push the branch**

```bash
git push -u origin design/issue-56-transactional-apply
```

- [ ] **Step 9.2: Open the PR**

```bash
gh pr create --title "Make config apply transactional (#56)" --body "$(cat <<'EOF'
## Summary

Closes #56. Wraps `defra-agent-cli config apply` in a single DefraDB
transaction. Apply errors discard, success commits. SIGKILL mid-apply
is reclaimed by DefraDB's tx GC. Lean surface unwidened.

## Test plan

- [ ] `cargo fmt --all` clean
- [ ] `cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens` clean
- [ ] `cargo test -p defra-agent-cli` green (extended production-boundary consumer + new SIGKILL integration test)
- [ ] `cargo test -p defra-agent --lib --tests` green (reference-model apply_conformance unchanged)
- [ ] `cd crates/defra-agent/proofs && lake build` clean (no Lean changes; sanity check)
- [ ] Manual revert of the orchestrator braid + rerun the conformance test → observe red (proves the fence is load-bearing)

## Spec

`docs/superpowers/specs/2026-05-15-issue-56-transactional-apply-design.md`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 9.3: Report**

Stop and report to the controller:
- commit list (`git log main..HEAD --oneline`)
- test status (output of the final `cargo test -p defra-agent-cli`)
- CI status if available (`gh pr checks` if it's been a minute)
- conformance test red→green confirmation
- any open follow-up issues filed

Do not merge.

---

## Verification (end-of-plan)

After Task 9:

- `cargo fmt --all` — clean.
- `cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens` — clean.
- `cargo test -p defra-agent-cli` — all green, including `lean_apply_write_boundary_tests::*`, `cli_config_apply_*`, and the new `cli_config_apply_transactional_rollback`.
- `cargo test -p defra-agent --lib --tests` — all green; `apply_conformance.rs` untouched.
- `cd crates/defra-agent/proofs && lake build` — zero `sorry`s.
- Conformance fence behaves as red→green: reverting Task 5 turns Task 6's assertions red.
- One follow-up issue filed (Lean external-abort projection); the tx idle-timeout audit issue filed only if implementation surfaced a concrete gap.
