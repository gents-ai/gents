use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct MeasuredRead<'a> {
    access: &'a crate::config_client::ConfigAccess,
    queries: AtomicUsize,
    max_bytes: AtomicUsize,
    total_bytes: AtomicUsize,
}

impl<'a> MeasuredRead<'a> {
    fn new(access: &'a crate::config_client::ConfigAccess) -> Self {
        Self {
            access,
            queries: AtomicUsize::new(0),
            max_bytes: AtomicUsize::new(0),
            total_bytes: AtomicUsize::new(0),
        }
    }

    fn snapshot(&self) -> Value {
        json!({
            "queries": self.queries.load(Ordering::Relaxed),
            "max_query_bytes": self.max_bytes.load(Ordering::Relaxed),
            "total_query_bytes": self.total_bytes.load(Ordering::Relaxed),
        })
    }
}

#[async_trait::async_trait]
impl crate::config_client::ConfigRead for MeasuredRead<'_> {
    async fn execute_read(
        &self,
        document: &str,
    ) -> Result<Value, crate::config_client::ConfigClientError> {
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.max_bytes.fetch_max(document.len(), Ordering::Relaxed);
        self.total_bytes
            .fetch_add(document.len(), Ordering::Relaxed);
        self.access.execute(document).await
    }
}

async fn seed_unrelated_completed_requests(
    access: &crate::config_client::ConfigAccess,
    count: usize,
) -> usize {
    let inputs = (0..count).map(|i| json!({
        "purpose": "normal", "request_id": format!("unrelated-{i}"),
        "node_did": "did:test:accounting", "requester_did": null,
        "session_id": "session", "created_at": format!("2026-10-01T00:{:02}:{:02}Z", i / 60, i % 60),
        "lifecycle_state": "completed", "agent_id": "default", "backend_id": "backend",
        "content": "unrelated completed request",
    })).collect::<Vec<_>>();
    let input = gents_protocol::graphql::graphql_input_literal(&json!(inputs)).unwrap();
    let data = access
        .write(
            "context_accounting.unrelated_fixture",
            &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    let documents = crate::graphql::mutation_documents(&data, "create_AgentRequest")
        .unwrap()
        .unwrap();
    assert_eq!(documents.len(), count);
    documents
        .iter()
        .map(|row| row["_docID"].as_str().unwrap().len())
        .sum()
}

#[tokio::test]
async fn query_inputs_do_not_grow_with_unrelated_completed_requests() {
    let db = crate::support::test_db("context-query-input-cost").await;
    let access = crate::config_client::ConfigAccess::local(db.node.clone());
    let own = request(&access, "session", None, "2026-10-01T00:00:00Z").await;
    for i in 0..CONTEXT_CALL_LIMIT {
        call(
            &access,
            &own,
            &format!("modern-{i:02}"),
            Some("session"),
            Some("2026-10-08T00:00:00Z"),
            Some(i64::try_from(i).unwrap()),
        )
        .await;
    }
    let before = MeasuredRead::new(&access);
    let original =
        load_latest_session_context_calls(&before, "did:test:accounting", None, "session")
            .await
            .unwrap();
    assert_eq!(original.len(), CONTEXT_CALL_LIMIT);

    let unrelated_id_bytes = seed_unrelated_completed_requests(&access, 1000).await;
    let after = MeasuredRead::new(&access);
    let same = load_latest_session_context_calls(&after, "did:test:accounting", None, "session")
        .await
        .unwrap();
    assert_eq!(same, original);
    assert!(after.queries.load(Ordering::Relaxed) <= before.queries.load(Ordering::Relaxed));
    assert!(after.max_bytes.load(Ordering::Relaxed) <= before.max_bytes.load(Ordering::Relaxed));

    call(
        &access,
        &own,
        "retained-late",
        None,
        Some("2026-10-09T00:00:00Z"),
        Some(10),
    )
    .await;
    let retained = MeasuredRead::new(&access);
    let rows = load_latest_session_context_calls(&retained, "did:test:accounting", None, "session")
        .await
        .unwrap();
    assert_eq!(rows.len(), CONTEXT_CALL_LIMIT);
    assert_eq!(rows[0]["call_id"], "retained-late");
    assert!(retained.max_bytes.load(Ordering::Relaxed) < unrelated_id_bytes);

    if let Ok(path) = std::env::var("GENTS_CONTEXT_READ_PROFILE") {
        let profile = json!({
            "unrelated_completed_requests": 1000,
            "unrelated_physical_id_bytes": unrelated_id_bytes,
            "hot_before": before.snapshot(),
            "hot_after": after.snapshot(),
            "retained_after": retained.snapshot(),
        });
        tokio::fs::write(path, serde_json::to_vec_pretty(&profile).unwrap())
            .await
            .unwrap();
    }
}
