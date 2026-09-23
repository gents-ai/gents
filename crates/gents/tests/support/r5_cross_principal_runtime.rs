//! Shared public-runtime fixture for R5 cross-principal accepted turns.
//!
//! Both generated R5 admission cases and composed crash/convergence scenarios
//! use this owner.  Provider publication is driven by `Gents`; this module
//! never calls a hook directly and never fabricates an accepted header.

use std::sync::Arc;

use gents::{agent::p2p_reconcile::resolve_template, graphql::escape_graphql_string};
use gents::{
    default_behavior_id_for_agent, load_agent_behavior, upsert_agent_behavior, AgentIdentity,
    DocumentRuntimeOptions, Gents, RuntimeSnapshotObserver, ToolCeiling,
};

use super::accepted_turn::{
    boot_prepared_accepted_turn, prepare_accepted_turn_as, AcceptedTurnRuntime, AcceptedTurnSpec,
};
use super::enrollment::{authorize_enrollment_peer, wait_for_peer_identity};
use super::fixtures::{
    bind_default_behavior_backend, configure_subagent_behavior, subagent_target,
};
use super::interrupt::BootedAgent;
use super::mock_endpoint::MockModelEndpoint;
use super::streaming_backend::{StreamChunk, StreamPlan, StreamResponse, StreamScript};
use super::{test_p2p_db, TestDb};

pub struct R5AcceptedRuntime {
    pub parent_db: TestDb,
    pub child_db: TestDb,
    pub parent: AcceptedTurnRuntime,
    pub child: BootedAgent,
    pub parent_session_id: String,
    pub parent_behavior_id: String,
    pub child_agent_did: String,
    child_endpoint: ChildEndpoint,
}

impl R5AcceptedRuntime {
    pub fn child_provider_observed_requests(&self, marker: &str) -> usize {
        match &self.child_endpoint {
            ChildEndpoint::Immediate(_) => 0,
            ChildEndpoint::Paused(backend) => backend.observed_requests(marker),
        }
    }

    pub async fn shutdown(self) {
        self.parent.shutdown().await;
        self.child.shutdown().await;
    }
}

enum ChildEndpoint {
    Immediate(MockModelEndpoint),
    Paused(super::streaming_backend::MockStreamingBackend),
}

impl ChildEndpoint {
    fn endpoint(&self) -> &str {
        match self {
            Self::Immediate(endpoint) => endpoint.endpoint(),
            Self::Paused(backend) => backend.endpoint(),
        }
    }
}

pub struct R5AcceptedSpec<'a> {
    pub name: &'a str,
    pub parent_request_id: &'a str,
    pub parent_session_id: &'a str,
    pub parent_tool_call_id: &'a str,
    pub target_behavior_id: &'a str,
    pub prompt: &'a str,
    /// Immutable depth of the accepted parent request whose tool call is delegated.
    pub parent_subagent_depth: u32,
    /// Hold the worker's provider response until cancellation has been observed.
    pub hold_child_provider: bool,
}

struct R5SnapshotProbe {
    published: tokio::sync::mpsc::UnboundedSender<Vec<String>>,
}

impl RuntimeSnapshotObserver for R5SnapshotProbe {
    fn on_generation_published(
        &self,
        _generation: u64,
        _configuration_fingerprint: &str,
        runnable_behavior_ids: &[String],
    ) {
        let _ = self.published.send(runnable_behavior_ids.to_vec());
    }

    fn on_event_sources_reconciled(
        &self,
        _generation: u64,
        _configuration_fingerprint: &str,
        _result: Result<(), &str>,
    ) {
    }
}

pub struct R5SamePrincipalRuntime {
    pub db: TestDb,
    pub parent: AcceptedTurnRuntime,
    pub parent_behavior_id: String,
}

impl R5SamePrincipalRuntime {
    pub async fn shutdown(self) {
        self.parent.shutdown().await;
    }
}

pub async fn boot_same_principal_accepted_turn(spec: R5AcceptedSpec<'_>) -> R5SamePrincipalRuntime {
    let db = super::test_db(&format!("{}-same-principal", spec.name)).await;
    let agent_did = db.node_identity.did().to_owned();
    let parent_behavior_id = format!("{}-parent-behavior", spec.name);
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        spec.target_behavior_id,
        &format!("{}-child-tools", spec.name),
        Vec::new(),
        false,
        false,
        None,
    )
    .await;
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        &parent_behavior_id,
        &format!("{}-parent-tools", spec.name),
        vec![subagent_target(
            &agent_did,
            spec.target_behavior_id.to_owned(),
            agent_did.clone(),
            spec.target_behavior_id.to_owned(),
        )],
        true,
        true,
        Some(true),
    )
    .await;
    let arguments = serde_json::json!({
        "name": spec.target_behavior_id,
        "prompt": format!("child prompt for {}", spec.name),
        "await_mode": "background"
    })
    .to_string();
    let backend_id = format!("{}-parent-backend", spec.name);
    let configured = [parent_behavior_id.as_str(), spec.target_behavior_id];
    let child_prompt = format!("child prompt for {}", spec.name);
    let prepared = prepare_accepted_turn_as(
        &db,
        db.node_identity.as_ref(),
        AcceptedTurnSpec {
            backend_id: &backend_id,
            model: "r5-same-principal-model",
            parent_behavior_id: &parent_behavior_id,
            configured_behavior_ids: &configured,
            request_id: spec.parent_request_id,
            session_id: spec.parent_session_id,
            prompt: spec.prompt,
            accepted_chunks: vec![StreamChunk::tool_call(
                spec.parent_tool_call_id,
                "spawn_subagent",
                arguments,
            )],
            // This case observes admission while the child is still running.
            // Hold its provider explicitly instead of racing child completion.
            child_plans: vec![StreamPlan::current_authored_user(
                &child_prompt,
                vec![StreamResponse::Stream(StreamScript::paused_before(
                    &child_prompt,
                    vec![StreamChunk::text("child complete")],
                ))],
            )],
            valid_until: None,
            subagent_depth: Some(spec.parent_subagent_depth),
            request_setup: None,
        },
    )
    .await;
    let agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        db.node_identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build same-principal R5 runtime");
    let parent = boot_prepared_accepted_turn(&db, prepared, agent).await;
    R5SamePrincipalRuntime {
        db,
        parent,
        parent_behavior_id,
    }
}

pub async fn boot_cross_principal_accepted_turn(spec: R5AcceptedSpec<'_>) -> R5AcceptedRuntime {
    let child_db = test_p2p_db(&format!("{}-child", spec.name)).await;
    // Enrollment binds the signed principal DID to the transport peer key.
    // A separately generated runtime identity cannot truthfully claim this
    // node's P2P endpoint and is correctly rejected by the enrollment owner.
    let child_identity: Arc<dyn AgentIdentity> = child_db.node_identity.clone();
    let child_agent_did = child_identity.did().to_owned();
    let default_behavior_id = default_behavior_id_for_agent(&child_agent_did);
    let child_endpoint = if spec.hold_child_provider {
        let child_prompt = format!("child prompt for {}", spec.name);
        ChildEndpoint::Paused(
            super::streaming_backend::MockStreamingBackend::start_with_plans(
                "default",
                vec![StreamPlan::current_authored_user(
                    &child_prompt,
                    vec![StreamResponse::Stream(StreamScript::paused_before(
                        &child_prompt,
                        vec![StreamChunk::text("child complete")],
                    ))],
                )],
            )
            .expect("R5 paused child backend"),
        )
    } else {
        ChildEndpoint::Immediate(
            MockModelEndpoint::start("default").expect("R5 child mock endpoint"),
        )
    };
    bind_default_behavior_backend(
        child_db.node.as_ref(),
        &child_agent_did,
        &format!("{}-child-backend", spec.name),
        child_endpoint.endpoint(),
    )
    .await;
    let mut child_behavior = load_agent_behavior(child_db.node.as_ref(), &default_behavior_id)
        .await
        .expect("load default R5 child behavior")
        .expect("default R5 child behavior");
    child_behavior.behavior_id = spec.target_behavior_id.to_owned();
    child_behavior.display_name = Some(spec.target_behavior_id.to_owned());
    upsert_agent_behavior(child_db.node.as_ref(), &child_behavior)
        .await
        .expect("upsert R5 child behavior");
    configure_subagent_behavior(
        child_db.node.as_ref(),
        &child_agent_did,
        spec.target_behavior_id,
        &format!("{}-child-tools", spec.name),
        Vec::new(),
        true,
        true,
        Some(true),
    )
    .await;
    let (snapshot_tx, mut snapshot_rx) = tokio::sync::mpsc::unbounded_channel();
    let child_agent = Gents::from_default_behavior_documents(
        child_db.node.clone(),
        child_identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            runtime_snapshot_observer: Some(Arc::new(R5SnapshotProbe {
                published: snapshot_tx,
            })),
            ..Default::default()
        },
    )
    .await
    .expect("build R5 child runtime");
    assert!(
        child_agent
            .behaviors()
            .iter()
            .any(|behavior| behavior.behavior_id == spec.target_behavior_id),
        "R5 target behavior must resolve into the child runtime snapshot"
    );
    let child_agent_did = child_agent.agent_did().to_owned();

    let parent_db = test_p2p_db(&format!("{}-parent", spec.name)).await;
    let parent_identity = parent_db.node_identity.clone();
    let parent_agent_did = parent_identity.did().to_owned();
    let (parent_peer, parent_address) = wait_for_peer_identity(parent_db.node.as_ref()).await;
    let (child_peer, child_address) = wait_for_peer_identity(child_db.node.as_ref()).await;
    authorize_enrollment_peer(
        child_db.node.clone(),
        &format!("network-{}", spec.name),
        &format!("R5 {}", spec.name),
        child_identity.clone(),
        parent_identity.clone(),
        &parent_peer,
        &parent_address,
    )
    .await;
    authorize_enrollment_peer(
        parent_db.node.clone(),
        &format!("network-{}-return", spec.name),
        &format!("R5 {} return", spec.name),
        parent_identity.clone(),
        child_identity.clone(),
        &child_peer,
        &child_address,
    )
    .await;
    write_data_plane_pairing(
        parent_db.node.as_ref(),
        &child_peer,
        &parent_agent_did,
        "subagent-coordinator",
        &child_address,
    )
    .await;
    write_data_plane_pairing(
        child_db.node.as_ref(),
        &parent_peer,
        &child_agent_did,
        "subagent-host",
        &parent_address,
    )
    .await;
    // Start the target runtime only after its peer enrollment and replication
    // authority are durable, so the first observed bridge is evaluated against
    // the same admitted peer facts as the production deployment boundary.
    let (child_shutdown, child_rx) = tokio::sync::watch::channel(false);
    let child_handle = tokio::spawn(child_agent.run(child_rx));
    super::interrupt::wait_for_runtime_ready(child_db.node.as_ref(), &child_agent_did).await;
    let snapshot_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let remaining = snapshot_deadline.saturating_duration_since(tokio::time::Instant::now());
        let observed = tokio::time::timeout(remaining, snapshot_rx.recv())
            .await
            .expect("child runtime did not publish its active behavior snapshot")
            .expect("child runtime snapshot observer stopped");
        if observed.iter().any(|id| id == spec.target_behavior_id) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < snapshot_deadline,
            "child active runtime snapshot omitted target behavior {}; runnable={observed:?}",
            spec.target_behavior_id
        );
    }
    let child = BootedAgent::new(child_shutdown, child_handle, child_agent_did.clone());

    let parent_behavior_id = format!("{}-parent-behavior", spec.name);
    configure_subagent_behavior(
        parent_db.node.as_ref(),
        &parent_agent_did,
        &parent_behavior_id,
        &format!("{}-parent-tools", spec.name),
        vec![subagent_target(
            &parent_agent_did,
            spec.target_behavior_id.to_owned(),
            child_agent_did.clone(),
            spec.target_behavior_id.to_owned(),
        )],
        true,
        true,
        Some(true),
    )
    .await;
    let arguments = serde_json::json!({
        "name": spec.target_behavior_id,
        "prompt": format!("child prompt for {}", spec.name),
        "await_mode": "background"
    })
    .to_string();
    let configured = [parent_behavior_id.as_str()];
    let parent_backend_id = format!("{}-parent-backend", spec.name);
    let prepared = prepare_accepted_turn_as(
        &parent_db,
        parent_identity.as_ref(),
        AcceptedTurnSpec {
            backend_id: &parent_backend_id,
            model: "r5-parent-model",
            parent_behavior_id: &parent_behavior_id,
            configured_behavior_ids: &configured,
            request_id: spec.parent_request_id,
            session_id: spec.parent_session_id,
            prompt: spec.prompt,
            accepted_chunks: vec![StreamChunk::tool_call(
                spec.parent_tool_call_id,
                "spawn_subagent",
                arguments,
            )],
            child_plans: Vec::new(),
            valid_until: None,
            subagent_depth: Some(spec.parent_subagent_depth),
            request_setup: None,
        },
    )
    .await;
    let parent_agent = Gents::from_default_behavior_documents(
        parent_db.node.clone(),
        parent_identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build R5 parent runtime");
    let parent = boot_prepared_accepted_turn(&parent_db, prepared, parent_agent).await;

    R5AcceptedRuntime {
        parent_db,
        child_db,
        parent,
        child,
        parent_session_id: spec.parent_session_id.to_owned(),
        parent_behavior_id,
        child_agent_did,
        child_endpoint,
    }
}

async fn write_data_plane_pairing(
    node: &gents::defra_node::EmbeddedNode,
    peer_id: &str,
    self_did: &str,
    template: &str,
    peer_address: &str,
) {
    let collections = resolve_template(template)
        .unwrap_or_else(|| panic!("R5 template {template} must resolve"))
        .collections
        .iter()
        .map(|collection| format!("\"{}\"", escape_graphql_string(collection)))
        .collect::<Vec<_>>()
        .join(", ");
    let peer_id = escape_graphql_string(peer_id);
    let self_did = escape_graphql_string(self_did);
    let template = escape_graphql_string(template);
    let peer_address = escape_graphql_string(peer_address);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let response = node
        .execute(&format!(
            r#"mutation {{
                upsert_DataPlanePairingDesired(
                    filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                    add: {{
                        peer_id: "{peer_id}", agent_did: "{self_did}",
                        collections: [{collections}], replicator_addresses: ["{peer_address}"],
                        template: "{template}", source: "r5-cross-principal-conformance",
                        created_at: "{now}", updated_at: "{now}"
                    }},
                    update: {{
                        agent_did: "{self_did}", collections: [{collections}],
                        replicator_addresses: ["{peer_address}"], template: "{template}",
                        source: "r5-cross-principal-conformance", updated_at: "{now}"
                    }}
                ) {{ _docID }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "write R5 {template} data-plane pairing: {:?}",
        response.errors
    );
}
