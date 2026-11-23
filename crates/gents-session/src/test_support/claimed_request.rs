use std::sync::Arc;
use std::time::Duration;

use gents_documents::test_support::SignedHome;
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_protocol::request_admission::RequestPurpose;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;

use crate::request::lifecycle::{
    ClaimOutcome, ExecutionBinding, ExecutionOrigin, RequestIdentity, RequestLifecycle,
    RequestSigner, RequestSpec, TriggerLineage,
};
use crate::request::streaming::DefraStreamWriter;

/// Who admits the request: the User's client signature, verified fresh
/// before the claim, or the Agent's own signature on a request it
/// materializes.
enum Admission {
    ClientSigned,
    AgentSelfSigned(TriggerLineage),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Admitted,
    Claimed,
    Executing,
}

/// One request on a [`SignedHome`]'s Agent, driven through the production
/// writers and the production claim (`claim_with_identity`) to `Stage`.
pub struct ClaimedRequest {
    pub lifecycle: RequestLifecycle,
    pub agent: Arc<dyn NodeIdentity>,
    pub agent_access: ConfigAccess,
    pub writer: Option<DefraStreamWriter>,
}

pub struct ClaimedRequestBuilder<'a> {
    home: &'a SignedHome,
    binding: &'a AgentIdentityBinding,
    admission: Admission,
    request_id: String,
    session_id: String,
    content: String,
    backend_id: Option<String>,
    lease: Option<Duration>,
}

impl ClaimedRequest {
    pub fn client_signed<'a>(
        home: &'a SignedHome,
        binding: &'a AgentIdentityBinding,
    ) -> ClaimedRequestBuilder<'a> {
        Self::builder(home, binding, Admission::ClientSigned)
    }

    pub fn self_signed<'a>(
        home: &'a SignedHome,
        binding: &'a AgentIdentityBinding,
    ) -> ClaimedRequestBuilder<'a> {
        Self::builder(
            home,
            binding,
            Admission::AgentSelfSigned(TriggerLineage::default()),
        )
    }

    fn builder<'a>(
        home: &'a SignedHome,
        binding: &'a AgentIdentityBinding,
        admission: Admission,
    ) -> ClaimedRequestBuilder<'a> {
        let id = uuid::Uuid::new_v4();
        ClaimedRequestBuilder {
            home,
            binding,
            admission,
            request_id: format!("request-{id}"),
            session_id: format!("session-{id}"),
            content: "hello".into(),
            backend_id: None,
            lease: None,
        }
    }

    pub fn writer(&self) -> &DefraStreamWriter {
        self.writer
            .as_ref()
            .expect("the request was driven to Stage::Executing")
    }
}

impl ClaimedRequestBuilder<'_> {
    pub fn caused_by(mut self, lineage: TriggerLineage) -> Self {
        if let Admission::AgentSelfSigned(caused) = &mut self.admission {
            *caused = lineage;
        }
        self
    }

    pub fn lease(mut self, lease: Duration) -> Self {
        self.lease = Some(lease);
        self
    }

    pub fn ids(mut self, request_id: &str, session_id: &str) -> Self {
        self.request_id = request_id.into();
        self.session_id = session_id.into();
        self
    }

    pub fn content(mut self, content: &str) -> Self {
        self.content = content.into();
        self
    }

    pub fn backend(mut self, backend_id: &str) -> Self {
        self.backend_id = Some(backend_id.into());
        self
    }

    pub async fn drive(self, stage: Stage) -> ClaimedRequest {
        let store = &self.home.store;
        let agent = gents_store::identity::resolve_agent_identity(
            store.node_identity.as_ref(),
            self.binding,
        )
        .unwrap();
        let agent_access = self.home.agent_access(self.binding);
        let mut lifecycle = match self.admission {
            Admission::ClientSigned => self.client_signed(agent.as_ref(), &agent_access).await,
            Admission::AgentSelfSigned(ref lineage) => {
                RequestLifecycle::materialize_pending_with_execution_binding(
                    store.node.clone(),
                    &self.binding.agent_did,
                    store.node_identity.clone(),
                    &self.content,
                    ExecutionBinding {
                        deadline_duration_secs: 60,
                        execution_origin: ExecutionOrigin::Interactive,
                        backend_id: self
                            .backend_id
                            .clone()
                            .unwrap_or_else(|| format!("{}:backend", self.binding.agent_did)),
                        operator_tool_root: None,
                    },
                    lineage.clone(),
                    Some(agent.clone()),
                )
                .await
                .unwrap()
            }
        };
        if let Some(lease) = self.lease {
            lifecycle.set_execution_lease_duration(lease);
        }
        let mut writer = None;
        if stage >= Stage::Claimed {
            assert_eq!(
                lifecycle.claim_with_identity().await.unwrap(),
                ClaimOutcome::Claimed
            );
        }
        if stage >= Stage::Executing {
            let stream = DefraStreamWriter::new_managed(
                store.node.clone(),
                agent.as_ref(),
                Duration::from_millis(1),
            )
            .unwrap();
            lifecycle.begin_owned_execution(&stream).await.unwrap();
            writer = Some(stream);
        }
        ClaimedRequest {
            lifecycle,
            agent,
            agent_access,
            writer,
        }
    }

    async fn client_signed(
        &self,
        agent: &dyn NodeIdentity,
        agent_access: &ConfigAccess,
    ) -> RequestLifecycle {
        let store = &self.home.store;
        let user = &self.home.user;
        let create = crate::request::lifecycle::build_signed_client_request(
            &self.home.user_access,
            RequestSpec::new(
                RequestPurpose::Normal,
                RequestIdentity {
                    request_id: self.request_id.clone(),
                    node_did: self.home.node_did().into(),
                    requester_did: Some(user.did().into()),
                    agent_did: agent.did().into(),
                    session_id: self.session_id.clone(),
                    content: self.content.clone(),
                    execution_origin: ExecutionOrigin::Interactive,
                    created_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                },
                gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
            ),
            RequestSigner::Identity(user),
        )
        .await
        .unwrap();
        crate::request::lifecycle::submit_client_request(&self.home.user_access, &create)
            .await
            .unwrap();
        let request_id = gents_store::graphql::escape_graphql_string(&self.request_id);
        let response = agent_access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {} }} }}"#,
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
            ))
            .await
            .unwrap();
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
        let (_authority_owner, authority) =
            gents_p2p::p2p_reconcile::enrollment_authority_channel();
        let (request, admission) = crate::request::admission::AgentRequestAdmissionVerifier::new(
            store.node.clone(),
            store.node_identity.clone(),
            authority,
        )
        .verify_fresh_for_claim(&row.try_into().unwrap(), agent.did())
        .await
        .unwrap();
        let mut lifecycle = RequestLifecycle::new_with_node_did(store.node.clone(), request, 60);
        lifecycle.bind_managed_agent(agent).unwrap();
        lifecycle.set_client_claim_admission(admission);
        lifecycle
    }
}
