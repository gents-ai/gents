//! The runtime booted as production boots it ([`boot_runtime`] over a served
//! home), its default Agent bound to a scripted HTTP model backend. Every wait
//! after boot follows a document change or a runtime event and fails with the
//! last state seen once nothing changes for [`STALL_CEILING`].

use std::sync::Arc;

use anyhow::{Context, Result};
use gents_documents::test_support::SignedHome;
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_protocol::node_readiness::NodeReadinessSnapshot;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_store::test_support::snapshot_reader::SnapshotReader as _;
use gents_store::test_support::{read_when_changed, STALL_CEILING};
use tokio::sync::mpsc;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::runtime::agent::DocumentRuntimeOptions;
use crate::runtime::embedded::{boot_runtime, RunningRuntime};

/// A signed home whose default Agent's inference profile names one
/// OpenAI-compatible backend at `endpoint`.
pub struct AgentRuntimeHome {
    pub home: SignedHome,
    pub binding: AgentIdentityBinding,
    pub agent: Arc<dyn NodeIdentity>,
}

impl AgentRuntimeHome {
    pub async fn start(name: &str, endpoint: &str, max_concurrent: i64) -> Self {
        let home = SignedHome::enroll(crate::support::trial_store(name).await, name).await;
        let binding = gents_inference::test_support::default_agent(&home).await;
        let agent = gents_store::identity::resolve_agent_identity(
            home.store.node_identity.as_ref(),
            &binding,
        )
        .unwrap();
        let runtime_home = Self {
            home,
            binding,
            agent,
        };
        runtime_home.bind_backend(endpoint, max_concurrent).await;
        runtime_home
    }

    fn backend_id(&self) -> String {
        format!("{}:backend", self.binding.agent_did)
    }

    /// Rewrites the Agent's backend document as its User; a change to what
    /// the Agent's slot is built from restages the slot.
    pub async fn bind_backend(&self, endpoint: &str, max_concurrent: i64) {
        use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        use gents_store::collection::Collection;
        let node_did = self.home.node_did();
        let backend_id = self.backend_id();
        let documents = [
            (
                Collection::InferenceProfile,
                serde_json::json!({
                    "node_did": node_did, "profile_id": format!("{}:inference", self.binding.agent_did),
                    "backend_id": backend_id, "model_name": "default"
                }),
            ),
            (
                Collection::InferenceBackend,
                serde_json::json!({
                    "node_did": node_did, "backend_id": backend_id, "name": "Scripted inference",
                    "provider_kind": "OpenAiCompatible", "endpoint": endpoint,
                    "max_concurrent": max_concurrent, "auth": {"kind": "unauthenticated"}
                }),
            ),
        ];
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
        self.home
            .user_access
            .transact_write("test.running_runtime.backend", |txn| {
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
        gents_inference::backends::registry::set_backend_probe_status_as(
            &self.home.node_access(),
            node_did,
            &backend_id,
            gents_inference::backends::registry::HEALTHY_PROBE_STATUS,
        )
        .await
        .unwrap();
    }

    /// Submits a request the User signs for the default Agent, titling its
    /// session so no title request precedes it, and returns its document ID.
    pub async fn submit_request(
        &self,
        request_id: &str,
        session_id: &str,
        content: &str,
    ) -> String {
        use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
        use gents_session::request::lifecycle::{RequestIdentity, RequestSigner, RequestSpec};
        let access = &self.home.user_access;
        let user_did = self.home.user.did();
        let node_did = self.home.node_did();
        let create = gents_session::request::lifecycle::build_signed_reserved_client_request(
            access,
            RequestSpec::new(
                RequestPurpose::Normal,
                RequestIdentity {
                    request_id: request_id.to_owned(),
                    node_did: node_did.to_owned(),
                    requester_did: Some(user_did.to_owned()),
                    agent_did: self.binding.agent_did.clone(),
                    session_id: session_id.to_owned(),
                    content: content.to_owned(),
                    execution_origin:
                        gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                    created_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                },
                AgentRequestAdmissionRecord::client(user_did),
            ),
            RequestSigner::Identity(&self.home.user),
        )
        .await
        .unwrap();
        gents_session::session::update_session_title_with_source(
            access,
            node_did,
            Some(user_did),
            session_id,
            "running runtime fixture",
            gents_protocol::session::SessionTitleSource::User,
        )
        .await
        .unwrap();
        gents_session::request::lifecycle::submit_client_request(access, &create)
            .await
            .unwrap();
        let request = escape_graphql_string(request_id);
        let response = access
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

/// The runtime running over an [`AgentRuntimeHome`]. It records each retired
/// Agent slot whose task has joined from the supervisor's own event, emitted
/// on the test's thread (`#[tokio::test]` polls every task there).
pub struct BootedRuntime {
    node: Arc<defra_node::Node>,
    runtime: RunningRuntime,
    retired_slots: mpsc::UnboundedReceiver<(String, u64)>,
    _events: tracing::subscriber::DefaultGuard,
}

impl BootedRuntime {
    pub async fn boot(home: &AgentRuntimeHome, options: DocumentRuntimeOptions) -> Self {
        let (retired_tx, retired_slots) = mpsc::unbounded_channel();
        let events = tracing::subscriber::set_default(
            tracing_subscriber::Registry::default().with(RetiredSlotEvents(retired_tx)),
        );
        let (runtime, _agent) = boot_runtime(
            &home.home.store,
            home.home.store.node_identity.clone(),
            options,
        )
        .await
        .expect("the runtime becomes ready");
        Self {
            node: home.home.store.node.clone(),
            runtime,
            retired_slots,
            _events: events,
        }
    }

    pub async fn readiness(&self) -> Option<NodeReadinessSnapshot> {
        let escaped = escape_graphql_string(&self.runtime.node_did);
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
        tokio::time::timeout(STALL_CEILING, self.runtime.shutdown())
            .await
            .context("the runtime stops on shutdown")?
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
