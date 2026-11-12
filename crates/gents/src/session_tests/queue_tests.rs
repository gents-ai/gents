use anyhow::Context;
use defra_node::Node as EmbeddedNode;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequest;
use gents_protocol::tool_call::AwaitMode;
use gents_protocol::{
    message::{AssistantContent, Message, ToolCall, ToolFunction},
    output::PresentationPart,
};
use gents_session::request::lifecycle::queue::mutation::{
    session_request_create_mutation, SessionRequestFields,
};
use gents_session::request::lifecycle::queue::*;
use gents_session::request::lifecycle::test_support::{hints, parent_request, wake_queue_input};
use gents_session::request::lifecycle::test_support::{test_db, TestDb};
use gents_session::request::lifecycle::ExecutionOrigin;
use gents_session::request::lifecycle::{
    extract_single_doc_id, ClaimOutcome, RequestLifecycle, DEFAULT_REQUEST_MAX_RETRIES,
};
use gents_session::request::streaming::DefraStreamWriter;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_toolcalls::call::lifecycle::ToolCallLifecycle;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

#[derive(Debug, Deserialize)]
struct QueueRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    session_id: String,
    agent_did: String,
    content: String,
    #[serde(default)]
    input: Option<RequestInput>,
    #[serde(default)]
    lifecycle_state: Option<RequestLifecycleState>,
    execution_origin: String,
    superseded_by_request: Option<String>,
    superseded_by_request_doc_id: Option<String>,
    request_hop: Option<u32>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_request_doc_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_parent_tool_call_doc_id: Option<String>,
}

async fn queue_rows(db: &TestDb, session_id: &str) -> Vec<QueueRow> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
            ) {{
                _docID
                request_id
                session_id
                agent_did
                content
                input
                lifecycle_state
                execution_origin
                superseded_by_request
                superseded_by_request_doc_id
                request_hop
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
            }}
        }}"#
    );
    let response = db
        .agent_access
        .execute(&query)
        .await
        .expect("queue row query");
    response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

async fn insert_runtime_queue_request(
    db: &TestDb,
    parent: &AgentRequest,
    request_id: &str,
    content: &str,
    input: RequestInput,
) -> String {
    let signer = db.agent_identity.managed_agent_signer().unwrap();
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation = gents_session::request::lifecycle::queue::mutation::session_request_create_mutation_at_hop_with_signer(
        parent,
        parent.request_hop,
        SessionRequestFields {
            agent_did: db.agent_did(),
            content,
            execution_origin: ExecutionOrigin::Scheduled,
            input,
            request_id,
            created_at: &created_at,
            retry_key: None,
        },
        Some(&signer),
    )
    .await
    .unwrap();
    let agent = escape_graphql_string(db.agent_did());
    let session = escape_graphql_string(&parent.session_id);
    let node = escape_graphql_string(db.node_did());
    let targets = db.agent_access.execute(&format!(
        r#"{{ Agent(filter: {{agent_did: {{_eq: "{agent}"}}}}, limit: 1) {{_docID}} AgentSession(filter: {{node_did: {{_eq: "{node}"}}, session_id: {{_eq: "{session}"}}}}, limit: 1) {{_docID}} }}"#,
    )).await.unwrap();
    let agent_doc = targets["data"]["Agent"][0]["_docID"].as_str().unwrap();
    let session_doc = targets["data"]["AgentSession"][0]["_docID"]
        .as_str()
        .unwrap();
    let actor = identity::Did::new(db.agent_did()).unwrap();
    db.agent_access
        .transact_write("test.runtime_queue_fixture", |txn| {
            let mutation = &mutation;
            let actor = &actor;
            Box::pin(async move {
                let response = txn.execute(mutation).await?;
                let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
                txn.add_actor_relationship("AgentRequest", &doc, "working_agent", actor)
                    .await?;
                txn.add_resource_relationship("AgentRequest", &doc, "parent", "agents", agent_doc)
                    .await?;
                txn.add_resource_relationship(
                    "AgentRequest",
                    &doc,
                    "session",
                    "sessions",
                    session_doc,
                )
                .await?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await
        .unwrap()
}

struct CanonicalBackgroundFixture {
    node: Arc<EmbeddedNode>,
    access: gents_store::config_client::ConfigAccess,
    parent: AgentRequest,
    lifecycle: RequestLifecycle,
    writer: DefraStreamWriter,
    next_turn: usize,
}

async fn canonical_background_fixture(db: &TestDb, session_id: &str) -> CanonicalBackgroundFixture {
    canonical_background_fixture_with_input(db, session_id, &RequestInput::default()).await
}

async fn canonical_background_fixture_with_input(
    db: &TestDb,
    session_id: &str,
    input: &RequestInput,
) -> CanonicalBackgroundFixture {
    let request_id = format!("background-parent-{}", uuid::Uuid::new_v4());
    let mut spec = gents_session::request::lifecycle::RequestSpec::new(
        gents_protocol::request_admission::RequestPurpose::Normal,
        gents_session::request::lifecycle::RequestIdentity {
            request_id: request_id.clone(),
            node_did: db.node_did().to_owned(),
            requester_did: Some(db.user.did().to_owned()),
            agent_did: db.agent_did().to_owned(),
            session_id: session_id.to_owned(),
            content: "work".to_owned(),
            execution_origin: ExecutionOrigin::Interactive,
            created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        },
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(db.user.did()),
    );
    spec.input = input.clone();
    let create = gents_session::request::lifecycle::build_signed_client_request(
        &db.user_access,
        spec,
        gents_session::request::lifecycle::RequestSigner::Identity(&db.user),
    )
    .await
    .expect("sign canonical fixture parent");
    gents_session::request::lifecycle::submit_client_request(&db.user_access, &create)
        .await
        .expect("publish canonical fixture parent");
    let escaped = escape_graphql_string(&request_id);
    let response = db.agent_access.execute(&format!(
        r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{escaped}"}}}}, limit: 2) {{ {} }} }}"#,
        gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
    )).await.expect("read canonical fixture parent");
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = row.try_into().expect("decode canonical fixture parent");
    let (_authority_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    let verifier = gents_session::request::admission::AgentRequestAdmissionVerifier::new(
        db.node.clone(),
        db.identity.clone(),
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(&request, db.agent_identity.as_ref())
        .await
        .expect("verify canonical fixture client admission");
    assert!(
        admission.is_some(),
        "fixture must retain client claim admission"
    );
    let mut lifecycle = RequestLifecycle::new_with_node_did(db.node.clone(), request, 60);
    lifecycle
        .bind_managed_agent(db.agent_identity.as_ref())
        .unwrap();
    lifecycle.set_client_claim_admission(admission);
    assert_eq!(
        lifecycle
            .claim_with_identity()
            .await
            .expect("claim canonical fixture parent"),
        ClaimOutcome::Claimed
    );
    let writer =
        DefraStreamWriter::new_managed(db.node.clone(), db.agent_identity.as_ref(), Duration::ZERO)
            .unwrap();
    lifecycle
        .begin_owned_execution(&writer)
        .await
        .expect("begin canonical fixture parent");
    CanonicalBackgroundFixture {
        node: db.node.clone(),
        access: db.agent_access.clone(),
        parent: lifecycle.request().clone(),
        lifecycle,
        writer,
        next_turn: 0,
    }
}

impl CanonicalBackgroundFixture {
    async fn persist_notification(
        &mut self,
        notification_content: &str,
        message_key: &str,
        wake_content: &str,
        queue: RequestQueue,
        existing_notification_doc_id: Option<&str>,
    ) -> anyhow::Result<EnqueuedBackgroundCompletionInput> {
        let publication = self.prepare_notification(notification_content).await?;
        self.publish_prepared_notification(
            notification_content,
            message_key,
            wake_content,
            queue,
            existing_notification_doc_id,
            &publication,
        )
        .await
    }

    async fn prepare_notification(
        &mut self,
        notification_content: &str,
    ) -> anyhow::Result<ToolNotificationPublication> {
        let turn = self.next_turn;
        self.next_turn += 1;
        self.writer
            .start_provider_attempt(
                &self.parent.doc_id,
                turn,
                0,
                format!("inference.{}", turn + 1).parse()?,
            )
            .await;
        let native_id = format!("fixture-native-tool-{turn}");
        let message = Message::Assistant {
            request_id: None,
            id: Some(format!("fixture-provider-message-{turn}")),
            content: vec![AssistantContent::ToolCall(ToolCall {
                raw_arguments: None,
                id: native_id.clone(),
                call_id: Some(format!("fixture-provider-call-{turn}")),
                function: ToolFunction::new(
                    gents_protocol::tool_names::SPAWN_PROCESS_TOOL_NAME.into(),
                    serde_json::json!({"tool_name": "fixture", "args": {}}),
                ),
                signature: None,
                additional_params: None,
            })],
        };
        let mut accepted = self
            .writer
            .publish_native_turn(&self.lifecycle, turn, 0, &message)
            .await?
            .accepted_tools;
        anyhow::ensure!(accepted.len() == 1, "fixture expected one accepted tool");
        let deadline = self
            .lifecycle
            .claimed_deadline_at()
            .context("fixture parent claim has no deadline")?;
        let mut tool = ToolCallLifecycle::from_accepted(
            self.node.clone(),
            self.parent.node_did.clone(),
            self.parent.requester_did.clone(),
            accepted.pop().expect("one accepted tool"),
            deadline,
            AwaitMode::Foreground,
        )?;
        tool.start_running().await?;
        let mut tool = tool
            .admit_spawned_background(
                gents_toolcalls::call::lifecycle::SpawnedBackgroundToolAdmission {
                    tool_name: "fixture".to_owned(),
                    deadline_at: deadline,
                    selected_tool_identity: None,
                },
                "background work started",
            )
            .await?;
        tool.start_running().await?;
        let binding = tool
            .tool_output_binding()
            .context("accepted fixture tool has output binding")?;
        gents_toolcalls::call::lifecycle::delivery::append_tool_output(
            &binding,
            notification_content,
        )
        .await?;
        anyhow::ensure!(
            tool.complete_owned(notification_content, None).await?,
            "fixture tool terminal CAS lost"
        );
        let tool_call_doc_id = tool
            .doc_id()
            .context("fixture tool has physical document")?;
        Ok(ToolNotificationPublication {
            tool_call_doc_id: tool_call_doc_id.to_owned(),
            presentation: vec![PresentationPart::OutputRange {
                start_byte: 0,
                end_byte: notification_content.len() as u64,
            }],
        })
    }

    async fn publish_prepared_notification(
        &self,
        notification_content: &str,
        message_key: &str,
        wake_content: &str,
        queue: RequestQueue,
        existing_notification_doc_id: Option<&str>,
        publication: &ToolNotificationPublication,
    ) -> anyhow::Result<EnqueuedBackgroundCompletionInput> {
        persist_background_completion_with_message_canonical_with_access(
            &self.access,
            &self.node,
            &self.parent,
            notification_content,
            message_key,
            wake_content,
            queue,
            existing_notification_doc_id,
            publication,
            gents_protocol::request_lifecycle::RequestHopCause::Continuation,
        )
        .await
    }
}

mod background_goal_repair;

mod background_completion;

mod coalescing;
