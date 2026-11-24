//! A runtime booted the way production boots it (`Gents::run` over the
//! Agent documents of a signed home), its Agent bound to an HTTP model backend
//! the test scripts. Every wait follows a document change or a runtime event
//! and fails with the last state seen once nothing changes for
//! [`STALL_CEILING`].

use std::sync::Arc;

use anyhow::{Context, Result};
use gents_protocol::node_readiness::{NodeReadinessProcessState, NodeReadinessSnapshot};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_store::test_support::snapshot_reader::SnapshotReader as _;
use gents_store::test_support::{read_when_changed, STALL_CEILING};
use tokio::sync::{mpsc, watch};
use tracing_subscriber::layer::SubscriberExt as _;

use crate::support::streaming_backend::{MockStreamingBackend, StreamScript};

/// A signed home whose User owns the default Agent, bound through its
/// inference profile to one OpenAI-compatible backend.
pub struct RuntimeHome {
    pub db: crate::support::TestDb,
    pub user: KeyIdentity,
    pub user_access: ConfigAccess,
    pub agent: Arc<dyn NodeIdentity>,
}

impl RuntimeHome {
    pub async fn new(
        name: &str,
        backend_id: &str,
        endpoint: &str,
        max_concurrent: i64,
        probe_status: &str,
    ) -> Self {
        let (db, user, user_access, binding) = crate::test_support::root::signed_test_node_from_db(
            crate::support::test_db(name).await,
            name,
        )
        .await;
        let node_did = db.node_identity.did();
        let profile = serde_json::json!({
            "node_did": node_did, "profile_id": format!("{}:inference", binding.agent_did),
            "backend_id": backend_id, "model_name": "default"
        });
        apply_documents(
            &user_access,
            vec![
                (
                    gents_store::collection::Collection::InferenceProfile,
                    profile,
                ),
                backend_document(node_did, backend_id, endpoint, max_concurrent),
            ],
        )
        .await;
        let runtime_access = ConfigAccess::local_as(db.node.clone(), node_did.parse().unwrap());
        gents_inference::backends::registry::set_backend_probe_status_as(
            &runtime_access,
            node_did,
            backend_id,
            probe_status,
        )
        .await
        .unwrap();
        let agent =
            gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .unwrap();
        grant_agent_operations(&db.node, db.node_identity.as_ref(), agent.as_ref()).await;
        Self {
            db,
            user,
            user_access,
            agent,
        }
    }

    pub fn admission(&self) -> gents_store::local::backend_protection::BackendAdmission {
        self.db.home.backend_admission()
    }

    pub async fn agent_config_access(&self) -> ConfigAccess {
        use gents_store::config_client::NodePermission;
        let runtime_did = self.db.node_identity.did();
        let admin = ConfigAccess::local_as(self.db.node.clone(), runtime_did.parse().unwrap());
        let agent_did = identity::Did::new(self.agent.did()).unwrap();
        for permission in [
            NodePermission::CollectionGet,
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
        ] {
            admin
                .grant_node_permission(&agent_did, permission)
                .await
                .unwrap();
        }
        self.user_access
            .transact_write("test.runtime.agent_config", |txn| {
                let agent_did = &agent_did;
                Box::pin(async move {
                    gents_documents::grant_selected_self_configuration(
                        txn,
                        runtime_did,
                        agent_did.as_str(),
                    )
                    .await
                })
            })
            .await
            .unwrap();
        ConfigAccess::local_managed_agent(self.db.node.clone(), agent_did, self.agent.as_ref())
            .unwrap()
    }

    /// Rewrites the backend document the User configured; a change to what an
    /// Agent's slot is built from restages that slot in the next generation.
    pub async fn restage(&self, backend_id: &str, endpoint: &str, max_concurrent: i64) {
        apply_documents(
            &self.user_access,
            vec![backend_document(
                self.db.node_identity.did(),
                backend_id,
                endpoint,
                max_concurrent,
            )],
        )
        .await;
    }

    /// Submits a request the User signs for `agent_did`, titling its session
    /// so no title request precedes it, and returns its document ID.
    pub async fn submit_request(
        &self,
        agent_did: &str,
        request_id: &str,
        session_id: &str,
        content: &str,
    ) -> String {
        use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
        use gents_session::request::lifecycle::{RequestIdentity, RequestSigner, RequestSpec};
        let user_did = self.user.did();
        let node_did = self.db.node_identity.did();
        let create = gents_session::request::lifecycle::build_signed_client_request(
            &self.user_access,
            RequestSpec::new(
                RequestPurpose::Normal,
                RequestIdentity {
                    request_id: request_id.to_owned(),
                    node_did: node_did.to_owned(),
                    requester_did: Some(user_did.to_owned()),
                    agent_did: agent_did.to_owned(),
                    session_id: session_id.to_owned(),
                    content: content.to_owned(),
                    execution_origin:
                        gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                    created_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                },
                AgentRequestAdmissionRecord::client(user_did),
            ),
            RequestSigner::Identity(&self.user),
        )
        .await
        .unwrap();
        gents_session::session::update_session_title_with_source(
            &self.user_access,
            node_did,
            Some(user_did),
            session_id,
            "running runtime fixture",
            gents_protocol::session::SessionTitleSource::User,
        )
        .await
        .unwrap();
        gents_session::request::lifecycle::submit_client_request(&self.user_access, &create)
            .await
            .unwrap();
        let request = escape_graphql_string(request_id);
        let response = self
            .user_access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{request}"}}}}) {{_docID}} }}"#
            ))
            .await
            .unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        let [row] = rows.as_slice() else {
            panic!("submitted request must be unique")
        };
        row["_docID"].as_str().unwrap().to_owned()
    }
}

fn backend_document(
    node_did: &str,
    backend_id: &str,
    endpoint: &str,
    max_concurrent: i64,
) -> (gents_store::collection::Collection, serde_json::Value) {
    (
        gents_store::collection::Collection::InferenceBackend,
        serde_json::json!({
            "node_did": node_did, "backend_id": backend_id,
            "name": backend_id, "provider_kind": "OpenAiCompatible", "endpoint": endpoint,
            "max_concurrent": max_concurrent, "auth": {"kind": "unauthenticated"}
        }),
    )
}

async fn apply_documents(
    access: &ConfigAccess,
    documents: Vec<(gents_store::collection::Collection, serde_json::Value)>,
) {
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .unwrap();
    access
        .transact_write("test.runtime.inference", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
}

/// The node permissions a managed Agent needs to run its own operations.
pub async fn grant_agent_operations(
    node: &Arc<defra_node::Node>,
    runtime: &dyn NodeIdentity,
    agent: &dyn NodeIdentity,
) {
    use gents_store::config_client::NodePermission;
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    let agent_did = identity::Did::new(agent.did()).unwrap();
    for permission in [
        NodePermission::CollectionGet,
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DocumentDelete,
        NodePermission::DacRelationAdd,
        NodePermission::DacRelationDelete,
    ] {
        admin
            .grant_node_permission(&agent_did, permission)
            .await
            .unwrap();
    }
}

/// A model backend that holds the chat completion whose request carries its
/// marker until the test releases it, then streams `text` and stops.
pub struct HeldBackend {
    backend: MockStreamingBackend,
    marker: String,
}

impl HeldBackend {
    pub fn start(marker: &str, text: &'static str) -> Self {
        Self {
            backend: MockStreamingBackend::start(
                "default",
                vec![StreamScript::paused(marker, [text])],
            )
            .unwrap(),
            marker: marker.to_owned(),
        }
    }

    pub fn endpoint(&self) -> &str {
        self.backend.endpoint()
    }

    /// Resolves once a completion for the marker has reached the backend.
    pub async fn entered(&self) {
        self.backend.wait_for_request(&self.marker).await;
    }

    pub fn release(&self) {
        self.backend.release(&self.marker);
    }
}

/// The runtime running over a [`RuntimeHome`]. It records each retired Agent
/// slot whose task has joined, from the runtime's own event, on the test's
/// thread (`#[tokio::test]` runs every task there).
pub struct RunningRuntime {
    node: Arc<defra_node::Node>,
    node_did: String,
    shutdown: watch::Sender<bool>,
    handle: tokio::task::JoinHandle<Result<()>>,
    retired_slots: mpsc::UnboundedReceiver<(String, u64)>,
    _events: tracing::subscriber::DefaultGuard,
}

impl RunningRuntime {
    pub async fn start(
        home: &RuntimeHome,
        options: crate::runtime::agent::DocumentRuntimeOptions,
    ) -> Self {
        let (retired_tx, retired_slots) = mpsc::unbounded_channel();
        let events = tracing::subscriber::set_default(
            tracing_subscriber::Registry::default().with(RetiredSlotEvents(retired_tx)),
        );
        let node = home.db.node.clone();
        let agent = crate::Gents::from_default_agent_documents(
            node.clone(),
            home.db.node_identity.clone(),
            crate::runtime::agent::DocumentRuntimeOptions {
                backend_admission: Some(home.admission()),
                ..options
            },
        )
        .await
        .unwrap();
        let node_did = agent.node_did().to_owned();
        let (shutdown, shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(agent.run(shutdown_rx));
        let runtime = Self {
            node,
            node_did,
            shutdown,
            handle,
            retired_slots,
            _events: events,
        };
        runtime
            .readiness_when("the runtime is ready", |snapshot| {
                snapshot.process_state == NodeReadinessProcessState::Ready
            })
            .await;
        runtime
    }

    pub async fn readiness(&self) -> Option<NodeReadinessSnapshot> {
        let escaped = escape_graphql_string(&self.node_did);
        let data = self
            .node
            .read_data(&format!(
                r#"{{ NodeReadiness(filter: {{ node_did: {{ _eq: "{escaped}" }} }}, limit: 1) {{ snapshot_json }} }}"#
            ))
            .await;
        data["NodeReadiness"][0]["snapshot_json"]
            .as_str()
            .and_then(|snapshot| serde_json::from_str(snapshot).ok())
    }

    /// The first published readiness that satisfies `reached`.
    pub async fn readiness_when(
        &self,
        what: &str,
        reached: impl Fn(&NodeReadinessSnapshot) -> bool,
    ) -> NodeReadinessSnapshot {
        read_when_changed(
            self.node.as_ref(),
            "NodeReadiness",
            what,
            async || match self.readiness().await {
                Some(snapshot) if reached(&snapshot) => Ok(snapshot),
                last => Err(format!("{last:?}")),
            },
        )
        .await
    }

    /// The retired slots, as `(agent_did, generation)`, whose task has joined
    /// since the last call.
    pub fn joined_retired_slots(&mut self) -> Vec<(String, u64)> {
        let mut joined = Vec::new();
        while let Ok(slot) = self.retired_slots.try_recv() {
            joined.push(slot);
        }
        joined
    }

    /// The next retired slot whose task joins; `last` describes the state the
    /// failure reports when none joins within [`STALL_CEILING`].
    pub async fn next_joined_retired_slot(
        &mut self,
        last: impl std::fmt::Display,
    ) -> (String, u64) {
        match tokio::time::timeout(STALL_CEILING, self.retired_slots.recv()).await {
            Ok(Some(slot)) => slot,
            _ => panic!("no retired slot joined within {STALL_CEILING:?}; last: {last}"),
        }
    }

    pub async fn shutdown(self) -> Result<()> {
        let _ = self.shutdown.send(true);
        tokio::time::timeout(STALL_CEILING, self.handle)
            .await
            .context("the runtime stops on shutdown")?
            .context("the runtime task joins")?
    }
}

struct RetiredSlotEvents(mpsc::UnboundedSender<(String, u64)>);

#[derive(Default)]
struct RetiredSlotFields {
    agent_did: Option<String>,
    generation: Option<u64>,
}

impl tracing::field::Visit for RetiredSlotFields {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "generation" {
            self.generation = Some(value);
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "agent_did" {
            self.agent_did = Some(format!("{value:?}"));
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for RetiredSlotEvents
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() != crate::runtime::status::RETIRED_SLOT_EVENT_TARGET {
            return;
        }
        let mut fields = RetiredSlotFields::default();
        event.record(&mut fields);
        if let (Some(agent_did), Some(generation)) = (fields.agent_did, fields.generation) {
            let _ = self.0.send((agent_did, generation));
        }
    }
}
