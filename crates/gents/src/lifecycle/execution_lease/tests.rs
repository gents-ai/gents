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
    crate::test_support::install_test_behavior(&node, "did:test:execution-lease", "general").await;
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

async fn claimed_owner_with_short_lease(node: &Arc<EmbeddedNode>) -> RequestLifecycle {
    let mut lifecycle = RequestLifecycle::materialize_pending_with_execution_binding(
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
    .unwrap();
    lifecycle.set_execution_lease_duration(Duration::from_secs(2));
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    lifecycle
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
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ _docID request_id agent_did lifecycle_state execution_generation execution_lease_expires_at terminal_output terminalized_at failure_reason }} }}"#,
        escape_graphql_string(doc_id),
    )).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    crate::graphql::first_row(&result, "AgentRequest")
        .unwrap()
        .unwrap()
}

async fn request_composite_version(node: &EmbeddedNode, doc_id: &str) -> String {
    let result = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ _version {{ cid height fieldName }} }} }}"#,
            escape_graphql_string(doc_id),
        ))
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let row = &result.data.as_ref().unwrap()["AgentRequest"][0];
    crate::graphql::document_composite_version(row, "AgentRequest begin replay")
        .unwrap()
        .expect("AgentRequest composite version")
        .cid
}

fn lease_tuple(row: &AgentRequestRow) -> (Option<String>, Option<String>) {
    (
        row.execution_generation.clone(),
        row.execution_lease_expires_at.clone(),
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
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_generation: "{}", execution_lease_expires_at: "{}" }}) {{ _docID }} }}"#,
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
async fn owned_stream_appends_do_not_renew_execution_lease() {
    let (node, _dir) = test_node().await;
    let mut lifecycle = claimed_owner(&node).await;
    let request = lifecycle.request().clone();
    let writer =
        crate::streaming::DefraStreamWriter::new(node.clone(), &request.agent_did, Duration::ZERO);
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let doc_id = lifecycle.request().doc_id.clone();
    let initial = lease_tuple(&request_row(&node, &request.doc_id).await);
    writer
        .start_provider_attempt(&doc_id, 0, 0, "inference.1".parse().unwrap())
        .await;
    assert!(writer
        .flush_native_partial(
            &lifecycle,
            &gents_protocol::message::Message::assistant("durable text"),
        )
        .await
        .unwrap());
    assert_eq!(
        lease_tuple(&request_row(&node, &request.doc_id).await),
        initial,
        "output flushes do not renew the execution lease"
    );
    drop(lifecycle);
}

#[tokio::test]
async fn renewal_timer_advances_durable_deadline_while_provider_and_tool_reads_wait() {
    use tokio::io::AsyncReadExt;

    let (node, _dir) = test_node().await;
    let mut lifecycle = claimed_owner_with_short_lease(&node).await;
    let writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        "did:test:execution-lease",
        Duration::ZERO,
    );
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let doc_id = lifecycle.request().doc_id.clone();

    // These stand in for an in-flight provider read and an independent tool
    // read. Neither produces bytes or touches the request's lease.
    let (mut provider_reader, _provider_writer) = tokio::io::duplex(64);
    let (mut tool_reader, _tool_writer) = tokio::io::duplex(64);
    let provider_read = tokio::spawn(async move {
        let mut byte = [0];
        provider_reader.read_exact(&mut byte).await
    });
    let tool_read = tokio::spawn(async move {
        let mut byte = [0];
        tool_reader.read_exact(&mut byte).await
    });
    tokio::task::yield_now().await;
    assert!(!provider_read.is_finished());
    assert!(!tool_read.is_finished());
    let before = request_row(&node, &doc_id).await;
    let generation = before.execution_generation.clone();
    let first_deadline =
        DateTime::parse_from_rfc3339(before.execution_lease_expires_at.as_deref().unwrap())
            .unwrap();

    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let row = request_row(&node, &doc_id).await;
            let deadline =
                DateTime::parse_from_rfc3339(row.execution_lease_expires_at.as_deref().unwrap())
                    .unwrap();
            if deadline > first_deadline {
                assert_eq!(row.lifecycle_state, Some(RequestLifecycleState::Processing));
                assert_eq!(row.execution_generation, generation);
                assert!(!provider_read.is_finished());
                assert!(!tool_read.is_finished());
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the actual renewal timer must advance the durable deadline while reads wait");

    provider_read.abort();
    tool_read.abort();
    drop(lifecycle);
}

#[tokio::test]
async fn renewal_after_a_simulated_scheduling_gap_cannot_revive_expired_owner() {
    let (node, _dir) = test_node().await;
    let mut lifecycle = claimed_owner_with_short_lease(&node).await;
    let writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        "did:test:execution-lease",
        Duration::ZERO,
    );
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let doc_id = lifecycle.request().doc_id.clone();
    let generation = request_row(&node, &doc_id)
        .await
        .execution_generation
        .expect("claimed generation");

    // Stop polling to simulate a scheduler gap. This is not an OS suspend or
    // wall-clock mutation: the stored deadline remains the authority.
    drop(lifecycle.renewal_task.take());
    let before = request_row(&node, &doc_id).await;
    let deadline =
        DateTime::parse_from_rfc3339(before.execution_lease_expires_at.as_deref().unwrap())
            .unwrap()
            .with_timezone(&Utc);
    let until_expired = (deadline - Utc::now() + chrono::Duration::milliseconds(50))
        .to_std()
        .unwrap_or(Duration::ZERO);
    tokio::time::sleep(until_expired).await;
    let observed_now = Utc::now();
    assert!(observed_now >= deadline);

    assert_eq!(
        crate::lifecycle::renew_execution_lease_once_at(
            &node,
            &doc_id,
            &generation,
            deadline,
            observed_now,
        )
        .await
        .unwrap(),
        crate::lifecycle::RenewalAttemptOutcome::Lost,
    );
    assert_eq!(
        lease_tuple(&request_row(&node, &doc_id).await),
        lease_tuple(&before)
    );
    assert_eq!(
        RequestLifecycle::recover_all(&node, "did:test:execution-lease")
            .await
            .unwrap()
            .requests_recovered,
        1,
    );
    let recovered = request_row(&node, &doc_id).await;
    assert_eq!(
        recovered.lifecycle_state,
        Some(RequestLifecycleState::Failed)
    );
    assert_ne!(
        recovered.execution_generation.as_deref(),
        Some(generation.as_str())
    );
    drop(lifecycle);
}

async fn output_segment_count(node: &EmbeddedNode, request_doc_id: &str) -> usize {
    let result = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(request_doc_id),
        ))
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    result.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .len()
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
    assert_eq!(output_segment_count(&node, &doc_id).await, 0);
}

#[tokio::test]
async fn racing_owned_begins_commit_one_processing_transition_without_renewal() {
    let (node, _dir) = test_node().await;
    let mut lifecycle = claimed_owner(&node).await;
    let before = request_row(&node, &lifecycle.request.doc_id).await;
    let mut competitor = RequestLifecycle::new_with_agent_did(
        node.clone(),
        "general",
        "did:test:execution-lease",
        lifecycle.request().clone(),
        60,
    );
    competitor.state = LocalLifecycleState::Claimed;
    competitor.execution_lease = lifecycle.execution_lease.clone();
    let first_writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        "did:test:execution-lease",
        Duration::ZERO,
    );
    let second_writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        "did:test:execution-lease",
        Duration::ZERO,
    );
    let (first, second) = tokio::join!(
        lifecycle.begin_owned_execution(&first_writer),
        competitor.begin_owned_execution(&second_writer),
    );
    assert!(first.is_ok(), "first={first:?}; second={second:?}");
    assert!(second.is_ok(), "first={first:?}; second={second:?}");
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
    assert_eq!(
        output_segment_count(&node, &lifecycle.request.doc_id).await,
        0
    );
    let committed_version = request_composite_version(&node, &lifecycle.request.doc_id).await;
    lifecycle
        .begin_owned_execution(&first_writer)
        .await
        .unwrap();
    assert_eq!(
        request_composite_version(&node, &lifecycle.request.doc_id).await,
        committed_version,
        "acknowledgement replay must not create another durable request version"
    );
    assert_eq!(
        lease_tuple(&request_row(&node, &lifecycle.request.doc_id).await),
        lease_tuple(&after),
        "acknowledgement replay must not renew the lease"
    );
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
    // The canonical recovery coordinator re-observes its fence inside the
    // transaction; verify that this exact observed generation was the winner.
    let agent = row
        .agent_did
        .as_deref()
        .context("observed request missing agent")?;
    let before_generation = row.execution_generation.as_deref();
    let report = RequestLifecycle::recover_all(node, agent).await?;
    let after = request_row(
        node,
        row.doc_id
            .as_deref()
            .context("observed request missing document")?,
    )
    .await;
    let recovered_exact_request = after
        .lifecycle_state
        .is_some_and(|state| state.is_terminal())
        && after.execution_generation.as_deref() != before_generation;
    Ok(
        if report.requests_recovered == 1 && recovered_exact_request {
            TerminalizeResult::Won
        } else {
            TerminalizeResult::Lost
        },
    )
}

async fn output_segment_snapshot(node: &EmbeddedNode, doc_id: &str) -> serde_json::Value {
    let result = node.execute(&format!(
        r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID ordinal payload close }} }}"#,
        escape_graphql_string(doc_id),
    )).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    result.data.as_ref().unwrap()["AgentOutputSegment"].clone()
}

#[tokio::test]
async fn revocation_preserves_latest_published_header_for_supersession_and_dead_child() {
    for outcome in [
        RequestTerminalOutcome::Superseded,
        RequestTerminalOutcome::Dead,
    ] {
        let (node, _dir) = test_node().await;
        let mut lifecycle = claimed_owner(&node).await;
        let writer = crate::streaming::DefraStreamWriter::new(
            node.clone(),
            "did:test:execution-lease",
            Duration::ZERO,
        );
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        let doc_id = lifecycle.request().doc_id.clone();
        let mut latest = None;
        for turn in 0..2 {
            writer
                .start_provider_attempt(&doc_id, turn, 0, "inference.1".parse().unwrap())
                .await;
            latest = Some(
                writer
                    .publish_native_turn(
                        &lifecycle,
                        turn,
                        0,
                        &gents_protocol::message::Message::assistant(format!(
                            "published turn {turn}"
                        )),
                    )
                    .await
                    .unwrap(),
            );
        }
        let segments_before = output_segment_snapshot(&node, &doc_id).await;
        let observed = observed_execution(&node, &doc_id).await;
        assert_eq!(
            revoke_execution_preserving_output(&node, &observed, outcome, "policy revocation",)
                .await
                .unwrap(),
            TerminalizeResult::Won
        );
        let terminal = request_row(&node, &doc_id).await;
        assert_eq!(terminal.lifecycle_state, Some(outcome.request_state()));
        assert_eq!(
            terminal.terminal_output,
            Some(TerminalOutput::Message {
                message_doc_id: latest.unwrap().message_doc_id,
            })
        );
        assert_ne!(terminal.execution_generation, observed.execution_generation);
        assert_eq!(
            output_segment_snapshot(&node, &doc_id).await,
            segments_before
        );
    }
}

#[tokio::test]
async fn expired_conflicting_source_is_revoked_without_rewriting_its_records() {
    use crate::session::canonical_rows::{
        decode_output_segment_row, output_segment_create_variables, AGENT_OUTPUT_SEGMENT_FIELDS,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    let (node, _dir) = test_node().await;
    let mut lifecycle = claimed_owner(&node).await;
    let writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        "did:test:execution-lease",
        Duration::ZERO,
    );
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let doc_id = lifecycle.request().doc_id.clone();
    writer
        .start_provider_attempt(&doc_id, 0, 0, "inference.1".parse().unwrap())
        .await;
    writer
        .flush_native_partial(
            &lifecycle,
            &gents_protocol::message::Message::assistant("retained bytes"),
        )
        .await
        .unwrap();
    let response = node.execute(&format!(
        "{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: \"{}\" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}",
        escape_graphql_string(&doc_id),
    )).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap();
    let mut twin = rows
        .iter()
        .map(|row| decode_output_segment_row(row).unwrap())
        .find(|row| {
            matches!(
                row.segment.source,
                gents_protocol::output::OutputSource::ProviderTurn { .. }
            )
        })
        .unwrap()
        .segment;
    twin.payload = "conflict bytes".into();
    let inserted = node
        .execute_request_with_retry(
            defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&twin).unwrap()),
            defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(!inserted.has_errors(), "{:?}", inserted.errors);
    let before = output_segment_snapshot(&node, &doc_id).await;
    let observed = expire_observed_execution(&node, &doc_id).await;
    let report = RequestLifecycle::recover_all(&node, "did:test:execution-lease")
        .await
        .unwrap();
    assert_eq!(report.requests_recovered, 1);
    let terminal = request_row(&node, &doc_id).await;
    assert_eq!(terminal.lifecycle_state, Some(RequestLifecycleState::Dead));
    assert_ne!(terminal.execution_generation, observed.execution_generation);
    assert_eq!(terminal.terminal_output, Some(TerminalOutput::NoMessage));
    assert_eq!(output_segment_snapshot(&node, &doc_id).await, before);
}

#[tokio::test]
async fn dead_revocation_accounts_tools_without_reading_corrupt_delivery_payload() {
    use crate::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables, AGENT_MESSAGE_FIELDS,
        AGENT_OUTPUT_SEGMENT_FIELDS, CREATE_AGENT_MESSAGE_MUTATION,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use crate::tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle};
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, ToolResultPart, TranscriptMessage,
    };

    let (node, _dir) = test_node().await;
    let mut lifecycle = claimed_owner(&node).await;
    let writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        "did:test:execution-lease",
        Duration::ZERO,
    );
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let request_doc_id = lifecycle.request().doc_id.clone();
    writer
        .start_provider_attempt(&request_doc_id, 0, 0, "inference.1".parse().unwrap())
        .await;
    let tool = |id: &str| {
        AssistantContent::ToolCall(ToolCall {
            id: id.into(),
            call_id: None,
            function: ToolFunction {
                name: "echo".into(),
                arguments: serde_json::json!({"value": id}),
            },
            signature: None,
            additional_params: None,
        })
    };
    let published = writer
        .publish_native_turn(
            &lifecycle,
            0,
            0,
            &Message::Assistant {
                id: None,
                content: vec![tool("pending-call"), tool("running-call")],
            },
        )
        .await
        .unwrap();
    let mut accepted = published.accepted_tools.into_iter();
    let pending = accepted.next().unwrap();
    let running = accepted.next().unwrap();
    let mut running_lifecycle = ToolCallLifecycle::from_accepted(
        node.clone(),
        lifecycle.request().agent_did.clone(),
        lifecycle.request().requester_did.clone(),
        running.clone(),
        Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
    )
    .unwrap();
    running_lifecycle.start_running().await.unwrap();

    // Persist a physically valid delivery header whose selected tool-output
    // extent is immutably corrupt. Metadata-only revocation must never read or
    // rewrite these bytes.
    let now = Utc::now().to_rfc3339();
    let payload = "durable-but-corrupt";
    let segment = OutputSegment {
        agent_did: lifecycle.request().agent_did.clone(),
        requester_did: lifecycle.request().requester_did.clone(),
        session_id: lifecycle.request().session_id.clone(),
        request_doc_id: request_doc_id.clone(),
        source: OutputSource::ToolCall {
            tool_call_doc_id: running.tool_call_doc_id.clone(),
        },
        writer: OutputWriter::ToolExecution {
            tool_call_doc_id: running.tool_call_doc_id.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: u32::try_from(payload.len()).unwrap(),
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolOutput,
            }),
        }],
        payload: payload.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![u64::try_from(payload.len()).unwrap() + 1],
        }),
        created_at: now.clone(),
    };
    let response = node
        .execute_request_with_retry(
            defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
            defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let close_doc_id =
        crate::graphql::single_mutation_document(&response, "create_AgentOutputSegment")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
    let delivery = TranscriptMessage {
        message_key: "corrupt-running-delivery".into(),
        session_id: lifecycle.request().session_id.clone(),
        agent_did: "did:test:execution-lease".into(),
        requester_did: None,
        request_doc_id: Some(request_doc_id.clone()),
        publication: MessagePublication::ToolDelivery {
            tool_call_doc_id: running.tool_call_doc_id.clone(),
        },
        outcome: OutputOutcome::Complete,
        sequence: published.sequence + 1,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![MessageBlock::ToolResult {
            tool_call_doc_id: running.tool_call_doc_id.clone(),
            id: running.id.clone(),
            call_id: running.call_id.clone(),
            parts: vec![ToolResultPart::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id,
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            }],
        }],
        created_at: now,
    };
    let response = node
        .execute_request_with_retry(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&delivery).unwrap()),
            defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let facts_query = format!(
        r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_MESSAGE_FIELDS} }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
        escape_graphql_string(&request_doc_id),
        escape_graphql_string(&request_doc_id),
    );
    let facts_before = node.execute(&facts_query).await;
    assert!(
        !facts_before.has_errors(),
        "load pre-revocation canonical facts: {:?}",
        facts_before.errors
    );
    let facts_before = facts_before.data.expect("pre-revocation fact data");
    let observed = observed_execution(&node, &request_doc_id).await;
    assert_eq!(
        revoke_execution_preserving_output(
            &node,
            &observed,
            RequestTerminalOutcome::Dead,
            "corrupt delivery",
        )
        .await
        .unwrap(),
        TerminalizeResult::Won
    );
    let facts_after = node.execute(&facts_query).await;
    assert!(
        !facts_after.has_errors(),
        "load post-revocation canonical facts: {:?}",
        facts_after.errors
    );
    let facts_after = facts_after.data.expect("post-revocation fact data");
    assert_eq!(
        facts_after, facts_before,
        "revocation must preserve corrupt facts"
    );

    let response = node.execute(&format!(
        r#"{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{}" }} }}, order: {{ message_sequence: ASC }}) {{ _docID lifecycle_state cancel_cause stuck_since tool_failure_class }} }}"#,
        escape_graphql_string(&request_doc_id),
    )).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response.data.as_ref().unwrap()["AgentToolCall"]
        .as_array()
        .unwrap();
    let row = |doc_id: &str| rows.iter().find(|row| row["_docID"] == doc_id).unwrap();
    assert_eq!(
        row(&pending.tool_call_doc_id)["lifecycle_state"],
        "cancelled"
    );
    assert_eq!(
        row(&pending.tool_call_doc_id)["cancel_cause"],
        "interrupted"
    );
    assert!(row(&pending.tool_call_doc_id)["tool_failure_class"].is_null());
    assert_eq!(row(&running.tool_call_doc_id)["lifecycle_state"], "running");
    assert!(row(&running.tool_call_doc_id)["stuck_since"].is_string());
    assert!(row(&running.tool_call_doc_id)["cancel_cause"].is_null());
    assert!(row(&running.tool_call_doc_id)["tool_failure_class"].is_null());
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
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let response_doc_id = lifecycle.request().doc_id.clone();
    writer
        .start_provider_attempt(&response_doc_id, 0, 0, "inference.1".parse().unwrap())
        .await;
    writer
        .flush_native_partial(
            &lifecycle,
            &gents_protocol::message::Message::assistant("preserved partial"),
        )
        .await
        .unwrap();
    let doc_id = lifecycle.request().doc_id.clone();
    let observed = observed_execution(&node, &doc_id).await;
    assert_eq!(
        revoke_execution_generation(
            &node,
            &observed,
            RequestTerminalOutcome::Superseded,
            TerminalOutput::NoMessage,
            "newer request"
        )
        .await
        .unwrap(),
        TerminalizeResult::Won
    );
    let terminal = request_row(&node, &doc_id).await;
    let response = output_segment_snapshot(&node, &doc_id).await;
    assert_ne!(terminal.execution_generation, observed.execution_generation);
    assert_eq!(
        terminal.lifecycle_state,
        Some(RequestLifecycleState::Superseded)
    );
    assert!(response.to_string().contains("preserved partial"));
    assert!(lifecycle.validate_owned_execution().await.is_err());
    assert!(writer
        .flush_native_partial(
            &lifecycle,
            &gents_protocol::message::Message::assistant("preserved partial stale overwrite"),
        )
        .await
        .is_err());
    assert_eq!(
        lifecycle
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                TerminalOutput::NoMessage,
                None
            )
            .await
            .unwrap(),
        TerminalizeResult::Lost
    );
    assert_eq!(
        lease_tuple(&request_row(&node, &doc_id).await),
        lease_tuple(&terminal)
    );
    assert_eq!(output_segment_snapshot(&node, &doc_id).await, response);
}

#[tokio::test]
async fn expired_owner_racing_recovery_cannot_win_terminalization() {
    let (node, _dir) = test_node().await;
    let mut lifecycle = owner(&node).await;
    let doc_id = lifecycle.request().doc_id.clone();
    let observed = expire_observed_execution(&node, &doc_id).await;
    let (owner_result, recovery_result) = tokio::join!(
        lifecycle.terminalize_owned(
            RequestTerminalOutcome::Completed,
            TerminalOutput::NoMessage,
            None
        ),
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
    assert_eq!(terminal.terminal_output, Some(TerminalOutput::NoMessage));
}

/// Simulates a host that made no progress while its *stored* deadline passed.
/// This is not an OS suspend or backward-clock test: the production gates read
/// a deliberately expired persisted deadline, then recovery takes the fence.
#[tokio::test]
async fn expired_stored_deadline_blocks_old_publication_and_tool_admission_after_recovery() {
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};

    let (node, _dir) = test_node().await;
    let lifecycle = owner(&node).await;
    let doc_id = lifecycle.request().doc_id.clone();
    let writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        "did:test:execution-lease",
        Duration::ZERO,
    );
    writer
        .start_provider_attempt(&doc_id, 0, 0, "inference.1".parse().unwrap())
        .await;
    let before = request_row(&node, &doc_id).await;
    let observed = expire_observed_execution(&node, &doc_id).await;
    assert_eq!(observed.execution_generation, before.execution_generation);
    assert_eq!(
        observed.lifecycle_state,
        Some(RequestLifecycleState::Processing)
    );

    let tool_turn = Message::Assistant {
        id: None,
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "stale-tool".into(),
            call_id: None,
            function: ToolFunction::new("echo".into(), serde_json::json!({"value":"stale"})),
            signature: None,
            additional_params: None,
        })],
    };
    assert!(writer
        .flush_native_partial(&lifecycle, &Message::assistant("stale bytes"))
        .await
        .is_err());
    assert!(writer
        .publish_native_turn(&lifecycle, 0, 0, &tool_turn)
        .await
        .is_err());
    assert_eq!(output_segment_count(&node, &doc_id).await, 0);

    assert_eq!(
        recover_observed_execution(&node, &observed).await.unwrap(),
        TerminalizeResult::Won
    );
    let recovered = request_row(&node, &doc_id).await;
    assert_eq!(
        recovered.lifecycle_state,
        Some(RequestLifecycleState::Failed)
    );
    assert_ne!(
        recovered.execution_generation,
        observed.execution_generation
    );
    assert!(writer
        .publish_native_turn(&lifecycle, 0, 0, &tool_turn)
        .await
        .is_err());
    assert_eq!(output_segment_count(&node, &doc_id).await, 0);
    assert_eq!(
        request_row(&node, &doc_id).await.execution_generation,
        recovered.execution_generation,
        "the stale owner must not replace recovery's terminal generation"
    );

    for collection in ["AgentMessage", "AgentToolCall"] {
        let query = format!(
            r#"{{ {collection}(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(&doc_id)
        );
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "{collection}: {:?}",
            response.errors
        );
        assert!(
            response.data.as_ref().unwrap()[collection]
                .as_array()
                .unwrap()
                .is_empty(),
            "an expired owner published a header or admitted a tool: {collection}"
        );
    }
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
        let response = output_segment_snapshot(&node, &doc_id).await;
        assert_eq!(
            terminal.lifecycle_state,
            Some(RequestLifecycleState::Failed)
        );
        assert_eq!(terminal.terminal_output, Some(TerminalOutput::NoMessage));
        assert_eq!(
            recover_observed_execution(&node, &observed).await.unwrap(),
            TerminalizeResult::Lost
        );
        assert_eq!(
            lease_tuple(&request_row(&node, &doc_id).await),
            lease_tuple(&terminal)
        );
        assert_eq!(output_segment_snapshot(&node, &doc_id).await, response);
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
                    .terminalize_owned(
                        RequestTerminalOutcome::Failed,
                        TerminalOutput::NoMessage,
                        Some("provider EOF without terminal event"),
                    )
                    .await
                    .unwrap()
            }
            1 => {
                let result = lifecycle
                    .terminalize_owned(
                        RequestTerminalOutcome::Failed,
                        TerminalOutput::NoMessage,
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
                    lifecycle.terminalize_owned(
                        RequestTerminalOutcome::Failed,
                        TerminalOutput::NoMessage,
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
        let response = output_segment_snapshot(&node, &doc_id).await;
        assert_eq!(terminal.terminal_output, Some(TerminalOutput::NoMessage));
        let replay = lifecycle
            .terminalize_owned(
                RequestTerminalOutcome::Failed,
                TerminalOutput::NoMessage,
                Some("replayed provider EOF"),
            )
            .await
            .unwrap();
        assert_ne!(replay, TerminalizeResult::Won);
        assert_eq!(output_segment_snapshot(&node, &doc_id).await, response);
        assert_eq!(
            request_row(&node, &doc_id).await.lifecycle_state,
            terminal.lifecycle_state
        );
    }
}

mod workspace_recovery;
