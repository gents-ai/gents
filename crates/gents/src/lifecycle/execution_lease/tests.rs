use super::*;
use std::{sync::Arc, time::Duration};

struct LeaseTestIdentity;

#[async_trait::async_trait]
impl crate::identity::AgentIdentity for LeaseTestIdentity {
    fn did(&self) -> &str {
        "did:test:execution-lease"
    }
    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>> {
        use sha2::Digest;
        Ok(sha2::Sha512::digest(payload).to_vec())
    }
    async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> Result<bool> {
        Ok(did == self.did() && self.sign(payload).await? == signature)
    }
    fn service_account(&self) -> Option<&crate::identity::ServiceAccount> {
        None
    }
}

async fn test_node() -> (Arc<EmbeddedNode>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    (node, dir)
}

async fn claimed_owner(node: &Arc<EmbeddedNode>) -> RequestLifecycle {
    RequestLifecycle::materialize_claimed_with_execution_binding(
        node.clone(),
        "general",
        Arc::new(LeaseTestIdentity),
        "hello",
        60,
        ExecutionOrigin::Interactive,
        "lease-test",
        TriggerLineage::default(),
    )
    .await
    .unwrap()
}

async fn owner(node: &Arc<EmbeddedNode>) -> RequestLifecycle {
    let mut lifecycle = claimed_owner(node).await;
    let writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        "did:test:execution-lease",
        Duration::ZERO,
    );
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    lifecycle
}

async fn request_row(node: &EmbeddedNode, doc_id: &str) -> AgentRequestRow {
    let result = node.execute(&format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ request_id lifecycle_state execution_generation execution_lease_expires_at execution_progress_seq }} }}"#,
        escape_graphql_string(doc_id),
    )).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    crate::graphql::first_row(&result, "AgentRequest")
        .unwrap()
        .unwrap()
}

fn lease_tuple(row: &AgentRequestRow) -> (Option<String>, Option<String>, Option<i64>) {
    (
        row.execution_generation.clone(),
        row.execution_lease_expires_at.clone(),
        row.execution_progress_seq,
    )
}

async fn await_expired(node: &EmbeddedNode, doc_id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row = request_row(node, doc_id).await;
            let deadline =
                DateTime::parse_from_rfc3339(row.execution_lease_expires_at.as_deref().unwrap())
                    .unwrap();
            if deadline < Utc::now() {
                assert_eq!(
                    row.lifecycle_state,
                    Some(RequestLifecycleState::Processing),
                    "drop relinquishes; recovery terminalizes"
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dropped owner must promptly expire its lease, before normal lease timeout");
}

#[tokio::test]
async fn execution_owner_drop_panic_and_abort_expire_for_recovery() {
    let (node, _dir) = test_node().await;
    for exit in ["drop", "panic", "abort"] {
        let lifecycle = owner(&node).await;
        let doc_id = lifecycle.request().doc_id.clone();
        let before = request_row(&node, &doc_id).await;
        assert!(
            DateTime::parse_from_rfc3339(before.execution_lease_expires_at.as_deref().unwrap())
                .unwrap()
                > Utc::now() + chrono::Duration::seconds(10)
        );
        match exit {
            "drop" => drop(lifecycle),
            "panic" => {
                let joined = tokio::spawn(async move {
                    let _owned_lifecycle = lifecycle;
                    panic!("simulated execution panic");
                })
                .await;
                assert!(joined.unwrap_err().is_panic());
            }
            "abort" => {
                let (ready, started) = tokio::sync::oneshot::channel();
                let task = tokio::spawn(async move {
                    let _owned_lifecycle = lifecycle;
                    ready.send(()).unwrap();
                    std::future::pending::<()>().await;
                });
                started.await.unwrap();
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            }
            _ => unreachable!(),
        }
        await_expired(&node, &doc_id).await;
        let report = RequestLifecycle::recover_all(&node, "did:test:execution-lease")
            .await
            .unwrap();
        assert_eq!(report.requests_recovered, 1, "{exit}");
        assert_eq!(
            request_row(&node, &doc_id).await.lifecycle_state,
            Some(RequestLifecycleState::Failed),
            "{exit}"
        );
    }
}

#[tokio::test]
async fn stale_execution_owner_drop_does_not_expire_successor_generation() {
    let (node, _dir) = test_node().await;
    let lifecycle = owner(&node).await;
    let doc_id = lifecycle.request().doc_id.clone();
    let successor_generation = uuid::Uuid::new_v4().to_string();
    let successor_deadline = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let response = node.execute(&format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_generation: "{}", execution_lease_expires_at: "{}", execution_progress_seq: 7 }}) {{ _docID }} }}"#,
        escape_graphql_string(&doc_id), escape_graphql_string(&successor_generation), escape_graphql_string(&successor_deadline),
    )).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let before = lease_tuple(&request_row(&node, &doc_id).await);
    let owners_before_drop = Arc::strong_count(&node);
    drop(lifecycle);
    // Drop transfers its node reference into the cleanup task. Wait for that
    // reference to be released, proving the stale CAS finished before asserting.
    tokio::time::timeout(Duration::from_secs(5), async {
        while Arc::strong_count(&node) >= owners_before_drop {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("detached relinquishment task must finish");
    assert_eq!(lease_tuple(&request_row(&node, &doc_id).await), before);
    let report = RequestLifecycle::recover_all(&node, "did:test:execution-lease")
        .await
        .unwrap();
    assert_eq!(report.requests_recovered, 0);
    assert_eq!(
        request_row(&node, &doc_id).await.lifecycle_state,
        Some(RequestLifecycleState::Processing)
    );
}

#[tokio::test]
async fn owned_stream_noops_do_not_renew_or_increment_execution_progress() {
    let (node, _dir) = test_node().await;
    let mut lifecycle = claimed_owner(&node).await;
    let request = lifecycle.request().clone();
    let writer =
        crate::streaming::DefraStreamWriter::new(node.clone(), &request.agent_did, Duration::ZERO);
    let doc_id = lifecycle.begin_owned_execution(&writer).await.unwrap();
    let initial = lease_tuple(&request_row(&node, &request.doc_id).await);
    assert!(!writer.write_tokens(&doc_id, "").await.unwrap());
    assert!(!writer.write_reasoning(&doc_id, "").await.unwrap());
    assert!(!writer.flush_pending(&doc_id).await.unwrap());
    writer.reset_tail(&doc_id).await.unwrap();
    assert_eq!(
        lease_tuple(&request_row(&node, &request.doc_id).await),
        initial
    );

    assert!(writer.write_tokens(&doc_id, "durable text").await.unwrap());
    let progressed = lease_tuple(&request_row(&node, &request.doc_id).await);
    assert_eq!(progressed.2, Some(initial.2.unwrap_or(0) + 1));
    assert!(
        DateTime::parse_from_rfc3339(progressed.1.as_deref().unwrap()).unwrap()
            > DateTime::parse_from_rfc3339(initial.1.as_deref().unwrap()).unwrap()
    );
    assert!(!writer.flush_pending(&doc_id).await.unwrap());
    assert!(!writer.write_tokens(&doc_id, "").await.unwrap());
    assert_eq!(
        lease_tuple(&request_row(&node, &request.doc_id).await),
        progressed
    );

    writer.reset_tail(&doc_id).await.unwrap();
    let cleared = lease_tuple(&request_row(&node, &request.doc_id).await);
    writer.reset_tail(&doc_id).await.unwrap();
    assert!(!writer.flush_pending(&doc_id).await.unwrap());
    assert!(!writer.write_reasoning(&doc_id, "").await.unwrap());
    assert_eq!(
        lease_tuple(&request_row(&node, &request.doc_id).await),
        cleared
    );
    lifecycle
        .terminalize_owned(&writer, RequestTerminalOutcome::Completed, None)
        .await
        .unwrap();
}

async fn response_count(node: &EmbeddedNode, request_doc_id: &str) -> usize {
    let result = node
        .execute(&format!(
            r#"{{ AgentResponse(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(request_doc_id),
        ))
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    result.data.as_ref().unwrap()["AgentResponse"]
        .as_array()
        .unwrap()
        .len()
}

fn begin_fence(lifecycle: &RequestLifecycle) -> ExecutionWriteFence {
    ExecutionWriteFence {
        request_doc_id: lifecycle.request.doc_id.clone(),
        execution_generation: lifecycle.execution_generation().unwrap().to_string(),
        lease_duration_secs: 120,
    }
}

fn response_create(lifecycle: &RequestLifecycle) -> String {
    let request = lifecycle.request();
    format!(
        r#"mutation {{ create_AgentResponse(input: {{
        response_key: "{request_id}", request_id: "{request_id}",
        request_doc_id: "{doc_id}", agent_did: "{did}", behavior_id: "general",
        session_id: "{session_id}", status: "streaming", content: "", reasoning: ""
    }}) {{ _docID }} }}"#,
        request_id = escape_graphql_string(&request.request_id),
        doc_id = escape_graphql_string(&request.doc_id),
        did = escape_graphql_string(&request.agent_did),
        session_id = escape_graphql_string(&request.session_id),
    )
}

#[tokio::test]
async fn owned_begin_rejects_expiry_without_materializing_response() {
    let (node, _dir) = test_node().await;
    let mut lifecycle = claimed_owner(&node).await;
    let doc_id = lifecycle.request.doc_id.clone();
    let expired = (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let result = node.execute(&format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_lease_expires_at: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(&doc_id), escape_graphql_string(&expired),
    )).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let before = request_row(&node, &doc_id).await;
    let writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        "did:test:execution-lease",
        Duration::ZERO,
    );
    assert!(lifecycle.begin_owned_execution(&writer).await.is_err());
    let after = request_row(&node, &doc_id).await;
    assert_eq!(after.lifecycle_state, Some(RequestLifecycleState::Claimed));
    assert_eq!(lease_tuple(&after), lease_tuple(&before));
    assert_eq!(response_count(&node, &doc_id).await, 0);
}

#[tokio::test]
async fn owned_begin_rolls_back_request_when_response_creation_fails() {
    let (node, _dir) = test_node().await;
    let lifecycle = claimed_owner(&node).await;
    let before = request_row(&node, &lifecycle.request.doc_id).await;
    let result = begin_fence(&lifecycle)
        .execute_response_write(
            &node,
            r#"mutation { create_MissingResponseCollection(input: {}) { _docID } }"#,
            ExecutionWriteKind::Begin,
        )
        .await;
    assert!(result.is_err());
    let after = request_row(&node, &lifecycle.request.doc_id).await;
    assert_eq!(after.lifecycle_state, Some(RequestLifecycleState::Claimed));
    assert_eq!(lease_tuple(&after), lease_tuple(&before));
    assert_eq!(response_count(&node, &lifecycle.request.doc_id).await, 0);
}

#[tokio::test]
async fn racing_owned_begins_commit_one_processing_response_pair() {
    let (node, _dir) = test_node().await;
    let lifecycle = claimed_owner(&node).await;
    let before = request_row(&node, &lifecycle.request.doc_id).await;
    let fence = begin_fence(&lifecycle);
    let mutation = response_create(&lifecycle);
    let (first, second) = tokio::join!(
        fence.execute_response_write(&node, &mutation, ExecutionWriteKind::Begin),
        fence.execute_response_write(&node, &mutation, ExecutionWriteKind::Begin),
    );
    assert_eq!(
        usize::from(first.is_ok()) + usize::from(second.is_ok()),
        1,
        "first={first:?}; second={second:?}"
    );
    let after = request_row(&node, &lifecycle.request.doc_id).await;
    assert_eq!(
        after.lifecycle_state,
        Some(RequestLifecycleState::Processing)
    );
    assert_eq!(
        lease_tuple(&after),
        lease_tuple(&before),
        "begin is not semantic renewal"
    );
    assert_eq!(response_count(&node, &lifecycle.request.doc_id).await, 1);
}

async fn observed_execution(node: &EmbeddedNode, doc_id: &str) -> AgentRequestRow {
    let mut row = request_row(node, doc_id).await;
    row.doc_id = Some(doc_id.to_owned());
    row
}

async fn expire_observed_execution(node: &EmbeddedNode, doc_id: &str) -> AgentRequestRow {
    let expiry = (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let response = node.execute(&format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_lease_expires_at: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(doc_id), escape_graphql_string(&expiry),
    )).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    observed_execution(node, doc_id).await
}

async fn recover_observed_execution(
    node: &EmbeddedNode,
    row: &AgentRequestRow,
) -> Result<TerminalizeResult> {
    recover_execution_generation(
        node,
        row,
        row.execution_generation.as_deref().unwrap(),
        row.execution_lease_expires_at.as_deref().unwrap(),
        row.execution_progress_seq.unwrap(),
        RequestTerminalOutcome::Failed,
        "expired regression execution",
    )
    .await
}

async fn terminal_response_snapshot(node: &EmbeddedNode, doc_id: &str) -> serde_json::Value {
    let result = node.execute(&format!(
        r#"{{ AgentResponse(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID status content reasoning completed_at }} }}"#,
        escape_graphql_string(doc_id),
    )).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let rows = result.data.as_ref().unwrap()["AgentResponse"]
        .as_array()
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "one response must represent the terminal winner"
    );
    rows[0].clone()
}

#[tokio::test]
async fn revoked_execution_rejects_stale_progress_stream_write_and_terminalization() {
    let (node, _dir) = test_node().await;
    let mut lifecycle = claimed_owner(&node).await;
    let writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        "did:test:execution-lease",
        Duration::ZERO,
    );
    let response_doc_id = lifecycle.begin_owned_execution(&writer).await.unwrap();
    writer
        .write_tokens(&response_doc_id, "preserved partial")
        .await
        .unwrap();
    let doc_id = lifecycle.request().doc_id.clone();
    let observed = observed_execution(&node, &doc_id).await;
    assert_eq!(
        revoke_execution_generation(
            &node,
            &observed,
            RequestTerminalOutcome::Superseded,
            "newer request"
        )
        .await
        .unwrap(),
        TerminalizeResult::Won
    );
    let terminal = request_row(&node, &doc_id).await;
    let response = terminal_response_snapshot(&node, &doc_id).await;
    assert_ne!(terminal.execution_generation, observed.execution_generation);
    assert_eq!(
        terminal.lifecycle_state,
        Some(RequestLifecycleState::Superseded)
    );
    assert_eq!(response["content"], "preserved partial");
    assert!(lifecycle.advance().await.is_err());
    assert!(writer
        .write_tokens(&response_doc_id, " stale overwrite")
        .await
        .is_err());
    assert_eq!(
        lifecycle
            .terminalize_owned_without_stream(RequestTerminalOutcome::Completed, None)
            .await
            .unwrap(),
        TerminalizeResult::Lost
    );
    assert_eq!(
        lease_tuple(&request_row(&node, &doc_id).await),
        lease_tuple(&terminal)
    );
    assert_eq!(terminal_response_snapshot(&node, &doc_id).await, response);
}

#[tokio::test]
async fn expired_owner_racing_recovery_cannot_win_terminalization() {
    let (node, _dir) = test_node().await;
    let mut lifecycle = owner(&node).await;
    let doc_id = lifecycle.request().doc_id.clone();
    let observed = expire_observed_execution(&node, &doc_id).await;
    let (owner_result, recovery_result) = tokio::join!(
        lifecycle.terminalize_owned_without_stream(RequestTerminalOutcome::Completed, None),
        recover_observed_execution(&node, &observed),
    );
    assert_eq!(owner_result.unwrap(), TerminalizeResult::Lost);
    assert_eq!(recovery_result.unwrap(), TerminalizeResult::Won);
    let terminal = request_row(&node, &doc_id).await;
    assert_eq!(
        terminal.lifecycle_state,
        Some(RequestLifecycleState::Failed)
    );
    assert_ne!(terminal.execution_generation, observed.execution_generation);
    assert_eq!(
        terminal_response_snapshot(&node, &doc_id).await["status"],
        "error"
    );
}

#[tokio::test]
async fn concurrent_recovery_of_one_observed_execution_commits_one_terminal_winner() {
    let (node, _dir) = test_node().await;
    // Exercise both creating an absent response and updating a streaming one.
    for processing in [false, true] {
        let lifecycle = if processing {
            owner(&node).await
        } else {
            claimed_owner(&node).await
        };
        let doc_id = lifecycle.request().doc_id.clone();
        let observed = expire_observed_execution(&node, &doc_id).await;
        let (first, second) = tokio::join!(
            recover_observed_execution(&node, &observed),
            recover_observed_execution(&node, &observed),
        );
        let outcomes = [first.unwrap(), second.unwrap()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == TerminalizeResult::Won)
                .count(),
            1,
            "{outcomes:?}"
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == TerminalizeResult::Lost)
                .count(),
            1,
            "{outcomes:?}"
        );
        let terminal = request_row(&node, &doc_id).await;
        let response = terminal_response_snapshot(&node, &doc_id).await;
        assert_eq!(
            terminal.lifecycle_state,
            Some(RequestLifecycleState::Failed)
        );
        assert_eq!(response["status"], "error");
        assert_eq!(
            recover_observed_execution(&node, &observed).await.unwrap(),
            TerminalizeResult::Lost
        );
        assert_eq!(
            lease_tuple(&request_row(&node, &doc_id).await),
            lease_tuple(&terminal)
        );
        assert_eq!(terminal_response_snapshot(&node, &doc_id).await, response);
    }
}

#[tokio::test]
async fn cancel_racing_provider_eof_commits_one_agreeing_terminal_pair() {
    let (node, _dir) = test_node().await;
    // Cover both committed orders and actual overlapping latch/terminal transactions.
    // EOF enters the owner as Failed; a latch observed by that transaction selects Interrupted.
    for ordering in 0..3 {
        let mut lifecycle = owner(&node).await;
        let doc_id = lifecycle.request().doc_id.clone();
        let request_id = lifecycle.request().request_id.clone();
        let outcome = match ordering {
            0 => {
                crate::interrupt::interrupt_request(&node, &request_id)
                    .await
                    .unwrap();
                lifecycle
                    .terminalize_owned_without_stream(
                        RequestTerminalOutcome::Failed,
                        Some("provider EOF without terminal event"),
                    )
                    .await
                    .unwrap()
            }
            1 => {
                let result = lifecycle
                    .terminalize_owned_without_stream(
                        RequestTerminalOutcome::Failed,
                        Some("provider EOF without terminal event"),
                    )
                    .await
                    .unwrap();
                crate::interrupt::interrupt_request(&node, &request_id)
                    .await
                    .unwrap();
                result
            }
            _ => {
                let (cancel, eof) = tokio::join!(
                    crate::interrupt::interrupt_request(&node, &request_id),
                    lifecycle.terminalize_owned_without_stream(
                        RequestTerminalOutcome::Failed,
                        Some("provider EOF without terminal event"),
                    ),
                );
                cancel.unwrap();
                eof.unwrap()
            }
        };
        assert_eq!(outcome, TerminalizeResult::Won);
        let terminal = request_row(&node, &doc_id).await;
        match ordering {
            0 => assert_eq!(
                terminal.lifecycle_state,
                Some(RequestLifecycleState::Interrupted)
            ),
            1 => assert_eq!(
                terminal.lifecycle_state,
                Some(RequestLifecycleState::Failed)
            ),
            _ => assert!(matches!(
                terminal.lifecycle_state,
                Some(RequestLifecycleState::Failed | RequestLifecycleState::Interrupted)
            )),
        }
        let response = terminal_response_snapshot(&node, &doc_id).await;
        assert_eq!(response["status"], "error");
        assert!(response["completed_at"].as_str().is_some());
        let replay = lifecycle
            .terminalize_owned_without_stream(
                RequestTerminalOutcome::Failed,
                Some("replayed provider EOF"),
            )
            .await
            .unwrap();
        assert_ne!(replay, TerminalizeResult::Won);
        assert_eq!(terminal_response_snapshot(&node, &doc_id).await, response);
        assert_eq!(
            request_row(&node, &doc_id).await.lifecycle_state,
            terminal.lifecycle_state
        );
    }
}

mod workspace_recovery;
