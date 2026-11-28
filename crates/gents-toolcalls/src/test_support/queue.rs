use crate::call::lifecycle::ToolCallLifecycle;
use anyhow::Context;
use defra_node::Node as EmbeddedNode;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequest;
use gents_protocol::tool_call::AwaitMode;
use gents_protocol::{
    message::{AssistantContent, Message, ToolCall, ToolFunction},
    output::PresentationPart,
};
use gents_session::request::lifecycle::queue::mutation::SessionRequestFields;
use gents_session::request::lifecycle::queue::*;
use gents_session::request::lifecycle::ExecutionOrigin;
use gents_session::request::lifecycle::RequestLifecycle;
use gents_session::request::streaming::DefraStreamWriter;
use gents_session::test_support::AgentHome;
use gents_store::graphql::escape_graphql_string;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub struct QueueRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub request_id: String,
    pub session_id: String,
    pub agent_did: String,
    pub content: String,
    #[serde(default)]
    pub input: Option<RequestInput>,
    #[serde(default)]
    pub lifecycle_state: Option<RequestLifecycleState>,
    pub execution_origin: String,
    pub superseded_by_request: Option<String>,
    pub superseded_by_request_doc_id: Option<String>,
    pub request_hop: Option<u32>,
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_parent_request_doc_id: Option<String>,
    pub caused_by_parent_tool_call_id: Option<String>,
    pub caused_by_parent_tool_call_doc_id: Option<String>,
}

pub async fn queue_rows(db: &AgentHome, session_id: &str) -> Vec<QueueRow> {
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

pub async fn insert_runtime_queue_request(
    db: &AgentHome,
    parent: &AgentRequest,
    request_id: &str,
    content: &str,
    input: RequestInput,
) -> String {
    let signer = db.agent_identity.managed_agent_signer().unwrap();
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation = gents_session::request::lifecycle::queue::mutation::build_session_request_at_hop_with_signer(
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
    .unwrap().graphql_mutation().unwrap();
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

pub struct CanonicalBackgroundFixture {
    pub node: Arc<EmbeddedNode>,
    pub access: gents_store::config_client::ConfigAccess,
    pub parent: AgentRequest,
    pub lifecycle: RequestLifecycle,
    pub writer: DefraStreamWriter,
    pub next_turn: usize,
    /// Read before the parent's claim, so it precedes the accepted deadline
    /// every fixture tool inherits from that claim.
    pub claimed_at: chrono::DateTime<chrono::Utc>,
}

pub async fn canonical_background_fixture(
    db: &AgentHome,
    session_id: &str,
) -> CanonicalBackgroundFixture {
    canonical_background_fixture_with_input(db, session_id, &RequestInput::default()).await
}

pub async fn canonical_background_fixture_with_input(
    db: &AgentHome,
    session_id: &str,
    input: &RequestInput,
) -> CanonicalBackgroundFixture {
    let request_id = format!("background-parent-{}", uuid::Uuid::new_v4());
    let claimed_at = chrono::Utc::now();
    let claimed = gents_session::test_support::ClaimedRequest::client_signed(db, &db.binding)
        .ids(&request_id, session_id)
        .content("work")
        .input(input.clone())
        .batch_interval(Duration::ZERO)
        .drive(gents_session::test_support::Stage::Executing)
        .await;
    let writer = claimed.writer.expect("an executing request has its writer");
    let lifecycle = claimed.lifecycle;
    CanonicalBackgroundFixture {
        node: db.node.clone(),
        access: db.agent_access.clone(),
        parent: lifecycle.request().clone(),
        lifecycle,
        writer,
        next_turn: 0,
        claimed_at,
    }
}

impl CanonicalBackgroundFixture {
    pub async fn persist_notification(
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

    pub async fn prepare_notification(
        &mut self,
        notification_content: &str,
    ) -> anyhow::Result<ToolNotificationPublication> {
        self.prepare_notification_at(notification_content, self.claimed_at)
            .await
    }

    pub async fn prepare_notification_at(
        &mut self,
        notification_content: &str,
        wall_observation: chrono::DateTime<chrono::Utc>,
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
                crate::call::lifecycle::SpawnedBackgroundToolAdmission {
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
        if wall_observation > deadline {
            use crate::call::lifecycle::delivery::{
                append_tool_output_at, ToolOutputAppendRejection,
            };
            let refusal = append_tool_output_at(
                &binding,
                notification_content,
                self.claimed_at,
                wall_observation,
            )
            .await
            .expect_err("an observation past the accepted deadline");
            assert!(
                matches!(
                    ToolOutputAppendRejection::in_failure(&refusal),
                    Some(ToolOutputAppendRejection::DeadlineExceeded)
                ),
                "{refusal:#}"
            );
        }
        // The append checks its observation time against the accepted deadline,
        // which the claim set from the wall clock. Observing it at the claim
        // instant keeps fixture setup time out of the deadline the test
        // does not exercise.
        crate::call::lifecycle::delivery::append_tool_output_at(
            &binding,
            notification_content,
            self.claimed_at,
            self.claimed_at,
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

    pub async fn publish_prepared_notification(
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
            gents_session::request::lifecycle::queue::BackgroundCompletionNotification {
                content: notification_content,
                message_key,
                wake_content,
                queue,
                existing_notification_doc_id,
                wake: gents_protocol::request_lifecycle::RequestHopCause::Continuation,
            },
            publication,
        )
        .await
    }
}

pub mod background_completion;
pub mod background_goal_repair;
