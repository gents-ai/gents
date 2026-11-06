use std::{path::PathBuf, sync::Arc, time::Duration};

use defra_node::Node as EmbeddedNode;

use crate::request::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::request::streaming::DefraStreamWriter;
use crate::store::identity::NodeIdentity;
use crate::tools::call::lifecycle::{AwaitMode, ToolCallLifecycle};

pub struct PublishedAdmissionOptions {
    pub name: String,
    pub real_identity: bool,
    pub await_mode: AwaitMode,
    pub start_running: bool,
    pub request_created_at: Option<String>,
    pub tool_name: Option<String>,
    pub paired_client: bool,
    /// Publish the row with a fixture `JobPin`, so it loads as
    /// `ToolRowKind::PluginJob`.
    pub job_pin: bool,
    pub p2p: bool,
}

impl Default for PublishedAdmissionOptions {
    fn default() -> Self {
        Self {
            name: String::new(),
            real_identity: false,
            await_mode: AwaitMode::Foreground,
            start_running: true,
            request_created_at: None,
            tool_name: None,
            paired_client: false,
            job_pin: false,
            p2p: false,
        }
    }
}

/// The `JobPin` every `job_pin: true` fixture row carries.
pub(crate) fn fixture_job_pin() -> gents_protocol::job::JobPin {
    gents_protocol::job::JobPin {
        script_id: "fixture-script".to_owned(),
        source_digest: "sha256:fixture-source".to_owned(),
        origin: gents_protocol::job::JobOrigin::Installed,
        engine_digest: "sha256:fixture-engine".to_owned(),
        plugin_ref: "fixture-plugin".to_owned(),
        agent_budget: 4,
        handle: "fixture-handle".to_owned(),
        cwd: "/fixture/cwd".to_owned(),
        adopted_at: 1_700_000_000_000,
        args_digest: "sha256:fixture-args".to_owned(),
        chain_root_doc_id: "fixture-chain-root".to_owned(),
        resume_of: None,
        replayed_from: None,
        ceiling_at_pin: gents_protocol::job::JobCeiling {
            targets: vec!["general".to_owned()],
            max_live: 2,
            max_spawns: 8,
            max_messages_per_slot: 4,
            result_bytes: 65_536,
            max_wall_secs: 600,
        },
    }
}

/// Write [`fixture_job_pin`] onto the tool-call row `tool_doc_id`.
pub(crate) async fn write_fixture_job_pin(
    node: &Arc<EmbeddedNode>,
    tool_doc_id: &str,
) -> anyhow::Result<()> {
    let mutation = format!(
        r#"mutation($job_pin: JSON) {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ job_pin: $job_pin }}) {{ _docID }} }}"#,
        crate::store::graphql::escape_graphql_string(tool_doc_id),
    );
    let variables = serde_json::json!({ "job_pin": fixture_job_pin() });
    crate::store::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "test.admission_fixture.job_pin",
        |txn| {
            let (mutation, variables) = (&mutation, &variables);
            Box::pin(async move { txn.execute_with_variables(mutation, variables).await })
        },
    )
    .await?;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "shared fixture with call sites across many test modules"
)]
pub(crate) async fn publish_accepted_on_claimed_request(
    node: Arc<EmbeddedNode>,
    request: &mut RequestLifecycle,
    node_did: &str,
    turn: usize,
    tool_name: &str,
    tool_call_id: &str,
    arguments: serde_json::Value,
    await_mode: AwaitMode,
    start_running: bool,
) -> anyhow::Result<ToolCallLifecycle> {
    let writer =
        DefraStreamWriter::new_for_lifecycle(node.clone(), request, Duration::from_millis(1))?;
    if turn == 0 {
        request.begin_owned_execution(&writer).await?;
    } else {
        request.validate_owned_execution().await?;
    }
    writer
        .start_provider_attempt(
            &request.request().doc_id,
            turn,
            0,
            format!("inference.{}", turn + 1).parse().unwrap(),
        )
        .await;
    let message = gents_protocol::message::Message::Assistant {
        request_id: None,
        id: Some(format!("fixture-provider-message-{turn}")),
        content: vec![gents_protocol::message::AssistantContent::ToolCall(
            gents_protocol::message::ToolCall {
                raw_arguments: None,
                id: tool_call_id.to_owned(),
                call_id: Some(format!("fixture-provider-call-{turn}")),
                function: gents_protocol::message::ToolFunction::new(
                    tool_name.to_owned(),
                    arguments,
                ),
                signature: None,
                additional_params: None,
            },
        )],
    };
    let mut published = writer
        .publish_native_turn(request, turn, 0, &message)
        .await?;
    anyhow::ensure!(
        published.accepted_tools.len() == 1,
        "fixture publication must accept exactly one tool call"
    );
    let accepted = published.accepted_tools.pop().unwrap();
    let deadline = request
        .claimed_deadline_at()
        .ok_or_else(|| anyhow::anyhow!("claimed fixture request has no deadline"))?;
    let mut tool = ToolCallLifecycle::from_accepted(
        node,
        node_did.to_owned(),
        request.request().requester_did.clone(),
        accepted,
        deadline,
        await_mode,
    )?;
    if start_running {
        tool.start_running().await?;
    }
    Ok(tool)
}

pub struct PublishedAdmission {
    pub node: Arc<EmbeddedNode>,
    pub path: PathBuf,
    pub tool: ToolCallLifecycle,
    pub node_did: String,
}

pub(crate) async fn complete_child_managed(
    node: &Arc<EmbeddedNode>,
    child_id: &str,
    node_did: &str,
    text: &str,
    identity: &dyn NodeIdentity,
) {
    complete_child_managed_inner(node, child_id, node_did, text, identity, None).await;
}

pub(crate) async fn complete_child_managed_verified(
    node: &Arc<EmbeddedNode>,
    child_id: &str,
    node_did: &str,
    text: &str,
    identity: &dyn NodeIdentity,
    runtime: Arc<dyn NodeIdentity>,
) {
    complete_child_managed_inner(node, child_id, node_did, text, identity, Some(runtime)).await;
}

async fn complete_child_managed_inner(
    node: &Arc<EmbeddedNode>,
    child_id: &str,
    node_did: &str,
    text: &str,
    identity: &dyn NodeIdentity,
    runtime: Option<Arc<dyn NodeIdentity>>,
) {
    let child_id_escaped = crate::store::graphql::escape_graphql_string(child_id);
    let access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(identity.did()).unwrap(),
        identity,
    )
    .unwrap();
    let response = access.execute(&format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child_id_escaped}" }} }}) {{ {} }} }}"#,
        crate::request::watcher::AGENT_REQUEST_FIELDS,
    )).await.unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let selected_agent = row
        .agent_did
        .clone()
        .expect("fixture request selects an agent");
    let request = row.try_into().unwrap();
    let (request, admission) = if let Some(runtime) = runtime {
        let (_, authority) = crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
        let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            runtime,
            authority,
        );
        verifier
            .verify_fresh_for_claim_with_identity(&request, identity)
            .await
            .expect("verify the child request and its native claim admission")
    } else {
        (request, None)
    };
    let mut request =
        RequestLifecycle::new_with_node_did(node.clone(), &selected_agent, node_did, request, 60);
    request.bind_managed_agent(identity).unwrap();
    request.set_client_claim_admission(admission);
    assert_eq!(request.claim().await.unwrap(), ClaimOutcome::Claimed);
    let writer =
        DefraStreamWriter::new_for_lifecycle(node.clone(), &request, Duration::ZERO).unwrap();
    request.begin_owned_execution(&writer).await.unwrap();
    writer
        .start_provider_attempt(
            &request.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    let message = gents_protocol::message::Message::Assistant {
        request_id: None,
        id: Some(format!("answer-{child_id}")),
        content: vec![gents_protocol::message::AssistantContent::Text(
            gents_protocol::message::Text {
                text: text.to_owned(),
            },
        )],
    };
    let published = writer
        .publish_native_turn(&request, 0, 0, &message)
        .await
        .unwrap();
    let terminal = request
        .terminalize_owned(
            crate::request::lifecycle::RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::Message {
                message_doc_id: published.message_doc_id,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(terminal, crate::request::lifecycle::TerminalizeResult::Won);
}

pub(crate) async fn ensure_fixture_session(
    node: &Arc<EmbeddedNode>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    created_at: &str,
) -> String {
    let session = crate::store::graphql::escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{session}" }} }}, limit: 2) {{ _docID node_did requester_did agent_did }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    let rows = response.data.unwrap()["AgentSession"]
        .as_array()
        .unwrap()
        .clone();
    assert!(rows.len() <= 1, "fixture session identity must be unique");
    if let Some(row) = rows.first() {
        assert_eq!(row["node_did"].as_str(), Some(node_did));
        assert_eq!(
            row["requester_did"].as_str(),
            if requester_did == Some(node_did) {
                row["agent_did"].as_str()
            } else {
                requester_did
            }
        );
        return row["agent_did"].as_str().unwrap().to_owned();
    }
    let root = crate::store::identity::RegisteredIdentity::from_registered_did(
        node.node_identity_did().expect("fixture node has a signer"),
        None,
    )
    .unwrap();
    let binding = crate::store::identity::provision_agent_identity(&root).unwrap();
    let agent_did = crate::store::graphql::escape_graphql_string(&binding.agent_did);
    let agent = crate::store::graphql::escape_graphql_string(node_did);
    let requester_did = if requester_did == Some(node_did) {
        Some(binding.agent_did.as_str())
    } else {
        requester_did
    };
    let requester = requester_did.map_or_else(
        || "null".to_owned(),
        |did| format!("\"{}\"", crate::store::graphql::escape_graphql_string(did)),
    );
    let created = crate::store::graphql::escape_graphql_string(created_at);
    let access = crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(binding.agent_did.as_str()).unwrap(),
    );
    access.write("fixture.create_session", &format!(r#"mutation {{ create_AgentSession(input: {{ session_id: "{session}", node_did: "{agent}", requester_did: {requester}, agent_did: "{agent_did}", created_at: "{created}" }}) {{ _docID }} }}"#)).await.expect("create agent-owned fixture session");
    binding.agent_did
}

async fn ensure_managed_fixture_session(
    node: &Arc<EmbeddedNode>,
    session_id: &str,
    root: &dyn NodeIdentity,
    requester_did: Option<&str>,
    created_at: &str,
) -> Arc<dyn NodeIdentity> {
    let session = crate::store::graphql::escape_graphql_string(session_id);
    let response = node.execute(&format!(r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{session}" }} }}, limit: 2) {{ _docID node_did requester_did agent_did }} }}"#)).await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    let rows = response.data.unwrap()["AgentSession"]
        .as_array()
        .unwrap()
        .clone();
    assert!(rows.len() <= 1, "fixture session identity must be unique");
    if let Some(row) = rows.first() {
        assert_eq!(row["node_did"].as_str(), Some(root.did()));
        let did = row["agent_did"].as_str().unwrap();
        let agent = crate::load_agent(node, did)
            .await
            .unwrap()
            .expect("fixture Agent binding");
        let binding = gents_protocol::agent_identity::AgentIdentityBinding {
            agent_did: agent.agent_did,
            creation_nonce: agent.creation_nonce,
        };
        return crate::store::identity::resolve_agent_identity(root, &binding).unwrap();
    }
    let binding = crate::store::identity::provision_agent_identity(root).unwrap();
    crate::test_support::install_test_agent_binding(
        node,
        root.did(),
        "admission fixture",
        &binding,
    )
    .await;
    let agent = crate::store::identity::resolve_agent_identity(root, &binding).unwrap();
    let access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let agent_did = crate::store::graphql::escape_graphql_string(agent.did());
    let node_did = crate::store::graphql::escape_graphql_string(root.did());
    let requester_did = if requester_did == Some(root.did()) {
        Some(agent.did())
    } else {
        requester_did
    };
    let requester = requester_did.map_or_else(
        || "null".to_owned(),
        |did| format!("\"{}\"", crate::store::graphql::escape_graphql_string(did)),
    );
    let created = crate::store::graphql::escape_graphql_string(created_at);
    access.write("fixture.create_managed_session", &format!(r#"mutation {{ create_AgentSession(input: {{ session_id: "{session}", node_did: "{node_did}", requester_did: {requester}, agent_did: "{agent_did}", created_at: "{created}" }}) {{ _docID }} }}"#)).await.expect("create managed fixture session");
    agent
}

pub async fn claimed_request(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    node_did: &str,
) -> RequestLifecycle {
    let now = chrono::Utc::now().to_rfc3339();
    let agent_did = ensure_fixture_session(node, session_id, node_did, None, &now).await;
    let agent_did = crate::store::graphql::escape_graphql_string(&agent_did);
    let now = crate::store::graphql::escape_graphql_string(&now);
    let request_id = crate::store::graphql::escape_graphql_string(request_id);
    let session_id = crate::store::graphql::escape_graphql_string(session_id);
    let node_did = crate::store::graphql::escape_graphql_string(node_did);
    let created = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request_id}", purpose: "normal", node_did: "{node_did}", agent_did: "{agent_did}", session_id: "{session_id}", retry_parent_request: "", retry_root_request: "{request_id}", superseded_by_request: "", content: "spawn", lifecycle_state: "pending", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{now}", retry_count: 0, max_retries: 3, request_hop: 0 }}) {{ _docID }} }}"#)).await;
    assert!(!created.has_errors(), "{:#?}", created.errors);
    let row = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {} }} }}"#,
            crate::request::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await;
    let row: gents_protocol::row::AgentRequestRow =
        crate::store::graphql::first_row(&row, "AgentRequest")
            .unwrap()
            .unwrap();
    let selected_agent = row
        .agent_did
        .clone()
        .expect("fixture request selects an agent");
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        node.clone(),
        &selected_agent,
        &node_did,
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
}

pub(crate) async fn claimed_signed_request(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    identity: &dyn NodeIdentity,
    created_at: Option<&str>,
) -> RequestLifecycle {
    claimed_signed_request_with_trigger(node, request_id, session_id, identity, created_at, None)
        .await
}

pub(crate) async fn claimed_signed_request_with_trigger(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    identity: &dyn NodeIdentity,
    created_at: Option<&str>,
    trigger_id: Option<&str>,
) -> RequestLifecycle {
    claimed_signed_request_with_trigger_correlation(
        node, request_id, session_id, identity, created_at, trigger_id, None,
    )
    .await
}

pub(crate) async fn claimed_signed_request_with_trigger_correlation(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    identity: &dyn NodeIdentity,
    created_at: Option<&str>,
    trigger_id: Option<&str>,
    correlation: Option<&str>,
) -> RequestLifecycle {
    let now = created_at
        .map(str::to_owned)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        identity.did(),
        identity.did(),
        identity.did(),
        session_id,
        "spawn",
        "interactive",
        now,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    if let Some(trigger_id) = trigger_id {
        create.caused_by_trigger_id = Some(trigger_id.to_owned());
        create.caused_by_trigger_kind = Some("schedule".to_owned());
    }
    create.caused_by_correlation = correlation.map(str::to_owned);
    claimed_signed_request_create(node, identity, create).await
}

pub(crate) async fn claimed_signed_request_create(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    mut create: gents_protocol::request_admission::AgentRequestCreate,
) -> RequestLifecycle {
    let agent_identity = ensure_managed_fixture_session(
        node,
        &create.session_id,
        identity,
        Some(&create.requester_did),
        &create.created_at,
    )
    .await;
    create.agent_did = agent_identity.did().to_owned();
    if create.admission.kind
        == gents_protocol::request_admission::AgentRequestAdmissionKind::LocalSelf
    {
        create.requester_did = create.agent_did.clone();
        create.admission =
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                &create.agent_did,
            );
        crate::sign_agent_request_create(agent_identity.as_ref(), &mut create)
            .await
            .expect("sign fixture request as selected agent");
    } else {
        crate::sign_agent_request_create(identity, &mut create)
            .await
            .expect("sign canonical admission fixture request");
    }
    let local_self = create.admission.kind
        == gents_protocol::request_admission::AgentRequestAdmissionKind::LocalSelf;
    let actor_access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(create.agent_did.as_str()).unwrap(),
        agent_identity.as_ref(),
    )
    .unwrap();
    if local_self {
        actor_access
            .write(
                "fixture.create_request",
                &create.graphql_mutation().unwrap(),
            )
            .await
            .expect("create agent-owned local request");
    } else {
        let created = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(!created.has_errors(), "{:#?}", created.errors);
    }
    let request_id = crate::store::graphql::escape_graphql_string(&create.request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {} }} }}"#,
        crate::request::watcher::AGENT_REQUEST_FIELDS,
    );
    let row: gents_protocol::row::AgentRequestRow = if local_self {
        let result = actor_access.execute(&query).await.unwrap();
        serde_json::from_value(result["data"]["AgentRequest"][0].clone()).unwrap()
    } else {
        let result = node.execute(&query).await;
        crate::store::graphql::first_row(&result, "AgentRequest")
            .unwrap()
            .unwrap()
    };
    let selected_agent = row
        .agent_did
        .clone()
        .expect("fixture request selects an agent");
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        node.clone(),
        &selected_agent,
        identity.did(),
        row.try_into().unwrap(),
        60,
    );
    lifecycle
        .bind_managed_agent(agent_identity.as_ref())
        .unwrap();
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
}

async fn claimed_paired_client_request(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    agent: &dyn NodeIdentity,
    path: &std::path::Path,
) -> RequestLifecycle {
    let client = crate::KeyIdentity::load_or_create(path.join("paired-client.key"), None)
        .expect("paired client identity");
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let agent_identity =
        ensure_managed_fixture_session(node, session_id, agent, Some(client.did()), &now).await;
    let agent_did = agent_identity.did();
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        agent.did(),
        client.did(),
        agent_did,
        session_id,
        "spawn",
        "interactive",
        now,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::enrollment(
            client.did(),
            "enrollment",
            "digest",
            agent.did(),
            1,
            "2099-01-01T00:00:00Z",
        ),
    );
    crate::sign_agent_request_create(&client, &mut create)
        .await
        .expect("sign paired client request");
    let created = node.execute(&create.graphql_mutation().unwrap()).await;
    assert!(!created.has_errors(), "{:#?}", created.errors);
    let request_id = crate::store::graphql::escape_graphql_string(request_id);
    let row = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {} }} }}"#,
            crate::request::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await;
    let row: gents_protocol::row::AgentRequestRow =
        crate::store::graphql::first_row(&row, "AgentRequest")
            .unwrap()
            .unwrap();
    let selected_agent = row
        .agent_did
        .clone()
        .expect("fixture request selects an agent");
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        node.clone(),
        &selected_agent,
        agent.did(),
        row.try_into().unwrap(),
        60,
    );
    lifecycle
        .bind_managed_agent(agent_identity.as_ref())
        .unwrap();
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
}

pub async fn published_admission(
    options: PublishedAdmissionOptions,
) -> anyhow::Result<PublishedAdmission> {
    let (admission, _owner) = published_admission_with_owner(options).await?;
    Ok(admission)
}

pub async fn published_admission_with_owner(
    options: PublishedAdmissionOptions,
) -> anyhow::Result<(PublishedAdmission, RequestLifecycle)> {
    let name = options.name;
    let path =
        std::env::temp_dir().join(format!("admission-fixture-{name}-{}", uuid::Uuid::new_v4()));
    let physical_identity = crate::KeyIdentity::load_or_create(path.join("test-agent.key"), None)?;
    let mut builder = EmbeddedNode::builder()
        .data_path(&path)
        .with_node_identity_did(physical_identity.did());
    if options.p2p {
        builder = builder.with_p2p(test_p2p_config());
    }
    let node = Arc::new(builder.build().await?);
    crate::store::schema::ensure_runtime_schemas(&node).await?;
    if options.real_identity {
        let root = crate::store::graphql::escape_graphql_string(physical_identity.did());
        crate::ConfigAccess::local(node.clone()).write(
            "fixture.create_runtime_node",
            &format!(r#"mutation {{ create_Node(input: {{ node_did: "{root}", display_name: "fixture", enabled: true }}) {{ _docID }} }}"#),
        ).await?;
    }
    let identity = options.real_identity.then_some(physical_identity);
    let node_did = identity.as_ref().map_or_else(
        || "did:test:test".to_owned(),
        |identity| identity.did().to_owned(),
    );
    let mut request = match identity.as_ref() {
        Some(identity) if options.paired_client => {
            claimed_paired_client_request(
                &node,
                &format!("request-{name}"),
                &format!("session-{name}"),
                identity,
                &path,
            )
            .await
        }
        Some(identity) => {
            claimed_signed_request(
                &node,
                &format!("request-{name}"),
                &format!("session-{name}"),
                identity,
                options.request_created_at.as_deref(),
            )
            .await
        }
        None => {
            claimed_request(
                &node,
                &format!("request-{name}"),
                &format!("session-{name}"),
                &node_did,
            )
            .await
        }
    };
    let writer =
        DefraStreamWriter::new_for_lifecycle(node.clone(), &request, Duration::from_millis(1))?;
    request.begin_owned_execution(&writer).await?;
    writer
        .start_provider_attempt(
            &request.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    let tool_name = options
        .tool_name
        .clone()
        .unwrap_or_else(|| crate::tools::native::SPAWN_PROCESS_TOOL_NAME.into());
    let (native_id, arguments) = if crate::tools::native::is_session_message_tool(&tool_name) {
        (
            "bridge-native-tool",
            serde_json::json!({"agent": request.request().agent_did, "prompt": "work"}),
        )
    } else {
        ("spawn-native-tool", serde_json::json!({"command": "work"}))
    };
    let message = gents_protocol::message::Message::Assistant {
        request_id: None,
        id: Some("spawn-provider-message".into()),
        content: vec![gents_protocol::message::AssistantContent::ToolCall(
            gents_protocol::message::ToolCall {
                raw_arguments: None,
                id: native_id.into(),
                call_id: Some("spawn-provider-call".into()),
                function: gents_protocol::message::ToolFunction::new(tool_name, arguments),
                signature: None,
                additional_params: None,
            },
        )],
    };
    let mut published = writer.publish_native_turn(&request, 0, 0, &message).await?;
    let accepted = published
        .accepted_tools
        .pop()
        .expect("canonical publication accepted the tool call");
    let deadline = request
        .claimed_deadline_at()
        .expect("claimed request deadline");
    let mut tool = ToolCallLifecycle::from_accepted(
        node.clone(),
        node_did.clone(),
        request.request().requester_did.clone(),
        accepted,
        deadline,
        options.await_mode,
    )?;
    if options.start_running {
        tool.start_running().await?;
    }
    if options.job_pin {
        let doc_id = tool.doc_id().expect("published tool row").to_owned();
        write_fixture_job_pin(&node, &doc_id).await?;
        tool = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &doc_id,
            &node_did,
            &format!("session-{name}"),
            request.request().requester_did.as_deref(),
        )
        .await?
        .expect("pinned tool row");
    }
    Ok((
        PublishedAdmission {
            node,
            path,
            tool,
            node_did,
        },
        request,
    ))
}

pub(crate) fn test_p2p_config() -> defra_node::P2PConfig {
    defra_node::P2PConfig {
        port: Some(0),
        bind_addr: Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        relay_mode: p2p::iroh::IrohRelayModeConfig::Disabled,
        discovery: p2p::iroh::IrohDiscoveryConfig::Disabled,
        allowlist: p2p::iroh::IrohAllowlistConfig::AcceptAll,
        max_concurrent_multipath_paths: None,
        secret_key_path: None,
        peer_key: defra_node::PeerKeySource::File,
        load_persisted_collections: false,
        sync: defra_node::SyncTuning::default(),
        rebroadcast_on_merge: false,
    }
}

pub async fn published_spawn_parent(name: &str) -> (Arc<EmbeddedNode>, PathBuf, ToolCallLifecycle) {
    let PublishedAdmission {
        node, path, tool, ..
    } = published_admission(PublishedAdmissionOptions {
        name: name.to_owned(),
        ..Default::default()
    })
    .await
    .expect("publish canonical native tool admission");
    (node, path, tool)
}

pub async fn published_background_bridge(
    name: &str,
) -> (Arc<EmbeddedNode>, PathBuf, ToolCallLifecycle) {
    let PublishedAdmission {
        node, path, tool, ..
    } = published_admission(PublishedAdmissionOptions {
        name: name.to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        tool_name: Some(crate::tools::native::AGENT_NEW_TOOL_NAME.to_owned()),
        ..Default::default()
    })
    .await
    .expect("publish canonical background agent_new admission");
    (node, path, tool)
}

pub(crate) async fn materialize_session_message(
    node: &Arc<EmbeddedNode>,
    request: &RequestLifecycle,
    tool: &mut ToolCallLifecycle,
    target_node_did: &str,
    prompt: &str,
) -> anyhow::Result<crate::session::message::SessionMessageReceipt> {
    let caller = request.request();
    let cause = crate::request::lifecycle::SessionMessageCause {
        caller_node_did: caller.node_did.clone(),
        caller_agent_did: caller.agent_did.clone(),
        caller_session_id: caller.session_id.clone(),
        caller_requester_did: caller.requester_did.clone(),
        caller_request_id: caller.request_id.clone(),
        caller_request_doc_id: caller.doc_id.clone(),
        caller_hop: caller.request_hop,
        tool_call_id: tool.tool_call_id().to_owned(),
        tool_call_doc_id: tool
            .doc_id()
            .expect("accepted session-message row")
            .to_owned(),
        correlation: None,
    };
    let target = crate::request::lifecycle::SessionMessageTarget {
        node_did: target_node_did.to_owned(),
        agent_did: caller.agent_did.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
    };
    let signer = tool.access()?.managed_agent_signer().ok_or_else(|| {
        anyhow::anyhow!("fixture session message requires a managed Agent signer")
    })?;
    let mut plan = crate::session::message::plan(
        node,
        &cause,
        &target,
        crate::session::message::RenderedBody {
            content: prompt.to_owned(),
            goal: None,
        },
        None,
        false,
        &signer,
    )
    .await?
    .map_err(anyhow::Error::msg)?;
    plan.sign_agent_creation(&cause.caller_node_did, &caller.input, &signer)
        .await?;
    crate::session::message::commit(node, &cause, tool, plan, false)
        .await
        .map_err(Into::into)
}

pub struct PublishedSessionMessage {
    pub admission: PublishedAdmission,
    pub caused_request_id: String,
    pub caused_request_doc_id: String,
}

pub async fn published_session_message(
    options: PublishedAdmissionOptions,
) -> anyhow::Result<PublishedSessionMessage> {
    Ok(published_session_message_with_owner(options).await?.0)
}

pub async fn published_session_message_with_owner(
    options: PublishedAdmissionOptions,
) -> anyhow::Result<(PublishedSessionMessage, RequestLifecycle)> {
    anyhow::ensure!(
        options.real_identity && options.await_mode == AwaitMode::Background,
        "a session message is signed by a real node and runs in background"
    );
    let (mut admission, request) = published_admission_with_owner(PublishedAdmissionOptions {
        tool_name: Some(crate::tools::native::AGENT_NEW_TOOL_NAME.to_owned()),
        start_running: false,
        ..options
    })
    .await?;
    let receipt = materialize_session_message(
        &admission.node,
        &request,
        &mut admission.tool,
        &admission.node_did.clone(),
        "work",
    )
    .await?;
    Ok((
        PublishedSessionMessage {
            admission,
            caused_request_id: receipt.request_id,
            caused_request_doc_id: receipt.request_doc_id,
        },
        request,
    ))
}

pub async fn pending_spawn_parent(name: &str) -> (Arc<EmbeddedNode>, PathBuf, ToolCallLifecycle) {
    let PublishedAdmission {
        node, path, tool, ..
    } = published_admission(PublishedAdmissionOptions {
        name: name.to_owned(),
        start_running: false,
        ..Default::default()
    })
    .await
    .expect("publish pending canonical native tool admission");
    (node, path, tool)
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::tools::call::lifecycle::{CancelCause, FailureClass};

    async fn row(node: &EmbeddedNode, session_id: &str) -> serde_json::Value {
        let session = crate::store::graphql::escape_graphql_string(session_id);
        let response = node
            .execute(&format!(r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ request_id request_doc_id lifecycle_state tool_failure_class cancel_cause deadline_at await_mode }} }}"#))
            .await;
        assert!(!response.has_errors(), "{:#?}", response.errors);
        let data = response.data.unwrap();
        let rows = data["AgentToolCall"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "exactly one admitted physical tool row");
        rows.first().cloned().expect("one admitted tool call")
    }

    async fn teardown(node: Arc<EmbeddedNode>, path: PathBuf) {
        node.shutdown().await.expect("shut down test node");
        std::fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn admitted_native_lifecycle_persists_completed_terminal() {
        let (admission, _request_owner) =
            published_admission_with_owner(PublishedAdmissionOptions {
                name: "native-complete".to_owned(),
                start_running: false,
                ..Default::default()
            })
            .await
            .expect("publish pending canonical native tool admission");
        let PublishedAdmission {
            node,
            path,
            mut tool,
            ..
        } = admission;
        let request_doc_id = tool.request_doc_id().unwrap().to_owned();
        tool.start_running().await.unwrap();
        let running = row(&node, "session-native-complete").await;
        assert_eq!(running["lifecycle_state"], "running");
        assert_eq!(running["request_doc_id"], request_doc_id);
        assert!(running["deadline_at"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        tool.complete("ok").await.unwrap();
        assert_eq!(
            row(&node, "session-native-complete").await["lifecycle_state"],
            "completed"
        );
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_native_lifecycle_persists_failure_class() {
        let (node, path, mut tool) = published_spawn_parent("native-fail").await;
        tool.fail("error", FailureClass::ToolReturnedError)
            .await
            .unwrap();
        let row = row(&node, "session-native-fail").await;
        assert_eq!(row["lifecycle_state"], "failed");
        assert_eq!(row["tool_failure_class"], "toolReturnedError");
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_native_terminal_is_irreversible() {
        let (node, path, mut tool) = published_spawn_parent("native-terminal").await;
        tool.complete("done").await.unwrap();
        let error = tool.fail("late", FailureClass::External).await.unwrap_err();
        assert!(format!("{error:#}").contains("illegal tool call transition"));
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_native_rejects_duplicate_dispatch_without_duplicate_row() {
        let (node, path, mut tool) = published_spawn_parent("native-duplicate").await;
        let error = tool.start_running().await.unwrap_err();
        assert!(format!("{error:#}").contains("cannot re-dispatch"));
        assert_eq!(
            row(&node, "session-native-duplicate").await["lifecycle_state"],
            "running"
        );
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_native_load_preserves_state_deadline_and_terminal_updates() {
        let (node, path, tool) = published_spawn_parent("native-load").await;
        let expected_deadline = tool.deadline_at;
        let mut loaded =
            ToolCallLifecycle::load(node.clone(), "session-native-load", "spawn-native-tool")
                .await
                .unwrap()
                .expect("admitted tool loads");
        loaded.timeout().await.unwrap();
        let row = row(&node, "session-native-load").await;
        assert_eq!(row["lifecycle_state"], "timedOut");
        assert_eq!(row["cancel_cause"], "deadline");
        let observed = chrono::DateTime::parse_from_rfc3339(row["deadline_at"].as_str().unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(observed, expected_deadline);
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_native_load_returns_persisted_failure() {
        let (node, path, mut tool) = published_spawn_parent("native-load-failure").await;
        tool.fail("oops", FailureClass::Transport).await.unwrap();
        drop(tool);
        let loaded = ToolCallLifecycle::load(
            node.clone(),
            "session-native-load-failure",
            "spawn-native-tool",
        )
        .await
        .unwrap()
        .expect("terminal admitted tool loads");
        drop(loaded);
        let row = row(&node, "session-native-load-failure").await;
        assert_eq!(row["lifecycle_state"], "failed");
        assert_eq!(row["tool_failure_class"], "transport");
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_native_cancel_during_run_and_after_load_persist_causes() {
        let (node, path, mut tool) = published_spawn_parent("native-cancel").await;
        tool.cancel_during_run(CancelCause::UserCancelled)
            .await
            .unwrap();
        let persisted = row(&node, "session-native-cancel").await;
        assert_eq!(persisted["lifecycle_state"], "cancelled");
        assert_eq!(persisted["cancel_cause"], "userCancelled");
        teardown(node, path).await;

        let (node, path, tool) = published_spawn_parent("native-cancel-load").await;
        assert!(row(&node, "session-native-cancel-load").await["cancel_cause"].is_null());
        drop(tool);
        let mut loaded = ToolCallLifecycle::load(
            node.clone(),
            "session-native-cancel-load",
            "spawn-native-tool",
        )
        .await
        .unwrap()
        .unwrap();
        loaded
            .cancel_during_run(CancelCause::Interrupted)
            .await
            .unwrap();
        assert_eq!(
            row(&node, "session-native-cancel-load").await["cancel_cause"],
            "interrupted"
        );
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_pending_native_cancel_persists_cause() {
        let (node, path, mut tool) = pending_spawn_parent("native-pending-cancel").await;
        tool.cancel_before_dispatch(CancelCause::Interrupted)
            .await
            .unwrap();
        let row = row(&node, "session-native-pending-cancel").await;
        assert_eq!(row["lifecycle_state"], "cancelled");
        assert_eq!(row["cancel_cause"], "interrupted");
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn timeout_adopts_terminal_written_by_terminal_parent_sweep() {
        let admission = published_admission(PublishedAdmissionOptions {
            name: "timeout-cas".to_owned(),
            real_identity: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let node = admission.node;
        let path = admission.path;
        let node_did = admission.node_did;
        let mut tool = admission.tool;
        let signer = tool.access().unwrap().managed_agent_signer().unwrap();
        let identities: Vec<Arc<dyn NodeIdentity>> = vec![Arc::new(signer)];
        let request = crate::store::graphql::escape_graphql_string("request-timeout-cas");
        let response = node
            .execute(&format!(r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{request}" }} }}, input: {{ lifecycle_state: "interrupted" }}) {{ _docID }} }}"#))
            .await;
        assert!(!response.has_errors(), "{:#?}", response.errors);

        let report =
            ToolCallLifecycle::reconcile_terminal_parent_owned_tools(&node, &node_did, &identities)
                .await
                .unwrap();
        assert_eq!(report.tool_calls_terminalized, 1);
        assert!(!tool.timeout().await.unwrap());
        let row = row(&node, "session-timeout-cas").await;
        assert_eq!(row["lifecycle_state"], "cancelled");
        assert_eq!(row["cancel_cause"], "interrupted");
        assert!(row["tool_failure_class"].is_null());
        teardown(node, path).await;
    }
}
