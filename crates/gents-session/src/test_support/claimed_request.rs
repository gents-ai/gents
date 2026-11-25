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
    Existing(String),
}

/// How far [`ClaimedRequestBuilder::drive`] takes the request. A submitted
/// client request is published and unverified; its lifecycle is bound to no
/// Agent, so a test can run the claim admission itself.
/// A test's edit to a client request's signed fields before signing.
type SpecEdit<'a> = Box<dyn FnOnce(&mut RequestSpec) + Send + 'a>;

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
    deadline_secs: u64,
    max_total_tokens: Option<u64>,
    max_request_retries: Option<u64>,
    fold_admitted: Vec<String>,
    steering_admission: bool,
    spec: Option<SpecEdit<'a>>,
}

impl ClaimedRequest {
    pub fn client_signed<'a>(
        home: &'a SignedHome,
        binding: &'a AgentIdentityBinding,
    ) -> ClaimedRequestBuilder<'a> {
        Self::builder(home, binding, Admission::ClientSigned { reserved: false })
    }

    /// The request already published as `doc_id`, verified fresh and
    /// claimed as the Agent claims a row it finds.
    pub fn existing<'a>(
        home: &'a SignedHome,
        binding: &'a AgentIdentityBinding,
        doc_id: &str,
    ) -> ClaimedRequestBuilder<'a> {
        Self::builder(home, binding, Admission::Existing(doc_id.to_owned()))
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
            deadline_secs: 60,
            max_total_tokens: None,
            max_request_retries: None,
            fold_admitted: Vec::new(),
            steering_admission: false,
            spec: None,
        }
    }

    pub fn writer(&self) -> &DefraStreamWriter {
        self.writer
            .as_ref()
            .expect("the request was driven to Stage::Executing")
    }

    pub async fn admit_request(
        node: Arc<defra_node::Node>,
        runtime: Arc<dyn NodeIdentity>,
        request: &gents_protocol::row::AgentRequest,
        agent: &dyn NodeIdentity,
        deadline_secs: u64,
        origin: ExecutionOrigin,
        backend_id: &str,
    ) -> anyhow::Result<RequestLifecycle> {
        let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
        let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            runtime,
            authority,
        );
        let (request, admission) = verifier
            .verify_fresh_for_claim_with_identity(request, agent)
            .await?;
        let mut lifecycle = RequestLifecycle::new_with_execution_binding(
            node,
            request,
            deadline_secs,
            origin,
            backend_id,
        );
        lifecycle.bind_managed_agent(agent)?;
        lifecycle.set_client_claim_admission(admission);
        Ok(lifecycle)
    }
}

impl<'a> ClaimedRequestBuilder<'a> {
    /// Sets any further signed field of a client request (trigger lineage,
    /// parent, goal input) before it is signed.
    pub fn spec(mut self, edit: impl FnOnce(&mut RequestSpec) + Send + 'a) -> Self {
        self.spec = Some(Box::new(edit));
        self
    }

    pub fn deadline_secs(mut self, secs: u64) -> Self {
        self.deadline_secs = secs;
        self
    }

    /// The runtime-configured token limit the claim pins.
    pub fn max_total_tokens(mut self, limit: u64) -> Self {
        self.max_total_tokens = Some(limit);
        self
    }

    /// The Agent's resolved retry budget, pinned by the Node at the claim.
    pub fn max_request_retries(mut self, budget: u64) -> Self {
        self.max_request_retries = Some(budget);
        self
    }

    /// Pending requests whose admission the test verified for this claim to
    /// fold.
    pub fn fold_admitted(mut self, admitted: Vec<String>) -> Self {
        self.fold_admitted = admitted;
        self
    }

    /// The claim verifies steering input through the production verifier.
    pub fn steering_admission(mut self) -> Self {
        self.steering_admission = true;
        self
    }

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
        let admission = std::mem::replace(
            &mut self.admission,
            Admission::ClientSigned { reserved: false },
        );
        let (doc_id, mut lifecycle) = match admission {
            Admission::ClientSigned { reserved } => {
                let spec = self.spec.take();
                self.publish_client(agent.as_ref(), reserved, retry, spec)
                    .await;
                let request_id = gents_store::graphql::escape_graphql_string(&self.request_id);
                self.admit(
                    &format!(r#"request_id: {{ _eq: "{request_id}" }}"#),
                    &agent,
                    &agent_access,
                    stage,
                )
                .await
            }
            Admission::Existing(ref doc_id) => {
                let doc_id = gents_store::graphql::escape_graphql_string(doc_id);
                self.admit(
                    &format!(r#"_docID: {{ _eq: "{doc_id}" }}"#),
                    &agent,
                    &agent_access,
                    stage,
                )
                .await
            }
            Admission::AgentSelfSigned(ref lineage) => {
                let lifecycle = RequestLifecycle::materialize_pending_with_execution_binding(
                    store.node.clone(),
                    &self.binding.agent_did,
                    store.node_identity.clone(),
                    &self.content,
                    ExecutionBinding {
                        deadline_duration_secs: self.deadline_secs,
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
        if stage > Stage::Submitted {
            self.configure(&mut lifecycle, &agent);
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

    fn configure(&self, lifecycle: &mut RequestLifecycle, agent: &Arc<dyn NodeIdentity>) {
        let store = &self.home.store;
        if let Some(limit) = self.max_total_tokens {
            lifecycle.set_configured_max_total_tokens(Some(limit));
        }
        if let Some(budget) = self.max_request_retries {
            lifecycle.set_configured_max_request_retries(budget, store.node_identity.clone());
        }
        if !self.fold_admitted.is_empty() {
            lifecycle.set_fold_admitted(self.fold_admitted.clone());
        }
        if self.steering_admission {
            lifecycle.set_steering_admission(self.verifier(), agent.clone());
        }
    }

    fn verifier(&self) -> crate::request::admission::AgentRequestAdmissionVerifier {
        let store = &self.home.store;
        let (_authority_owner, authority) =
            gents_p2p::p2p_reconcile::enrollment_authority_channel();
        crate::request::admission::AgentRequestAdmissionVerifier::new(
            store.node.clone(),
            store.node_identity.clone(),
            authority,
        )
    }

    async fn publish_client(
        &self,
        agent: &dyn NodeIdentity,
        reserved: bool,
        retry: Option<RetryLink>,
        edit: Option<SpecEdit<'a>>,
    ) {
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
        if let Some(edit) = edit {
            edit(&mut spec);
        }
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
    }

    /// Reads the one request matching `filter` as the Agent and, past
    /// [`Stage::Submitted`], verifies it fresh for the claim and binds the
    /// Agent with the admission the verifier returns.
    async fn admit(
        &self,
        filter: &str,
        agent: &Arc<dyn NodeIdentity>,
        agent_access: &ConfigAccess,
        stage: Stage,
    ) -> (String, RequestLifecycle) {
        let store = &self.home.store;
        let response = agent_access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ {filter} }}, limit: 2) {{ {} }} }}"#,
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
            ))
            .await
            .unwrap();
        let [row] = response["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .as_slice()
        else {
            panic!("exactly one request matches {filter}: {response:?}");
        };
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(row.clone()).unwrap();
        let doc_id = row
            .doc_id
            .clone()
            .expect("the published request has its document id");
        let request = row.try_into().unwrap();
        let backend = self.backend_id.clone().unwrap_or_default();
        if stage == Stage::Submitted {
            let lifecycle = RequestLifecycle::new_with_execution_binding(
                store.node.clone(),
                request,
                self.deadline_secs,
                self.origin,
                backend,
            );
            return (doc_id, lifecycle);
        }
        let lifecycle = ClaimedRequest::admit_request(
            store.node.clone(),
            store.node_identity.clone(),
            &request,
            agent.as_ref(),
            self.deadline_secs,
            self.origin,
            &backend,
        )
        .await
        .unwrap();
        (doc_id, lifecycle)
    }
}
