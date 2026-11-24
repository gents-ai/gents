use std::sync::Arc;
use std::time::Duration;

use gents_documents::test_support::SignedHome;
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::request_input::RequestInput;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;

use crate::request::lifecycle::{
    ClaimOutcome, ExecutionBinding, ExecutionOrigin, RequestIdentity, RequestLifecycle,
    RequestSigner, RequestSpec, RetryLink, TriggerLineage,
};
use crate::request::streaming::DefraStreamWriter;

/// Who admits the request: the User's client signature, verified fresh
/// before the claim, or the Agent's own signature on a request it
/// materializes.
enum Admission {
    ClientSigned { reserved: bool },
    AgentSelfSigned(TriggerLineage),
}

/// How far [`ClaimedRequestBuilder::drive`] takes the request. A submitted
/// client request is published and unverified; its lifecycle is bound to no
/// Agent, so a test can run the claim admission itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Submitted,
    Admitted,
    Claimed,
    Executing,
}

/// One request on a [`SignedHome`]'s Agent, driven through the production
/// writers and the production claim (`claim_with_identity`) to `Stage`.
pub struct ClaimedRequest {
    pub doc_id: String,
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
    origin: ExecutionOrigin,
    created_at: Option<String>,
    input: RequestInput,
    retry: Option<RetryLink>,
    backend_id: Option<String>,
    lease: Option<Duration>,
    batch_interval: Duration,
}

impl ClaimedRequest {
    pub fn client_signed<'a>(
        home: &'a SignedHome,
        binding: &'a AgentIdentityBinding,
    ) -> ClaimedRequestBuilder<'a> {
        Self::builder(home, binding, Admission::ClientSigned { reserved: false })
    }

    /// A client request signed against a session the User reserved first, as
    /// a Goal start signs its command against a committed session.
    pub fn client_signed_reserved<'a>(
        home: &'a SignedHome,
        binding: &'a AgentIdentityBinding,
    ) -> ClaimedRequestBuilder<'a> {
        Self::builder(home, binding, Admission::ClientSigned { reserved: true })
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
            origin: ExecutionOrigin::Interactive,
            created_at: None,
            input: RequestInput::default(),
            retry: None,
            backend_id: None,
            lease: None,
            batch_interval: Duration::from_millis(1),
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

    /// An unattended request runs with [`ExecutionOrigin::Scheduled`].
    pub fn origin(mut self, origin: ExecutionOrigin) -> Self {
        self.origin = origin;
        self
    }

    pub fn created_at(mut self, created_at: &str) -> Self {
        self.created_at = Some(created_at.into());
        self
    }

    /// The signed input, such as a queue or steer delivery
    /// (`RequestInput::queue`).
    pub fn input(mut self, input: RequestInput) -> Self {
        self.input = input;
        self
    }

    /// A client request that retries an earlier one.
    pub fn retry(mut self, retry: RetryLink) -> Self {
        self.retry = Some(retry);
        self
    }

    /// The executing writer's batch interval.
    pub fn batch_interval(mut self, interval: Duration) -> Self {
        self.batch_interval = interval;
        self
    }

    pub fn backend(mut self, backend_id: &str) -> Self {
        self.backend_id = Some(backend_id.into());
        self
    }

    pub async fn drive(mut self, stage: Stage) -> ClaimedRequest {
        let retry = self.retry.take();
        let store = &self.home.store;
        let agent = gents_store::identity::resolve_agent_identity(
            store.node_identity.as_ref(),
            self.binding,
        )
        .unwrap();
        let agent_access = self.home.agent_access(self.binding);
        let (doc_id, mut lifecycle) = match self.admission {
            Admission::ClientSigned { reserved } => {
                self.client_signed(agent.as_ref(), &agent_access, reserved, retry, stage)
                    .await
            }
            Admission::AgentSelfSigned(ref lineage) => {
                let lifecycle = RequestLifecycle::materialize_pending_with_execution_binding(
                    store.node.clone(),
                    &self.binding.agent_did,
                    store.node_identity.clone(),
                    &self.content,
                    ExecutionBinding {
                        deadline_duration_secs: 60,
                        execution_origin: self.origin,
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
                .unwrap();
                (lifecycle.request().doc_id.clone(), lifecycle)
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
                self.batch_interval,
            )
            .unwrap();
            lifecycle.begin_owned_execution(&stream).await.unwrap();
            writer = Some(stream);
        }
        ClaimedRequest {
            doc_id,
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
        reserved: bool,
        retry: Option<RetryLink>,
        stage: Stage,
    ) -> (String, RequestLifecycle) {
        let store = &self.home.store;
        let user = &self.home.user;
        let mut spec = RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                request_id: self.request_id.clone(),
                node_did: self.home.node_did().into(),
                requester_did: Some(user.did().into()),
                agent_did: agent.did().into(),
                session_id: self.session_id.clone(),
                content: self.content.clone(),
                execution_origin: self.origin,
                created_at: self.created_at.clone().unwrap_or_else(|| {
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                }),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
        );
        spec.input = self.input.clone();
        spec.retry = retry;
        let signer = RequestSigner::Identity(user);
        let create = if reserved {
            crate::request::lifecycle::build_signed_reserved_client_request(
                &self.home.user_access,
                spec,
                signer,
            )
            .await
        } else {
            crate::request::lifecycle::build_signed_client_request(
                &self.home.user_access,
                spec,
                signer,
            )
            .await
        }
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
        let doc_id = row
            .doc_id
            .clone()
            .expect("the published request has its document id");
        if stage == Stage::Submitted {
            let lifecycle = RequestLifecycle::new_with_node_did(
                store.node.clone(),
                row.try_into().unwrap(),
                60,
            );
            return (doc_id, lifecycle);
        }
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
        (doc_id, lifecycle)
    }
}
