//! The runtime booted as production boots it ([`boot_runtime`] over a served
//! home), its default Agent bound to a scripted HTTP model backend. Every wait
//! after boot follows a document change or a runtime event and fails with the
//! last state seen once nothing changes for [`STALL_CEILING`].

use std::sync::Arc;

use anyhow::{Context, Result};
use gents_automation::triggers::snapshot_observer::RuntimeSnapshotObserver;
use gents_documents::test_support::SignedHome;
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_protocol::node_readiness::NodeReadinessSnapshot;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_store::test_support::snapshot_reader::SnapshotReader as _;
use gents_store::test_support::{read_when_changed, STALL_CEILING};
use tokio::sync::mpsc;

use crate::runtime::agent::DocumentRuntimeOptions;
use crate::runtime::embedded::{boot_runtime, RunningRuntime};

/// A signed home whose default Agent's inference profile names one
/// OpenAI-compatible backend at an endpoint: the Agent's own backend, or the
/// one a test names.
pub struct AgentRuntimeHome {
    pub home: SignedHome,
    pub binding: AgentIdentityBinding,
    pub agent: Arc<dyn NodeIdentity>,
    backend_id: String,
    wire_api: Option<gents_loop::openai_wire::OpenAiWireApi>,
}

impl AgentRuntimeHome {
    /// The home with its backend's probe status recorded as `probe_status`.
    pub async fn start(
        name: &str,
        backend_id: Option<&str>,
        endpoint: &str,
        max_concurrent: i64,
        probe_status: &str,
    ) -> Self {
        Self::start_with_wire_api(
            name,
            backend_id,
            endpoint,
            max_concurrent,
            probe_status,
            None,
        )
        .await
    }

    pub async fn start_with_wire_api(
        name: &str,
        backend_id: Option<&str>,
        endpoint: &str,
        max_concurrent: i64,
        probe_status: &str,
        wire_api: Option<gents_loop::openai_wire::OpenAiWireApi>,
    ) -> Self {
        let home = SignedHome::enroll(crate::support::trial_store(name).await, name).await;
        let binding = gents_inference::test_support::default_agent(&home).await;
        let agent = gents_store::identity::resolve_agent_identity(
            home.store.node_identity.as_ref(),
            &binding,
        )
        .unwrap();
        let backend_id =
            backend_id.map_or_else(|| format!("{}:backend", binding.agent_did), str::to_owned);
        let runtime_home = Self {
            home,
            binding,
            agent,
            backend_id,
            wire_api,
        };
        runtime_home
            .write_backend(endpoint, max_concurrent, probe_status)
            .await;
        runtime_home
    }

    /// The Agent's own access, which reads the documents it owns.
    pub fn agent_access(&self) -> gents_store::config_client::ConfigAccess {
        self.access_for_agent(self.agent.as_ref())
    }

    /// Managed-agent access for any Agent identity provisioned on this home.
    pub fn access_for_agent(
        &self,
        agent: &dyn NodeIdentity,
    ) -> gents_store::config_client::ConfigAccess {
        gents_store::config_client::ConfigAccess::local_managed_agent(
            self.home.node.clone(),
            agent.did().parse().unwrap(),
            agent,
        )
        .unwrap()
    }

    /// Provisions an additional Agent on this home with its own backend and
    /// auth configuration, through the User's signed desired-state transaction.
    pub async fn add_agent_with_backend(
        &self,
        backend_id: &str,
        endpoint: &str,
        max_concurrent: i64,
        auth: serde_json::Value,
        probe_status: &str,
    ) -> (AgentIdentityBinding, Arc<dyn NodeIdentity>) {
        use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        use gents_store::collection::Collection;
        let node_did = self.home.node_did();
        let binding = gents_store::identity::provision_agent_identity_authorized(
            &self.home.node,
            self.home.store.node_identity.as_ref(),
            &self.home.user.did().parse().unwrap(),
        )
        .await
        .unwrap();
        let agent = gents_store::identity::resolve_agent_identity(
            self.home.store.node_identity.as_ref(),
            &binding,
        )
        .unwrap();
        let agent_did = binding.agent_did.as_str();
        let context_id = format!("{agent_did}:context");
        let tools_id = format!("{agent_did}:tools");
        let profile_id = format!("{agent_did}:inference");
        let documents = [
            (
                Collection::Agent,
                serde_json::json!({
                    "node_did": node_did,
                    "agent_did": agent_did,
                    "creation_nonce": binding.creation_nonce,
                    "context_id": context_id,
                    "inference_profile_id": profile_id,
                }),
            ),
            (
                Collection::AgentContext,
                serde_json::json!({
                    "node_did": node_did,
                    "context_id": context_id,
                    "tools_id": tools_id,
                }),
            ),
            (
                Collection::Tools,
                serde_json::json!({
                    "node_did": node_did,
                    "tools_id": tools_id,
                }),
            ),
            (
                Collection::InferenceProfile,
                serde_json::json!({
                    "node_did": node_did,
                    "profile_id": profile_id,
                    "backend_id": backend_id,
                    "model_name": "default",
                }),
            ),
            (
                Collection::InferenceBackend,
                serde_json::json!({
                    "node_did": node_did,
                    "backend_id": backend_id,
                    "name": backend_id,
                    "provider_kind": "OpenAiCompatible",
                    "endpoint": endpoint,
                    "max_concurrent": max_concurrent,
                    "auth": auth,
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
            .transact_signed_conflict_rebuild("test.running_runtime.add_agent", |txn| {
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
            backend_id,
            probe_status,
        )
        .await
        .unwrap();
        (binding, agent)
    }

    /// Provisions an additional Agent on this home sharing the default Agent's
    /// inference profile, with custom `Tools` fields merged into its tools doc.
    pub async fn add_agent_with_tools(
        &self,
        extra_tools_fields: serde_json::Value,
    ) -> (AgentIdentityBinding, Arc<dyn NodeIdentity>) {
        use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        use gents_store::collection::Collection;
        let node_did = self.home.node_did();
        let binding = gents_store::identity::provision_agent_identity_authorized(
            &self.home.node,
            self.home.store.node_identity.as_ref(),
            &self.home.user.did().parse().unwrap(),
        )
        .await
        .unwrap();
        let agent = gents_store::identity::resolve_agent_identity(
            self.home.store.node_identity.as_ref(),
            &binding,
        )
        .unwrap();
        let agent_did = binding.agent_did.as_str();
        let context_id = format!("{agent_did}:context");
        let tools_id = format!("{agent_did}:tools");
        let mut tools = serde_json::json!({
            "node_did": node_did,
            "tools_id": tools_id,
        });
        if let (Some(target), Some(source)) =
            (tools.as_object_mut(), extra_tools_fields.as_object())
        {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        }
        let documents = [
            (
                Collection::Agent,
                serde_json::json!({
                    "node_did": node_did,
                    "agent_did": agent_did,
                    "creation_nonce": binding.creation_nonce,
                    "context_id": context_id,
                    "inference_profile_id": format!("{}:inference", self.binding.agent_did),
                }),
            ),
            (
                Collection::AgentContext,
                serde_json::json!({
                    "node_did": node_did,
                    "context_id": context_id,
                    "tools_id": tools_id,
                }),
            ),
            (Collection::Tools, tools),
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
            .transact_signed_conflict_rebuild("test.running_runtime.add_agent_tools", |txn| {
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
        (binding, agent)
    }

    /// Rewrites the Agent's backend document as its User, probed healthy; a
    /// change to what the Agent's slot is built from restages the slot. The
    /// User's write rebuilds on a conflict, as a client's configuration apply
    /// does: the running runtime records its probes on the same backend.
    pub async fn bind_backend(&self, endpoint: &str, max_concurrent: i64) {
        self.write_backend(
            endpoint,
            max_concurrent,
            gents_inference::backends::registry::HEALTHY_PROBE_STATUS,
        )
        .await;
    }

    async fn write_backend(&self, endpoint: &str, max_concurrent: i64, probe_status: &str) {
        use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        use gents_store::collection::Collection;
        let node_did = self.home.node_did();
        let backend_id = self.backend_id.as_str();
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
                    "node_did": node_did, "backend_id": backend_id, "name": backend_id,
                    "provider_kind": "OpenAiCompatible", "endpoint": endpoint, "openai_wire_api": self.wire_api,
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
            .transact_signed_conflict_rebuild("test.running_runtime.backend", |txn| {
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
            backend_id,
            probe_status,
        )
        .await
        .unwrap();
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

/// The runtime running over an [`AgentRuntimeHome`]. It observes the
/// runtime's snapshot owner, which reports each retired Agent slot whose task
/// has joined.
pub struct BootedRuntime {
    node: Arc<defra_node::Node>,
    node_did: String,
    runtime: Option<RunningRuntime>,
    signal_shutdown_on_drop: bool,
    retired_slots: mpsc::UnboundedReceiver<(String, u64)>,
}

impl BootedRuntime {
    pub async fn boot(home: &AgentRuntimeHome, options: DocumentRuntimeOptions) -> Self {
        Self::boot_with_home(
            &home.home.store,
            home.home.store.node_identity.clone(),
            options,
        )
        .await
    }

    pub async fn boot_with_home(
        home: &gents_store::local::home::ServedHome,
        identity: Arc<dyn NodeIdentity>,
        mut options: DocumentRuntimeOptions,
    ) -> Self {
        let (retired_tx, retired_slots) = mpsc::unbounded_channel();
        assert!(
            options.runtime_snapshot_observer.is_none(),
            "the booted runtime observes its own snapshot owner"
        );
        options.runtime_snapshot_observer = Some(Arc::new(RetiredSlots(retired_tx)));
        let (runtime, _agent) = boot_runtime(home, identity, options)
            .await
            .expect("the runtime becomes ready");
        let node_did = runtime.node_did.clone();
        Self {
            node: home.node.clone(),
            node_did,
            runtime: Some(runtime),
            signal_shutdown_on_drop: true,
            retired_slots,
        }
    }

    pub fn node_did(&self) -> &str {
        &self.node_did
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

    pub async fn shutdown(mut self) -> Result<()> {
        let Some(runtime) = self.runtime.take() else {
            return Ok(());
        };
        tokio::time::timeout(STALL_CEILING, runtime.shutdown())
            .await
            .context("the runtime stops on shutdown")?
    }

    pub async fn crash(mut self) {
        self.signal_shutdown_on_drop = false;
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        runtime.handle.abort();
        let error = runtime
            .handle
            .await
            .expect_err("aborted runtime task must be cancelled");
        assert!(error.is_cancelled(), "aborted runtime task: {error}");
    }
}

impl Drop for BootedRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            if self.signal_shutdown_on_drop {
                let _ = runtime.shutdown.send(true);
            }
            runtime.handle.abort();
        }
    }
}

pub(crate) struct RetiredSlots(pub(crate) mpsc::UnboundedSender<(String, u64)>);

impl RuntimeSnapshotObserver for RetiredSlots {
    fn on_generation_published(&self, _: u64, _: &str, _: &[String]) {}

    fn on_event_sources_reconciled(&self, _: u64, _: &str, _: Result<(), &str>) {}

    fn on_retired_slot_joined(&self, agent_did: &str, generation: u64) {
        let _ = self.0.send((agent_did.to_owned(), generation));
    }
}
