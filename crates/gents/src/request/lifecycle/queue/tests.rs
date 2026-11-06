use super::*;
use crate::request::lifecycle::{
    extract_single_doc_id, ClaimOutcome, RequestLifecycle, DEFAULT_REQUEST_MAX_RETRIES,
};
use crate::request::streaming::DefraStreamWriter;
use crate::store::identity::NodeIdentity;
use crate::tools::call::lifecycle::{AwaitMode, ToolCallLifecycle};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::{
    message::{AssistantContent, Message, ToolCall, ToolFunction},
    output::PresentationPart,
};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

mod background_goal_repair;
mod folding;
mod goal_assignment_wakes;
mod management;
mod steering;

struct TestDb {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    agent_identity: Arc<dyn NodeIdentity>,
    user: crate::KeyIdentity,
    user_access: crate::ConfigAccess,
    agent_access: crate::ConfigAccess,
    _db: crate::support::TestDb,
    _tempdir: TempDir,
}

impl TestDb {
    fn node_did(&self) -> &str {
        self.identity.did()
    }

    fn agent_did(&self) -> &str {
        self.agent_identity.did()
    }
}

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

fn hints(source: QueueSource, policy: QueuePolicy) -> RequestQueue {
    RequestQueue {
        delivery: Default::default(),
        position: None,
        source,
        policy,
        key: Some("session:sess-1".to_string()),
        queued_after_request_id: Some("req-1".to_string()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    }
}

fn wake_queue_input(queue: RequestQueue) -> RequestInput {
    RequestInput {
        queue: Some(queue),
        ..Default::default()
    }
}

fn parent_request(node_did: &str, agent_did: &str, session_id: &str) -> AgentRequest {
    AgentRequest {
        retry_parent_request_doc_id: None,
        purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        doc_id: "parent-doc".to_string(),
        request_id: "parent-request".to_string(),
        node_did: node_did.to_string(),
        requester_did: None,
        agent_did: agent_did.to_string(),
        session_id: session_id.to_string(),
        content: "parent".to_string(),
        max_total_tokens: None,
        input: gents_protocol::request_input::RequestInput::default(),
        execution_origin: Some("interactive".to_string()),
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        deadline: None,
        execution_generation: None,
        execution_lease_expires_at: None,
        execution_lease_secs: None,
        request_hop: 2,
        caused_by_parent_request_id: Some("root-parent-request".to_string()),
        caused_by_parent_request_doc_id: Some("root-parent-request-doc".to_string()),
        caused_by_parent_tool_call_id: Some("root-parent-tool-call".to_string()),
        caused_by_parent_tool_call_doc_id: Some("root-parent-tool-call-doc".to_string()),
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_owner_node_did: None,
        workspace_authority: None,
        workspace_seal_hash: None,
    }
}

#[test]
fn transaction_create_doc_id_accepts_both_defradb_response_shapes() {
    for response in [
        serde_json::json!({
            "data": { "create_AgentRequest": { "_docID": "doc-object" } }
        }),
        serde_json::json!({
            "data": { "add_AgentRequest": [{ "_docID": "doc-array" }] }
        }),
    ] {
        let doc_id = transaction_created_doc_id(&response, "AgentRequest").unwrap();
        assert!(doc_id == "doc-object" || doc_id == "doc-array");
    }
}

async fn test_db(name: &str) -> TestDb {
    let (db, user, user_access, binding) = crate::test_support::signed_test_node(name).await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    let agent_identity =
        crate::store::identity::resolve_agent_identity(identity.as_ref(), &binding)
            .expect("derived test agent identity");
    let agent_access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent_identity.did()).unwrap(),
        agent_identity.as_ref(),
    )
    .unwrap();
    TestDb {
        node,
        identity,
        agent_identity,
        user,
        user_access,
        agent_access,
        _db: db,
        _tempdir: tempfile::tempdir().expect("queue fixture directory"),
    }
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

async fn insert_raw_queue_request(
    db: &TestDb,
    request_id: &str,
    session_id: &str,
    input: &RequestInput,
) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_node_did = escape_graphql_string(db.node_did());
    let escaped_agent_did = escape_graphql_string(db.agent_did());
    let input_literal = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(input).expect("canonical input serializes"),
    )
    .expect("canonical input is a GraphQL literal");
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                purpose: "normal",
                node_did: "{escaped_node_did}",
                agent_did: "{escaped_agent_did}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "raw duplicate",
                input: {input_literal},
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "scheduled",
                failure_reason: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: {max_retries},
                request_hop: 0
            }}) {{ _docID }}
        }}"#,
        max_retries = DEFAULT_REQUEST_MAX_RETRIES,
    );
    let response = crate::store::config_client::ConfigAccess::write_local_response(
        &db.node,
        "test.insert_raw_queue_request",
        &mutation,
    )
    .await
    .unwrap();
    extract_single_doc_id(&response, "create_AgentRequest")
        .expect("raw queue create returns _docID")
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
    let mutation = super::mutation::session_request_create_mutation_at_hop_with_signer(
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
                let doc = crate::store::graphql::created_doc_id(&response, "AgentRequest")?;
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

async fn insert_signed_queue_request(
    db: &TestDb,
    request_id: &str,
    session_id: &str,
    input: &RequestInput,
) -> String {
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        request_id,
        db.node_did(),
        db.agent_did(),
        db.agent_did(),
        session_id,
        "work",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(db.agent_did()),
    );
    create.input = input.clone();
    publish_signed_queue_request(db, &mut create).await
}

async fn publish_signed_queue_request(
    db: &TestDb,
    create: &mut gents_protocol::request_admission::AgentRequestCreate,
) -> String {
    create.admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(db.agent_did());
    let access = crate::ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(db.agent_did()).unwrap(),
        db.agent_identity.as_ref(),
    )
    .unwrap();
    create.session_doc_id = Some(
        crate::request::lifecycle::client::reserve_goal_session(&access, create)
            .await
            .expect("reserve queue fixture session"),
    );
    let agent = escape_graphql_string(db.agent_did());
    let response = access
        .execute(&format!(
            r#"{{ Agent(filter: {{ agent_did: {{ _eq: "{agent}" }} }}, limit: 2) {{ _docID }} }}"#,
        ))
        .await
        .expect("query fixture agent");
    let [row] = response["data"]["Agent"].as_array().unwrap().as_slice() else {
        panic!("fixture agent must be unique");
    };
    create.agent_doc_id = Some(row["_docID"].as_str().unwrap().to_owned());
    crate::sign_agent_request_create(db.agent_identity.as_ref(), create)
        .await
        .unwrap();
    crate::request::lifecycle::client::submit_client_request(&access, create)
        .await
        .expect("publish queue fixture request");
    let request_id = escape_graphql_string(&create.request_id);
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 2) {{ _docID }} }}"#,
        ))
        .await
        .unwrap();
    let [row] = response["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("published queue request must be unique");
    };
    row["_docID"].as_str().unwrap().to_owned()
}

struct CanonicalBackgroundFixture {
    node: Arc<EmbeddedNode>,
    access: crate::ConfigAccess,
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
    let mut spec = crate::request::lifecycle::RequestSpec::new(
        gents_protocol::request_admission::RequestPurpose::Normal,
        crate::request::lifecycle::RequestIdentity {
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
    let create = crate::request::lifecycle::build_signed_client_request(
        &db.user_access,
        spec,
        crate::request::lifecycle::RequestSigner::Identity(&db.user),
    )
    .await
    .expect("sign canonical fixture parent");
    crate::request::lifecycle::submit_client_request(&db.user_access, &create)
        .await
        .expect("publish canonical fixture parent");
    let escaped = escape_graphql_string(&request_id);
    let response = db.agent_access.execute(&format!(
        r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{escaped}"}}}}, limit: 2) {{ {} }} }}"#,
        crate::SIGNED_REQUEST_FIELDS,
    )).await.expect("read canonical fixture parent");
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = row.try_into().expect("decode canonical fixture parent");
    let (_authority_owner, authority) =
        crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        db.node.clone(),
        db.identity.clone(),
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim(&request, db.agent_did())
        .await
        .expect("verify canonical fixture client admission");
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        db.node.clone(),
        db.agent_did(),
        db.node_did(),
        request,
        60,
    );
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
        anyhow::ensure!(
            queue.source == QueueSource::BackgroundCompletion
                && queue.policy == QueuePolicy::Coalesce
                && queue
                    .key
                    .as_deref()
                    .is_some_and(|key| !key.trim().is_empty()),
            "fixture notification requires a keyed coalescing background queue"
        );
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
                    crate::tools::native::SPAWN_PROCESS_TOOL_NAME.into(),
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
                crate::tools::call::lifecycle::SpawnedBackgroundToolAdmission {
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
        crate::tools::call::lifecycle::delivery::append_tool_output(&binding, notification_content)
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
            crate::request::lifecycle::RequestHopCause::Continuation,
        )
        .await
    }
}

#[tokio::test]
async fn request_doc_lookup_rejects_duplicate_logical_request_ids() {
    let db = test_db("ambiguous-request-doc-lookup").await;
    let input = wake_queue_input(hints(QueueSource::User, QueuePolicy::Append));
    insert_raw_queue_request(&db, "duplicate-logical-id", "session-a", &input).await;
    insert_raw_queue_request(&db, "duplicate-logical-id", "session-b", &input).await;

    let error = crate::request::binding::resolve_request_doc_id(&db.node, "duplicate-logical-id")
        .await
        .expect_err("duplicate logical ids must not resolve to an arbitrary document");
    assert!(error.to_string().contains("ambiguous across 2 documents"));
}

fn normalize_pin_fields(fields: &str) -> String {
    let start = fields
        .find("admission_signature: \"")
        .expect("admission signature field present");
    let rest = &fields[start + "admission_signature: \"".len()..];
    let end = rest.find('"').expect("signature value closes");
    let mut normalized = String::with_capacity(fields.len());
    normalized.push_str(&fields[..start + "admission_signature: \"".len()]);
    normalized.push_str("<SIGNATURE>");
    normalized.push_str(&rest[end..]);
    normalized
}

mod background_completion;
mod coalescing;
mod input;

mod pin_tests {
    use super::*;
    use crate::request::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};

    fn pin_parent_request() -> AgentRequest {
        let mut parent = parent_request(PIN_FIXED_DID, PIN_FIXED_DID, "sess-pin-parent");
        parent.request_id = "pin-parent-request".to_string();
        parent.doc_id = "pin-parent-doc".to_string();
        parent.request_hop = 2;
        parent.caused_by_correlation = Some("corr-parent".to_string());
        parent.caused_by_trigger_context = Some(r#"{"a":"b"}"#.to_string());
        parent
    }

    #[tokio::test]
    async fn pin_session_request_create_mutation() {
        let tempdir = tempfile::tempdir().unwrap();
        let _identity = pin_fixed_signing_identity(tempdir.path());

        let parent = pin_parent_request();
        let mutation = session_request_create_mutation(
            &parent,
            SessionRequestFields {
                agent_did: PIN_FIXED_DID,
                content: "steering content",
                execution_origin: ExecutionOrigin::Interactive,
                input: wake_queue_input(RequestQueue {
                    delivery: Default::default(),
                    position: None,
                    source: QueueSource::Steering,
                    policy: QueuePolicy::Append,
                    key: None,
                    queued_after_request_id: None,
                    interrupted_request_id: None,
                    background_completion_wake_version: None,
                }),
                request_id: "req-session-mutation-1",
                created_at: "2030-01-01T00:00:00Z",
                retry_key: Some("retry-key-session-1"),
            },
        )
        .await
        .expect("build session request create mutation");

        let fields = mutation
            .strip_prefix("mutation { create_AgentRequest(input: { ")
            .and_then(|rest| rest.strip_suffix(" }) { _docID } }"))
            .expect("mutation wraps create_AgentRequest input fields");

        let normalized = normalize_pin_fields(fields);
        assert_eq!(
            normalized,
            "request_id: \"req-session-mutation-1\", purpose: \"normal\", node_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", requester_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", agent_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", session_id: \"sess-pin-parent\", retry_root_request: \"req-session-mutation-1\", retry_key: \"retry-key-session-1\", content: \"steering content\", input: { queue: { policy: \"append\", source: \"steering\" } }, execution_origin: \"interactive\", caused_by_correlation: \"corr-parent\", caused_by_trigger_context: \"{\\\"a\\\":\\\"b\\\"}\", created_at: \"2030-01-01T00:00:00Z\", retry_count: 0, max_retries: 3, request_hop: 2, caused_by_parent_request_id: \"pin-parent-request\", caused_by_parent_request_doc_id: \"pin-parent-doc\", admission_kind: \"runtime-internal\", admission_signer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", admission_signature: \"<SIGNATURE>\", runtime_issuer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", runtime_source_request_id: \"pin-parent-request\", runtime_source_kind: \"local-control\", lifecycle_state: \"pending\", failure_reason: \"\""
        );
    }

    #[tokio::test]
    async fn runtime_controls_keep_the_parent_workspace_binding() {
        let tempdir = tempfile::tempdir().unwrap();
        let _identity = pin_fixed_signing_identity(tempdir.path());
        let mut parent = pin_parent_request();
        parent.workspace_id = Some("workspace".into());
        parent.workspace_owner_node_did = Some("did:key:workspace-owner".into());
        parent.workspace_authority = Some("readOnly".into());
        parent.workspace_seal_hash = Some("seal".into());
        for source in [QueueSource::BackgroundCompletion, QueueSource::Steering] {
            let mutation = session_request_create_mutation(
                &parent,
                SessionRequestFields {
                    agent_did: PIN_FIXED_DID,
                    content: "continue",
                    execution_origin: ExecutionOrigin::Scheduled,
                    input: wake_queue_input(hints(source, QueuePolicy::Append)),
                    request_id: "runtime-control",
                    created_at: "2030-01-01T00:00:00Z",
                    retry_key: None,
                },
            )
            .await
            .unwrap();
            for field in [
                "workspace_id: \"workspace\"",
                "workspace_owner_node_did: \"did:key:workspace-owner\"",
                "workspace_authority: \"readOnly\"",
                "workspace_seal_hash: \"seal\"",
            ] {
                assert!(mutation.contains(field), "{field}: {mutation}");
            }
        }
    }

    #[tokio::test]
    async fn pin_goal_continuation_preparation() {
        let tempdir = tempfile::tempdir().unwrap();
        let _identity = pin_fixed_signing_identity(tempdir.path());

        let mut parent = pin_parent_request();
        parent.workspace_id = Some("ws-goal-1".to_string());
        parent.workspace_owner_node_did = Some("did:key:workspace-owner".to_string());
        parent.workspace_authority = Some("readWrite".to_string());
        parent.workspace_seal_hash = Some("seal-goal-1".to_string());

        let goal_id = "goal-1";
        let continuation_sequence: i64 = 3;
        let content = "continue the goal";
        let agent_did = parent.agent_did.clone();

        let mut create = prepare_goal_continuation(
            &parent,
            agent_did,
            goal_id,
            content,
            continuation_sequence,
            false,
            "2030-01-01T00:00:00Z",
            parent.request_hop,
        )
        .expect("prepare goal continuation");
        crate::request::lifecycle::materialize::sign_request(
            &mut create,
            crate::request::lifecycle::materialize::RequestSigner::RegisteredTarget,
        )
        .await
        .expect("sign goal continuation request");

        let fields = create.graphql_input_fields().expect("graphql_input_fields");
        assert_eq!(
            normalize_pin_fields(&fields),
            "request_id: \"goal-cont-00000000000000000003-355ac0cefea9f3d0afab06ffb90fd1ec\", purpose: \"normal\", node_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", requester_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", agent_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", session_id: \"sess-pin-parent\", retry_root_request: \"goal-cont-00000000000000000003-355ac0cefea9f3d0afab06ffb90fd1ec\", retry_key: \"goal-continuation:355ac0cefea9f3d0afab06ffb90fd1ec\", content: \"continue the goal\", input: { goal_continuation: { sequence: 3, wrapup: false }, queue: { key: \"goal:355ac0cefea9f3d0afab06ffb90fd1ec\", policy: \"coalesce\", queued_after_request_id: \"pin-parent-request\", source: \"goal\" } }, execution_origin: \"scheduled\", caused_by_trigger_id: \"goal-1\", caused_by_trigger_kind: \"goal\", caused_by_correlation: \"corr-parent\", caused_by_trigger_context: \"{\\\"a\\\":\\\"b\\\"}\", created_at: \"2030-01-01T00:00:00Z\", retry_count: 0, max_retries: 3, request_hop: 2, caused_by_parent_request_id: \"pin-parent-request\", caused_by_parent_request_doc_id: \"pin-parent-doc\", workspace_id: \"ws-goal-1\", workspace_owner_node_did: \"did:key:workspace-owner\", workspace_authority: \"readWrite\", workspace_seal_hash: \"seal-goal-1\", admission_kind: \"runtime-internal\", admission_signer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", admission_signature: \"<SIGNATURE>\", runtime_issuer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", runtime_source_request_id: \"pin-parent-request\", runtime_source_kind: \"local-control\", lifecycle_state: \"pending\", failure_reason: \"\""
        );
    }
}
