use std::sync::Arc;

use anyhow::Context;

use gents_documents::apply_desired_state_plan;
use gents_documents::mailbox_write_decl::canonical_mailbox_write_decl;
use gents_documents::DatastoreToolSurfaceDocument;
use gents_documents::{
    grant_selected_self_configuration, read_desired_state_record_in_txn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use gents_documents::{DatastoreTools, SurfaceToolDecl, Tools};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose, RequestScope,
};
use gents_session::request::lifecycle::{reserve_goal_session, submit_client_request};
use gents_store::config_client::GraphqlEndpoint;
use gents_store::{
    collection::Collection, config_client::ConfigAccess, identity::KeyIdentity,
    identity::NodeIdentity,
};

use super::accepted_turn::PreparedAcceptedTurn;
use gents_inference::test_support::scripted_backend::{
    ScriptedBackend, StreamChunk, StreamPlan, StreamResponse,
};
use gents_store::test_support::TestStore;

pub struct SignedMailboxTurn {
    pub user: KeyIdentity,
    pub client: ConfigAccess,
    pub agent: Arc<dyn NodeIdentity>,
    pub agent_access: ConfigAccess,
    pub node_did: String,
    pub session_doc: String,
    pub agent_doc: String,
}

impl SignedMailboxTurn {
    pub async fn submit(&self, mut request: AgentRequestCreate) -> anyhow::Result<String> {
        request.session_doc_id = Some(self.session_doc.clone());
        request.agent_doc_id = Some(self.agent_doc.clone());
        gents_session::request::admission::sign_agent_request_create(&self.user, &mut request)
            .await?;
        submit_client_request(&self.client, &request).await?;
        let query = format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}, limit: 2) {{_docID}} }}"#,
            gents_store::graphql::escape_graphql_string(&request.request_id)
        );
        let response = self.client.execute(&query).await?;
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .context("client request query omitted rows")?;
        let [row] = rows.as_slice() else {
            anyhow::bail!("client request unavailable or ambiguous")
        };
        Ok(row["_docID"]
            .as_str()
            .context("client request physical ID missing")?
            .to_owned())
    }

    pub fn request(&self, request_id: &str, session_id: &str, content: &str) -> AgentRequestCreate {
        AgentRequestCreate::base(
            RequestPurpose::Normal,
            request_id,
            RequestScope::new(
                &self.node_did,
                self.user.did(),
                self.agent.did(),
                session_id,
            ),
            content,
            "interactive",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            AgentRequestAdmissionRecord::client(self.user.did()),
        )
    }
}

pub async fn prepare_signed_mailbox_turn(
    db: &TestStore,
    backend_id: &str,
    model: &str,
    request_id: &str,
    session_id: &str,
    prompt: &str,
    chunks: Vec<StreamChunk>,
) -> anyhow::Result<(PreparedAcceptedTurn, SignedMailboxTurn)> {
    let user = KeyIdentity::load_or_create(db.path().join("mailbox-client.key"), None)?;
    let client = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(db.graphql_endpoint(), user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?
            .with_client_signed_agent_creates(),
    );
    // The client key is an enrolled User; its Agent creation projects only
    // what that enrollment grants.
    gents_documents::test_support::enroll_test_user(
        &db.node,
        db.node_identity.as_ref(),
        &user,
        &client,
    )
    .await;
    let node_did = db.node_identity.did().to_owned();
    let binding = gents_store::identity::provision_agent_identity_authorized(
        &db.node,
        db.node_identity.as_ref(),
        &user.did().parse()?,
    )
    .await?;
    let agent = gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)?;
    let agent_access =
        ConfigAccess::local_managed_agent(db.node.clone(), agent.did().parse()?, agent.as_ref())?;
    let context_id = format!("{}:context", agent.did());
    let tools_id = format!("{}:tools", agent.did());
    let profile_id = format!("{}:profile", agent.did());
    let surface_id = format!("{}:mailbox", agent.did());
    let plans = vec![StreamPlan::current_authored_user(
        prompt,
        vec![
            StreamResponse::streams(prompt, chunks),
            StreamResponse::completes(prompt, ["parent complete"]),
        ],
    )];
    let backend = ScriptedBackend::start_with_plans(model, plans)?;
    let tools = Tools {
        tools_id: tools_id.clone(),
        node_did: node_did.clone(),
        datastore: Some(DatastoreTools {
            enable_defra_query: Some(true),
            datastore_tool_surface_ids: Some(vec![surface_id.clone()]),
            ..Default::default()
        }),
        agents: Some(gents_documents::AgentTools {
            target_ids: Vec::new(),
            enabled: Some(true),
        }),
        built_ins: Some(gents_documents::BuiltInTools {
            enable_session_history_tool: Some(true),
            ..Default::default()
        }),
        self_config: Some(gents_tools::builtin::self_config::engineer_self_config()),
        ..Default::default()
    };
    let surface = DatastoreToolSurfaceDocument {
        surface_id: surface_id.clone(),
        node_did: node_did.clone(),
        display_name: Some("Engineer escalations".into()),
        enabled: true,
        entries: Some(vec![
            SurfaceToolDecl::Create(canonical_mailbox_write_decl()),
        ]),
        created_at: None,
        tags: Vec::new(),
    };
    let backend_doc = serde_json::json!({"node_did":node_did, "backend_id":backend_id, "name":backend_id,
    "provider_kind":"OpenAiCompatible", "openai_wire_api":"chat_completions", "auth":{"kind":"unauthenticated"},
    "endpoint":backend.endpoint(), "max_concurrent":1, "max_queue_depth":100, "enabled":true});
    let profile_doc = serde_json::json!({"node_did":node_did, "profile_id":profile_id, "backend_id":backend_id, "model_name":model});
    let docs = vec![
        (Collection::Node, serde_json::json!({"node_did":node_did})),
        (
            Collection::DefaultAgentSelection,
            serde_json::json!({"node_did":node_did, "default_agent_did":agent.did()}),
        ),
        (
            Collection::Agent,
            serde_json::json!({"node_did":node_did, "agent_did":agent.did(),
            "creation_nonce":binding.creation_nonce, "inference_profile_id":profile_id, "context_id":context_id, "enabled":true}),
        ),
        (
            Collection::AgentContext,
            serde_json::json!({"node_did":node_did, "context_id":context_id, "tools_id":tools_id}),
        ),
        (Collection::Tools, serde_json::to_value(tools)?),
        (
            Collection::DatastoreToolSurface,
            serde_json::to_value(surface)?,
        ),
        (Collection::InferenceProfile, profile_doc),
        (Collection::InferenceBackend, backend_doc),
    ];
    let plan = DesiredStateApplyPlan::new(
        docs.into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )?;
    let agent_doc = client
        .transact_write("test.mailbox.signed_configuration", |txn| {
            let plan = &plan;
            let node_did = &node_did;
            let surface_id = &surface_id;
            let agent = &agent;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await?;
                grant_selected_self_configuration(txn, node_did, agent.did()).await?;
                let (surface, _) = read_desired_state_record_in_txn(
                    txn,
                    Collection::DatastoreToolSurface,
                    node_did,
                    surface_id,
                )
                .await?
                .context("mailbox surface missing")?;
                txn.add_actor_relationship(
                    "DatastoreToolSurface",
                    &surface,
                    "reader",
                    &agent.did().parse()?,
                )
                .await?;
                let (doc, _) =
                    read_desired_state_record_in_txn(txn, Collection::Agent, node_did, agent.did())
                        .await?
                        .context("mailbox Agent missing")?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await?;
    gents_inference::backends::registry::set_backend_probe_status_as(
        &client, &node_did, backend_id, "healthy",
    )
    .await?;
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        request_id,
        RequestScope::new(&node_did, user.did(), agent.did(), session_id),
        prompt,
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::client(user.did()),
    );
    create.input.initial_title = Some(gents_protocol::session::SessionTitle {
        text: "Mailbox test".into(),
        source: gents_protocol::session::SessionTitleSource::User,
    });
    let session_doc = reserve_goal_session(&client, &create).await?;
    let turn = SignedMailboxTurn {
        user,
        client,
        agent,
        agent_access,
        node_did,
        session_doc,
        agent_doc,
    };
    turn.submit(create).await?;
    Ok((PreparedAcceptedTurn { backend }, turn))
}
