//! One consistent view of the store for every read a live receipt needs.
//!
//! The collector reads a request row, its capture rows, its call rows, the
//! session's request set, and then per-field commit fingerprints. Issued one
//! after another through `ConfigAccess::execute`, those reads can straddle a
//! concurrent write: a request row read before a terminal lifecycle flip can be
//! paired with a commit fingerprint read after it, and the published receipt
//! then describes a state that never existed.
//!
//! [`observe`] therefore performs every read through one [`SnapshotQuery`], and
//! [`snapshot`] supplies that query from a single
//! `ConfigAccess::transact_readonly` transaction: DefraDB's native snapshot for
//! an embedded node, and the served node's own read-only transaction over HTTP.
//! Rows and commit fingerprints in one [`Observation`] are then from one view.
//!
//! Limits, stated honestly:
//! * The snapshot is the store's, not ours. `?read_only=true` is enforced by the
//!   served node; this module only refuses to read outside the transaction.
//! * Capture payload blocks and delta bases are content addressed and are read
//!   after the transaction by the existing decoder, which verifies each block
//!   against the manifest's recorded field commit and each delta base against
//!   its recorded commit. A rewritten block fails decoding instead of
//!   publishing, so the post-snapshot read cannot silently change the evidence.
//! * A write that commits after the snapshot opened is invisible here, which is
//!   the point; a write that commits before it is included whole.

use std::collections::BTreeMap;

use anyhow::{ensure, Context, Result};
use futures::future::BoxFuture;
use gents::inference::rendered_request::commits;
use gents::store::config_client::{ConfigAccess, ConfigApplyTxn};
use gents::store::graphql::escape_graphql_string;
use serde_json::Value;

use super::validation::{rows, string};
use super::{ReceiptLimits, ReceiptRequest};

/// The single executor every read of one observation goes through.
pub(super) trait SnapshotQuery {
    fn query<'a>(&'a self, document: &'a str) -> BoxFuture<'a, Result<Value>>;
}

impl SnapshotQuery for ConfigApplyTxn<'_> {
    fn query<'a>(&'a self, document: &'a str) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move { Ok(self.execute(document).await?) })
    }
}

/// The highest commit for one field, as the receipt records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FieldFingerprint {
    pub cid: String,
    pub height: i64,
}

/// Every row and commit fingerprint one request's evidence needs, read from one
/// view of the store.
#[derive(Clone, Debug)]
pub(super) struct Observation {
    pub request: Value,
    pub captures: Vec<Value>,
    pub calls: Vec<Value>,
    pub session_requests: Vec<Value>,
    /// Keyed by `(physical docID, field name)`.
    pub commits: BTreeMap<(String, String), FieldFingerprint>,
}

/// Collect one observation per request inside a single read-only transaction.
pub(super) async fn snapshot(
    access: &ConfigAccess,
    requests: &[ReceiptRequest],
    limits: ReceiptLimits,
) -> Result<Vec<Observation>> {
    access
        .transact_readonly("test.live_receipt.snapshot", |txn| {
            Box::pin(async move {
                let mut observations = Vec::with_capacity(requests.len());
                for expected in requests {
                    observations.push(observe(txn, expected, limits).await?);
                }
                Ok::<_, anyhow::Error>(observations)
            })
        })
        .await
}

/// Read one request's complete evidence through `query`, which must answer from
/// one view of the store.
pub(super) async fn observe<Q: SnapshotQuery + Sync>(
    query: &Q,
    expected: &ReceiptRequest,
    limits: ReceiptLimits,
) -> Result<Observation> {
    let request_id = escape_graphql_string(&expected.request_id);
    // Deliberately query the logical ID broadly: a sibling physical identity
    // must fail validation instead of disappearing behind an ownership filter.
    let request_query = format!(
        r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{request_id}"}}}}, limit: 2) {{
            _docID request_id node_did requester_did agent_id session_id lifecycle_state
            created_at claimed_at terminalized_at stopped_before_running
        }} }}"#
    );
    let response = query.query(&request_query).await?;
    let request_rows = rows(&response, "AgentRequest", 1)?;
    let [request] = request_rows else {
        anyhow::bail!("receipt requires exactly one AgentRequest")
    };
    let request = request.clone();
    let physical = string(&request, "_docID")?.to_owned();

    let query_limit = limits.max_attempts_per_request + 1;
    let attempts_query = format!(
        r#"{{
        RenderedRequest(filter: {{request_id: {{_eq: "{request_id}"}}}}, limit: {query_limit}) {{
            _docID capture_key request_doc_id request_commit_cid request_id session_id
            node_did requester_did agent_id capture_scope turn_index attempt capture_version
            model_name source request_json provenance_json created_at
        }}
        InferenceCall(filter: {{request_id: {{_eq: "{request_id}"}}}}, limit: {query_limit}) {{
            _docID call_id request_id request_doc_id session_id node_did agent_id call_seq
            model_name profile_id backend_id call_kind attempt call_state queued_at started_at ended_at
        }} }}"#
    );
    let response = query.query(&attempts_query).await?;
    let captures = rows(
        &response,
        "RenderedRequest",
        limits.max_attempts_per_request,
    )?
    .to_vec();
    let calls = rows(&response, "InferenceCall", limits.max_attempts_per_request)?.to_vec();

    let session_query = format!(
        r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{node}"}},
        session_id: {{_eq: "{session}"}}}}, limit: {limit}) {{
        _docID request_id node_did requester_did agent_id session_id lifecycle_state
    }} }}"#,
        node = escape_graphql_string(&expected.node_did),
        session = escape_graphql_string(&expected.session_id),
        limit = limits.max_requests + 1
    );
    let response = query.query(&session_query).await?;
    let session_requests = rows(&response, "AgentRequest", limits.max_requests)?.to_vec();

    let mut wanted = BTreeMap::<String, Vec<String>>::new();
    wanted
        .entry(physical.clone())
        .or_default()
        .push("lifecycle_state".to_owned());
    for capture in &captures {
        wanted
            .entry(string(capture, "_docID")?.to_owned())
            .or_default()
            .extend(["request_json".to_owned(), "provenance_json".to_owned()]);
    }
    for call in &calls {
        wanted
            .entry(string(call, "_docID")?.to_owned())
            .or_default()
            .push("call_state".to_owned());
    }
    let commits = read_commits(query, &wanted).await?;

    Ok(Observation {
        request,
        captures,
        calls,
        session_requests,
        commits,
    })
}

/// Read the highest commit for every wanted field, batched under DefraDB's
/// per-level field width guardrail. A missing commit is an error, never an
/// unchanged value.
async fn read_commits<Q: SnapshotQuery + Sync>(
    query: &Q,
    wanted: &BTreeMap<String, Vec<String>>,
) -> Result<BTreeMap<(String, String), FieldFingerprint>> {
    let doc_ids = wanted.keys().cloned().collect::<Vec<_>>();
    let mut fingerprints = BTreeMap::new();
    for batch in doc_ids.chunks(commits::FIELD_COMMIT_ALIAS_BATCH) {
        let mut document = String::from("query {");
        for (index, doc_id) in batch.iter().enumerate() {
            document.push_str(&format!(
                r#" w{index}: _commits(docID: "{doc_id}") {{ cid height fieldName }}"#,
                doc_id = escape_graphql_string(doc_id),
            ));
        }
        document.push_str(" }");
        let response = query.query(&document).await?;
        ensure!(
            response
                .get("errors")
                .is_none_or(|value| value.is_null() || value.as_array().is_some_and(Vec::is_empty)),
            "receipt commit query returned errors"
        );
        let data = response
            .get("data")
            .and_then(Value::as_object)
            .context("receipt commit query carried no data object")?;
        for (index, doc_id) in batch.iter().enumerate() {
            let rows = data
                .get(&format!("w{index}"))
                .and_then(Value::as_array)
                .with_context(|| format!("receipt commit query carried no array for {doc_id}"))?;
            for field in &wanted[doc_id] {
                let fingerprint = highest_field_commit(rows, field).with_context(|| {
                    format!("receipt lacks durable {field} commit for {doc_id}")
                })?;
                ensure!(
                    !fingerprint.cid.is_empty() && fingerprint.height > 0,
                    "invalid field commit"
                );
                fingerprints.insert((doc_id.clone(), field.clone()), fingerprint);
            }
        }
    }
    Ok(fingerprints)
}

fn highest_field_commit(rows: &[Value], field_name: &str) -> Option<FieldFingerprint> {
    rows.iter()
        .filter(|commit| commit.get("fieldName").and_then(Value::as_str) == Some(field_name))
        .filter_map(|commit| {
            Some(FieldFingerprint {
                cid: commit.get("cid")?.as_str()?.to_owned(),
                height: commit.get("height").and_then(Value::as_i64)?,
            })
        })
        .max_by(|left, right| (left.height, &left.cid).cmp(&(right.height, &right.cid)))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use gents::eval::runner::embedded::EmbeddedHome;
    use gents::store::graphql::created_doc_id;
    use serde_json::json;

    use super::*;
    use gents::documents::InferenceSampling;

    use crate::support::live_receipt::{ExpectedCall, ReceiptLimits};

    fn expectation() -> ReceiptRequest {
        ReceiptRequest {
            node_did: "node".into(),
            requester_did: "requester".into(),
            agent_id: "agent".into(),
            session_id: "session".into(),
            request_id: "request".into(),
            request_doc_id: Some("request-doc".into()),
            lifecycle_state: "completed".into(),
            ordinary: ExpectedCall {
                backend_id: "backend".into(),
                model: "model".into(),
                endpoint: "https://provider.invalid".into(),
                route_path: "/v1/chat/completions".into(),
                provider_family: "OpenAiCompatible".into(),
                source: "openai_chat_completions".into(),
                sampling: InferenceSampling {
                    top_k: Some(17),
                    seed: Some(42),
                    ..Default::default()
                },
                output_token_cap: 1024,
            },
            summary: None,
            require_summary: false,
        }
    }

    /// A store whose reads answer from a view frozen when the observation
    /// started, while a concurrent writer keeps changing the live state.
    struct FrozenStore {
        view: State,
        live: Mutex<State>,
        queries: Mutex<Vec<String>>,
        mutate_after: usize,
        pending: Option<State>,
    }

    #[derive(Clone)]
    struct State {
        request: Value,
        captures: Vec<Value>,
        calls: Vec<Value>,
        session_requests: Vec<Value>,
        commits: BTreeMap<String, Vec<Value>>,
    }

    impl State {
        fn baseline() -> Self {
            let request = json!({"_docID":"request-doc","request_id":"request","node_did":"node",
                "requester_did":"requester","agent_id":"agent","session_id":"session",
                "lifecycle_state":"completed","created_at":"2026-10-31T00:00:00Z",
                "terminalized_at":"2026-10-31T00:00:01Z"});
            let capture = json!({"_docID":"capture-doc","capture_key":"key","request_doc_id":"request-doc",
                "request_commit_cid":"commit","request_id":"request","session_id":"session",
                "node_did":"node","requester_did":"requester","agent_id":"agent",
                "capture_scope":"inference.1","turn_index":0,"attempt":0,"capture_version":2,
                "model_name":"model","source":"openai_chat_completions","request_json":"{}",
                "provenance_json":"{}","created_at":"2026-10-31T00:00:00Z"});
            let call = json!({"_docID":"call-doc","call_id":"call","request_id":"request",
                "request_doc_id":"request-doc","session_id":"session","node_did":"node",
                "agent_id":"agent","call_seq":1,"model_name":"model","profile_id":"profile",
                "backend_id":"backend","call_kind":"inference","attempt":1,
                "call_state":"completed","started_at":"2026-10-31T00:00:00Z",
                "ended_at":"2026-10-31T00:00:01Z"});
            let mut commits = BTreeMap::new();
            commits.insert(
                "request-doc".to_owned(),
                vec![json!({"cid":"lifecycle-commit","height":2,"fieldName":"lifecycle_state"})],
            );
            commits.insert(
                "capture-doc".to_owned(),
                vec![
                    json!({"cid":"request-json-commit","height":1,"fieldName":"request_json"}),
                    json!({"cid":"provenance-commit","height":1,"fieldName":"provenance_json"}),
                ],
            );
            commits.insert(
                "call-doc".to_owned(),
                vec![json!({"cid":"call-state-commit","height":1,"fieldName":"call_state"})],
            );
            Self {
                request: request.clone(),
                captures: vec![capture],
                calls: vec![call.clone()],
                session_requests: vec![request],
                commits,
            }
        }

        /// A terminal lifecycle flip, a late call, and a capture rewrite.
        fn mutated() -> Self {
            let mut state = Self::baseline();
            state.request["lifecycle_state"] = json!("failed");
            state.request["terminalized_at"] = json!("2026-10-31T00:00:09Z");
            state.captures[0]["request_json"] = json!("{\"rewritten\":true}");
            let mut late = state.calls[0].clone();
            late["_docID"] = json!("late-call-doc");
            late["call_id"] = json!("late-call");
            late["call_seq"] = json!(2);
            state.calls.push(late);
            state.commits.insert(
                "request-doc".to_owned(),
                vec![json!({"cid":"lifecycle-commit-2","height":3,"fieldName":"lifecycle_state"})],
            );
            state.commits.insert(
                "capture-doc".to_owned(),
                vec![
                    json!({"cid":"request-json-commit-2","height":2,"fieldName":"request_json"}),
                    json!({"cid":"provenance-commit","height":1,"fieldName":"provenance_json"}),
                ],
            );
            state.commits.insert(
                "late-call-doc".to_owned(),
                vec![json!({"cid":"late-call-state-commit","height":1,"fieldName":"call_state"})],
            );
            state
        }

        fn respond(&self, document: &str) -> Value {
            if document.contains("_commits(docID:") {
                let mut data = serde_json::Map::new();
                let mut rest = document;
                let mut index = 0;
                while let Some(start) = rest.find("_commits(docID: \"") {
                    rest = &rest[start + "_commits(docID: \"".len()..];
                    let end = rest.find('"').expect("commit alias names a document");
                    let doc_id = &rest[..end];
                    let rows = self.commits.get(doc_id).cloned().unwrap_or_default();
                    data.insert(format!("w{index}"), json!(rows));
                    index += 1;
                }
                return json!({"data": data});
            }
            if document.contains("request_id: {_eq:") && document.contains("AgentRequest") {
                return json!({"data": {"AgentRequest": [self.request.clone()]}});
            }
            if document.contains("RenderedRequest(filter") {
                return json!({"data": {
                    "RenderedRequest": self.captures.clone(),
                    "InferenceCall": self.calls.clone(),
                }});
            }
            if document.contains("AgentRequest") {
                return json!({"data": {"AgentRequest": self.session_requests.clone()}});
            }
            panic!("unexpected receipt query: {document}");
        }
    }

    impl SnapshotQuery for FrozenStore {
        fn query<'a>(&'a self, document: &'a str) -> BoxFuture<'a, Result<Value>> {
            Box::pin(async move {
                let mut queries = self.queries.lock().unwrap();
                queries.push(document.to_owned());
                let answered = self.view.respond(document);
                if queries.len() == self.mutate_after {
                    if let Some(pending) = &self.pending {
                        *self.live.lock().unwrap() = pending.clone();
                    }
                }
                Ok(answered)
            })
        }
    }

    fn frozen(mutate_after: usize, pending: Option<State>) -> FrozenStore {
        FrozenStore {
            view: State::baseline(),
            live: Mutex::new(State::baseline()),
            queries: Mutex::new(Vec::new()),
            mutate_after,
            pending,
        }
    }

    #[tokio::test]
    async fn every_read_uses_the_one_snapshot_and_a_late_write_stays_invisible() {
        let store = frozen(2, Some(State::mutated()));
        let observation = observe(&store, &expectation(), ReceiptLimits::default())
            .await
            .unwrap();
        // The concurrent writer landed mid-observation...
        assert_eq!(
            store.live.lock().unwrap().request["lifecycle_state"],
            json!("failed")
        );
        // ...and none of it reached the observation.
        assert_eq!(observation.request["lifecycle_state"], json!("completed"));
        assert_eq!(observation.calls.len(), 1);
        assert_eq!(observation.captures[0]["request_json"], json!("{}"));
        assert_eq!(
            observation
                .commits
                .get(&("request-doc".to_owned(), "lifecycle_state".to_owned()))
                .unwrap()
                .cid,
            "lifecycle-commit"
        );
        // Every read went through this one executor: request, attempts, session
        // set, and one commit statement.
        assert_eq!(store.queries.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn a_snapshot_taken_after_the_write_carries_the_new_state() {
        let store = FrozenStore {
            view: State::mutated(),
            live: Mutex::new(State::mutated()),
            queries: Mutex::new(Vec::new()),
            mutate_after: 0,
            pending: None,
        };
        let observation = observe(&store, &expectation(), ReceiptLimits::default())
            .await
            .unwrap();
        assert_eq!(observation.request["lifecycle_state"], json!("failed"));
        assert_eq!(observation.calls.len(), 2);
        assert_eq!(
            observation.captures[0]["request_json"],
            json!("{\"rewritten\":true}")
        );
        assert_eq!(
            observation
                .commits
                .get(&("request-doc".to_owned(), "lifecycle_state".to_owned()))
                .unwrap()
                .cid,
            "lifecycle-commit-2"
        );
    }

    #[tokio::test]
    async fn a_missing_commit_fails_closed_instead_of_reading_as_unchanged() {
        let mut store = frozen(0, None);
        store.view.commits.remove("capture-doc");
        let failure = observe(&store, &expectation(), ReceiptLimits::default())
            .await
            .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("lacks durable request_json commit"),
            "{failure:#}"
        );
        let mut store = frozen(0, None);
        store.view.commits.insert("call-doc".to_owned(), Vec::new());
        let failure = observe(&store, &expectation(), ReceiptLimits::default())
            .await
            .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("lacks durable call_state commit"),
            "{failure:#}"
        );
    }

    #[tokio::test]
    async fn a_local_snapshot_hides_a_write_that_commits_after_it_opens() {
        let home = EmbeddedHome::create_temp("live-receipt-snapshot")
            .await
            .unwrap();
        let node = home.node.clone();
        let access = ConfigAccess::local(node.clone());
        let created = access
            .write(
                "test.live_receipt_snapshot",
                r#"mutation {
                create_AgentRequest(input: {request_id: "request", node_did: "node",
                    requester_did: "requester", agent_id: "agent", session_id: "session",
                    lifecycle_state: "running", created_at: "2026-10-31T00:00:00Z"}) { _docID }
            }"#,
            )
            .await
            .unwrap();
        let physical = created_doc_id(&created, "AgentRequest").unwrap();
        let read = format!(
            r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{physical}"}}}}, limit: 2) {{
                _docID lifecycle_state
            }} }}"#
        );
        let flip = format!(
            r#"mutation {{ update_AgentRequest(filter: {{_docID: {{_eq: "{physical}"}}}},
                input: {{lifecycle_state: "completed"}}) {{ _docID }} }}"#
        );
        // A separate task commits the flip while the snapshot is open. The
        // transaction cannot issue it itself: a nested write is refused.
        let (start, started) = tokio::sync::oneshot::channel::<()>();
        let (finished, done) = tokio::sync::oneshot::channel::<()>();
        let writer = {
            let access = access.clone();
            tokio::spawn(async move {
                started.await.unwrap();
                access
                    .write("test.live_receipt_snapshot_flip", &flip)
                    .await
                    .unwrap();
                finished.send(()).unwrap();
            })
        };
        let start = std::sync::Arc::new(tokio::sync::Mutex::new(Some(start)));
        let done = std::sync::Arc::new(tokio::sync::Mutex::new(Some(done)));
        let (start_in, done_in) = (start.clone(), done.clone());
        let (first, second) = access
            .transact_readonly("test.live_receipt.snapshot_isolation", |txn| {
                let read = &read;
                let start = start_in.clone();
                let done = done_in.clone();
                Box::pin(async move {
                    let first = txn.execute(read).await?;
                    if let Some(sender) = start.lock().await.take() {
                        sender.send(()).ok();
                    }
                    if let Some(receiver) = done.lock().await.take() {
                        receiver.await.ok();
                    }
                    let second = txn.execute(read).await?;
                    Ok::<_, anyhow::Error>((first, second))
                })
            })
            .await
            .unwrap();
        writer.await.unwrap();
        let state = |response: &Value| {
            response["data"]["AgentRequest"][0]["lifecycle_state"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(state(&first), "running");
        assert_eq!(
            state(&second),
            "running",
            "the snapshot moved under the read"
        );
        let outside = access.execute(&read).await.unwrap();
        assert_eq!(state(&outside), "completed");
        node.shutdown().await;
    }
}
