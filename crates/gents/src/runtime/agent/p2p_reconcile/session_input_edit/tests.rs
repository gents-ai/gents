use super::super::session_commands::{unanswered_commands, CommandKind, Sweep};
use super::*;
use crate::lean_vocab_test::{
    LeanQueueManagementState, LeanSessionInputEditAuthority, LeanSessionInputEditCommand,
    LeanSessionInputEditStep, LeanSessionInputEditor, QueueImage,
};
use crate::store::identity::KeyIdentity;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{
    QueueDelivery, QueuePolicy, QueueSource, RequestInput, RequestQueue,
};
use gents_protocol::session_input_edit::{PendingMessageEdit, PendingQueueEdit};
use rapidhash::{HashMapExt, RapidHashMap};

const SESSION: &str = "edit-session";

struct Fixture {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    endpoint: String,
    _dir: tempfile::TempDir,
    original: String,
    agent_did: String,
    agent_identity: Arc<dyn NodeIdentity>,
}

impl Fixture {
    async fn empty() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let identity: Arc<dyn NodeIdentity> =
            Arc::new(KeyIdentity::load_or_create(dir.path().join("node.key"), None).unwrap());
        let key = crate::store::identity::load_file_identity(&dir.path().join("node.key"))
            .unwrap()
            .private_key_bytes();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        // Edit authority is observed through native node permissions, which
        // exist only on a node with node ACP and its signing key.
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(dir.path())
                .with_node_identity_did(identity.did())
                .with_signing_key(defra_node::SigningKey::Ed25519(key))
                .with_node_acp_enabled()
                .with_http(defra_node::HttpConfig::with_listener(listener).unwrap())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let binding = crate::store::identity::provision_agent_identity(identity.as_ref()).unwrap();
        let access = ConfigAccess::Graphql(
            crate::store::config_client::GraphqlEndpoint::as_principal(&endpoint, identity.did())
                .with_client_signing_capability(identity.client_signing_capability().unwrap())
                .unwrap()
                .with_client_signed_agent_creates(),
        );
        crate::test_support::bootstrap_signed_test_node(&access, identity.did()).await;
        crate::test_support::install_test_agent_binding_with_access(
            identity.did(),
            "General",
            &binding,
            &access,
        )
        .await;
        // Production projects the AgentBaseline from each live Agent document;
        // this fixture has no projection pass, so the node admin grants it.
        crate::store::identity::GrantSet::AgentBaseline
            .grant(
                &ConfigAccess::local_as(
                    node.clone(),
                    ::identity::Did::new(identity.did()).unwrap(),
                ),
                &::identity::Did::new(&binding.agent_did).unwrap(),
            )
            .await
            .unwrap();
        let agent_identity =
            crate::store::identity::resolve_agent_identity(identity.as_ref(), &binding).unwrap();
        let agent_doc = access
            .transact_readonly("test.edit.agent", |txn| {
                Box::pin(async {
                    Ok::<_, anyhow::Error>(
                        crate::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            crate::Collection::Agent,
                            identity.did(),
                            &binding.agent_did,
                        )
                        .await?
                        .context("Agent missing")?
                        .0,
                    )
                })
            })
            .await
            .unwrap();
        let agent = ::identity::Did::new(&binding.agent_did).unwrap();
        access.transact_write("test.edit.session", |txn| Box::pin(async {
            let response = txn.execute_client_signed_create(
                "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                &serde_json::json!({"input": {"session_id":SESSION,"node_did":identity.did(),"requester_did":identity.did(),"agent_did":binding.agent_did,"relationships_pending":false,"created_at":"2026-10-10T00:00:00Z"}}),
                gents_protocol::schemas::AGENT_SESSION).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "AgentSession")?;
            txn.add_resource_relationship("AgentSession", &doc, "parent", "agents", &agent_doc).await?;
            txn.add_actor_relationship("AgentSession", &doc, "working_agent", &agent).await?;
            Ok::<_, anyhow::Error>(())
        })).await.unwrap();
        Self {
            node,
            identity,
            endpoint,
            _dir: dir,
            original: String::new(),
            agent_did: binding.agent_did,
            agent_identity,
        }
    }

    fn client_access(&self) -> ConfigAccess {
        ConfigAccess::Graphql(
            crate::store::config_client::GraphqlEndpoint::as_principal(
                &self.endpoint,
                self.identity.did(),
            )
            .with_client_signing_capability(self.identity.client_signing_capability().unwrap())
            .unwrap()
            .with_client_signed_agent_creates(),
        )
    }

    async fn new() -> Self {
        let mut fixture = Self::empty().await;
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "original",
            fixture.identity.did(),
            fixture.identity.did(),
            &fixture.agent_did,
            SESSION,
            "original content",
            "interactive",
            "2026-10-10T00:00:00Z",
            AgentRequestAdmissionRecord::client(fixture.identity.did()),
        );
        create.input = RequestInput {
            queue: Some(RequestQueue {
                delivery: QueueDelivery::Steer,
                position: None,
                source: QueueSource::User,
                policy: QueuePolicy::Append,
                key: None,
                queued_after_request_id: None,
                interrupted_request_id: None,
                background_completion_wake_version: None,
            }),
            ..Default::default()
        };
        fixture.original = fixture.write_request(create).await;
        fixture
    }

    async fn write_request(&self, mut create: AgentRequestCreate) -> String {
        let access = self.client_access();
        let (session, agent) = access
            .transact_readonly("test.edit.targets", |txn| {
                Box::pin(async move {
                    let session = crate::store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn,
                        self.identity.did(),
                        SESSION,
                    )
                    .await?
                    .context("session missing")?;
                    let (agent, _) = crate::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::Agent,
                        self.identity.did(),
                        &self.agent_did,
                    )
                    .await?
                    .context("agent missing")?;
                    Ok::<_, anyhow::Error>((session.doc_id, agent))
                })
            })
            .await
            .unwrap();
        create.session_doc_id = Some(session.clone());
        create.agent_doc_id = Some(agent.clone());
        crate::sign_agent_request_create(self.identity.as_ref(), &mut create)
            .await
            .unwrap();
        access
            .transact_write("test.edit.request", |txn| {
                let create = &create;
                let session = &session;
                let agent = &agent;
                Box::pin(async move {
                    crate::request::lifecycle::stage_client_request_in_txn(
                        txn, create, session, agent,
                    )
                    .await
                })
            })
            .await
            .unwrap()
    }

    async fn command(&self) -> SessionInputEdit {
        let edit = PendingQueueEdit {
            expected_request_doc_ids: vec![self.original.clone()],
            selected_request_doc_ids: vec![self.original.clone()],
            messages: vec![PendingMessageEdit {
                authored_input: None,
                request_doc_id: self.original.clone(),
                content: "corrected content".into(),
            }],
        };
        let replacements = crate::request::lifecycle::queue::prepare_pending_user_edit(
            &self.client_access(),
            self.identity.as_ref(),
            AgentRequestAdmissionRecord::client(self.identity.did()),
            self.identity.did(),
            SESSION,
            self.identity.did(),
            &edit,
        )
        .await
        .unwrap();
        let (session_doc_id, agent_did, agent_doc_id, replacement_evidence) =
            test_signed_replacement_fields(
                &self.client_access(),
                self.identity.as_ref(),
                self.identity.did(),
                SESSION,
                &replacements,
            )
            .await;
        let mut command = SessionInputEdit {
            version: SESSION_INPUT_EDIT_VERSION,
            command_id: uuid::Uuid::new_v4().to_string(),
            requester_did: self.identity.did().to_owned(),
            requester_peer_id: None,
            node_did: self.identity.did().to_owned(),
            session_id: SESSION.into(),
            session_doc_id,
            agent_did,
            agent_doc_id,
            issued_at: (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
            expires_at: (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
            edit,
            replacements,
            replacement_evidence,
            signature: Vec::new(),
        };
        command.signature = self
            .identity
            .sign(&command.signing_payload())
            .await
            .unwrap();
        command
    }

    async fn submit(&self, command: &SessionInputEdit) -> String {
        let access = self.client_access();
        let input = serde_json::json!({"command_id":command.command_id, "requester_did":command.requester_did,
            "requester_peer_id":command.requester_peer_id, "node_did":command.node_did,
            "session_id":command.session_id, "intent_json":serde_json::to_string(command).unwrap()});
        access.transact_write("test.edit.intent", |txn| {
            let input = &input;
            Box::pin(async move {
                let response = txn.execute_client_signed_create(
                    "mutation($input: AgentSessionInputEditMutationInputArg!) { create_AgentSessionInputEdit(input: $input) { _docID } }",
                    &serde_json::json!({"input":input}), gents_protocol::schemas::AGENT_SESSION_INPUT_EDIT,
                ).await?;
                let doc = crate::store::graphql::created_doc_id(&response, "AgentSessionInputEdit")?;
                txn.add_resource_relationship("AgentSessionInputEdit", &doc, "parent", "sessions", &command.session_doc_id).await?;
                txn.add_actor_relationship("AgentSessionInputEdit", &doc, "working_agent", &::identity::Did::new(&command.agent_did)?).await?;
                Ok::<_, anyhow::Error>(doc)
            })
        }).await.unwrap()
    }

    async fn receipt(&self, id: &str) -> SessionInputEditReceipt {
        let value = ConfigAccess::local_as(self.node.clone(), ::identity::Did::new(self.identity.did().to_owned()).unwrap()).execute(&format!(r#"{{ AgentSessionInputEdit(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#, escape_graphql_string(id))).await.unwrap();
        serde_json::from_str(
            value["data"]["AgentSessionInputEdit"][0]["receipt_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap()
    }

    async fn requests(&self) -> serde_json::Value {
        ConfigAccess::local_as(
            self.node.clone(),
            ::identity::Did::new(self.identity.did().to_owned()).unwrap(),
        )
        .execute(
            "{ AgentRequest { request_id content lifecycle_state superseded_by_request_doc_id } }",
        )
        .await
        .unwrap()["data"]["AgentRequest"]
            .clone()
    }
}

struct ModeledQueue {
    fixture: Fixture,
    request_ids: RapidHashMap<u64, String>,
    doc_ids: RapidHashMap<u64, String>,
    commands: RapidHashMap<(u64, u64), (String, SessionInputEdit)>,
    active: Option<crate::request::lifecycle::RequestLifecycle>,
    anchor: DateTime<Utc>,
    nows: Vec<u64>,
}

impl ModeledQueue {
    async fn seed(state: &LeanQueueManagementState, nows: Vec<u64>) -> Self {
        let fixture = Fixture::empty().await;
        let mut queue = Self {
            fixture,
            request_ids: RapidHashMap::new(),
            doc_ids: RapidHashMap::new(),
            commands: RapidHashMap::new(),
            active: None,
            anchor: Utc::now(),
            nows,
        };
        let did = queue.fixture.identity.did().to_owned();
        let request = |id: u64, origin: &str, arrival: usize| {
            AgentRequestCreate::base(
                RequestPurpose::Normal,
                id.to_string(),
                &did,
                &did,
                &queue.fixture.agent_did,
                SESSION,
                format!("message {id}"),
                origin,
                format!("2026-10-10T00:00:{arrival:02}Z"),
                AgentRequestAdmissionRecord::client(&did),
            )
        };
        let mut arrival = 0;
        if let Some(active) = state.active {
            let doc = queue
                .fixture
                .write_request(request(active, "interactive", arrival))
                .await;
            queue.request_ids.insert(active, active.to_string());
            queue.doc_ids.insert(active, doc);
            arrival += 1;
        }
        for entry in state.folding.iter().chain(&state.pending) {
            assert_eq!(entry.requester_id, Some(1), "requester 1 is the node");
            assert_eq!(entry.turn_context, 0);
            let mut create = request(entry.request_id, &entry.execution_origin, arrival);
            arrival += 1;
            create.input.queue = Some(RequestQueue {
                delivery: serde_json::from_value(serde_json::json!(entry.delivery)).unwrap(),
                position: None,
                source: serde_json::from_value(serde_json::json!(entry.source)).unwrap(),
                policy: serde_json::from_value(serde_json::json!(entry.policy)).unwrap(),
                key: None,
                queued_after_request_id: entry.queued_after.map(|id| id.to_string()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            });
            create.valid_until = (!entry.fresh).then(|| "2000-01-01T00:00:00Z".into());
            let doc = queue.fixture.write_request(create).await;
            queue
                .request_ids
                .insert(entry.request_id, entry.request_id.to_string());
            queue.doc_ids.insert(entry.request_id, doc);
        }
        if let Some(active) = state.active {
            let request = crate::request::binding::load_agent_request_by_doc_id(
                &queue.fixture.node,
                &queue.doc_ids[&active],
            )
            .await
            .unwrap()
            .unwrap();
            let (_owner, authority) =
                crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
            let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
                queue.fixture.node.clone(),
                queue.fixture.identity.clone(),
                authority,
            );
            let (request, admission) = verifier
                .verify_fresh_for_claim(&request, &queue.fixture.agent_did)
                .await
                .unwrap();
            assert!(admission.is_some());
            let mut lifecycle =
                crate::request::lifecycle::RequestLifecycle::new_with_execution_binding(
                    queue.fixture.node.clone(),
                    &queue.fixture.agent_did,
                    &did,
                    request,
                    600,
                    crate::request::lifecycle::ExecutionOrigin::Interactive,
                    String::new(),
                );
            lifecycle
                .bind_managed_agent(queue.fixture.agent_identity.as_ref())
                .unwrap();
            lifecycle.set_client_claim_admission(admission);
            lifecycle.set_fold_admitted(
                state
                    .folding
                    .iter()
                    .map(|entry| queue.doc_ids[&entry.request_id].clone())
                    .collect(),
            );
            assert_eq!(
                lifecycle.claim_with_identity().await.unwrap(),
                crate::request::lifecycle::ClaimOutcome::Claimed
            );
            queue.active = Some(lifecycle);
        }
        assert_eq!(queue.image().await, queue.modeled(state));
        queue.anchor = Utc::now();
        queue
    }

    fn modeled(&self, state: &LeanQueueManagementState) -> QueueImage {
        state.image(|id| self.request_ids[&id].clone())
    }

    async fn image(&self) -> QueueImage {
        crate::lean_vocab_test::native_queue_image(
            &self.fixture.node,
            self.fixture.identity.did(),
            SESSION,
            self.active
                .as_ref()
                .map_or(&[], |lifecycle| lifecycle.folded_selection()),
        )
        .await
    }

    fn instant(&self, model: u64) -> DateTime<Utc> {
        let offset = i64::try_from(model).unwrap();
        if self.nows.iter().all(|now| model <= *now) {
            self.anchor - chrono::Duration::hours(1) + chrono::Duration::seconds(offset)
        } else if self.nows.iter().all(|now| model > *now) {
            self.anchor + chrono::Duration::hours(1) + chrono::Duration::seconds(offset)
        } else {
            self.anchor + chrono::Duration::seconds(3) + chrono::Duration::milliseconds(offset)
        }
    }

    async fn wait_until(&self, now: u64) {
        if self.nows.iter().any(|earlier| *earlier < now) {
            if let Ok(wait) = (self.anchor + chrono::Duration::seconds(4) - Utc::now()).to_std() {
                tokio::time::sleep(wait).await;
            }
        }
    }

    async fn intent(
        &mut self,
        model: &LeanSessionInputEditCommand,
        authority: &LeanSessionInputEditAuthority,
    ) -> SessionInputEdit {
        assert!(!model.has_peer, "remote authority is bound by e2e_runtime");
        assert!(authority.command_identity_unique);
        let foreign;
        // The fixture's client key is the session's requester; a fresh key is
        // neither requester, working Agent nor admin.
        let requester: &dyn NodeIdentity = if authority.editor == LeanSessionInputEditor::Requester
        {
            self.fixture.identity.as_ref()
        } else {
            assert_eq!(
                authority.editor,
                LeanSessionInputEditor::Other,
                "generated local edit names an unrealized editor"
            );
            foreign = KeyIdentity::load_or_create(
                self.fixture
                    ._dir
                    .path()
                    .join(format!("requester-{}.key", model.id)),
                None,
            )
            .unwrap();
            &foreign
        };
        let pending = crate::request::lifecycle::pending_user_queue(
            &ConfigAccess::local_as(
                self.fixture.node.clone(),
                ::identity::Did::new(self.fixture.identity.did().to_owned()).unwrap(),
            ),
            self.fixture.identity.did(),
            SESSION,
            self.fixture.identity.did(),
        )
        .await
        .unwrap()
        .entries;
        let selected = pending
            .iter()
            .skip(model.offset)
            .take(model.count)
            .map(|entry| entry.request_doc_id.clone())
            .collect::<Vec<_>>();
        let mut edit = PendingQueueEdit {
            expected_request_doc_ids: pending
                .iter()
                .map(|entry| entry.request_doc_id.clone())
                .collect(),
            messages: model
                .replacements
                .iter()
                .zip(&selected)
                .map(|(replacement, doc)| PendingMessageEdit {
                    authored_input: None,
                    request_doc_id: doc.clone(),
                    content: format!("message {} intent {}", replacement.request_id, model.digest),
                })
                .collect(),
            selected_request_doc_ids: selected,
        };
        let mut replacements = crate::request::lifecycle::queue::prepare_pending_user_edit(
            &ConfigAccess::local_as(
                self.fixture.node.clone(),
                ::identity::Did::new(self.fixture.identity.did().to_owned()).unwrap(),
            ),
            self.fixture.identity.as_ref(),
            AgentRequestAdmissionRecord::client(self.fixture.identity.did()),
            self.fixture.identity.did(),
            SESSION,
            self.fixture.identity.did(),
            &edit,
        )
        .await
        .unwrap();
        if requester.did() != self.fixture.identity.did() {
            for create in &mut replacements {
                create.admission = AgentRequestAdmissionRecord::client(requester.did());
                crate::sign_agent_request_create(requester, create)
                    .await
                    .unwrap();
            }
        }
        edit.expected_request_doc_ids = model
            .expected
            .iter()
            .map(|id| self.doc_ids[id].clone())
            .collect();
        if !authority.replacements_valid {
            replacements[0].admission.signature[0] ^= 1;
        }
        let (session_doc_id, agent_did, agent_doc_id, replacement_evidence) =
            test_signed_replacement_fields(
                &ConfigAccess::local_as(
                    self.fixture.node.clone(),
                    ::identity::Did::new(self.fixture.identity.did()).unwrap(),
                ),
                requester,
                self.fixture.identity.did(),
                SESSION,
                &replacements,
            )
            .await;
        let mut command = SessionInputEdit {
            version: SESSION_INPUT_EDIT_VERSION,
            command_id: format!("command-{}", model.id),
            requester_did: requester.did().to_owned(),
            requester_peer_id: None,
            node_did: self.fixture.identity.did().to_owned(),
            session_id: if authority.session_owned {
                SESSION.into()
            } else {
                "another-session".into()
            },
            session_doc_id,
            agent_did,
            agent_doc_id,
            issued_at: self.instant(model.issued_at).to_rfc3339(),
            expires_at: self.instant(model.expires_at).to_rfc3339(),
            edit,
            replacements,
            replacement_evidence,
            signature: Vec::new(),
        };
        command.signature = requester.sign(&command.signing_payload()).await.unwrap();
        if !authority.signature_valid {
            command.signature[0] ^= 1;
        }
        command
    }

    async fn process(
        &mut self,
        now: u64,
        model: &LeanSessionInputEditCommand,
        authority: &LeanSessionInputEditAuthority,
    ) {
        self.wait_until(now).await;
        let doc = if let Some((doc, _)) = self.commands.get(&(model.id, model.digest)) {
            doc.clone()
        } else {
            let command = self.intent(model, authority).await;
            if let Some((_, (doc, _))) = self.commands.iter().find(|((id, _), _)| *id == model.id) {
                let doc = doc.clone();
                let written = ConfigAccess::local_as(self.fixture.node.clone(), ::identity::Did::new(self.fixture.identity.did().to_owned()).unwrap()).write("test.edit.collision", &format!(r#"mutation {{ create_AgentSessionInputEdit(input: {{ command_id: "{}", requester_did: "{}", node_did: "{}", session_id: "{}", intent_json: "{}" }}) {{ _docID }} }}"#,
                    escape_graphql_string(&command.command_id), escape_graphql_string(&command.requester_did), escape_graphql_string(&command.node_did), escape_graphql_string(&command.session_id), escape_graphql_string(&serde_json::to_string(&command).unwrap()))).await;
                assert!(written.is_err());
                doc
            } else {
                let doc = self.fixture.submit(&command).await;
                for (model, native) in model.replacements.iter().zip(&command.replacements) {
                    self.request_ids
                        .insert(model.request_id, native.request_id.clone());
                }
                self.commands
                    .insert((model.id, model.digest), (doc.clone(), command));
                doc
            }
        };
        process_command(
            &self.fixture.node,
            &self.fixture.identity,
            &doc,
            self.fixture.agent_identity.as_ref(),
        )
        .await
        .unwrap();
    }

    async fn lose_receipts(&self) {
        for (doc, _) in self.commands.values() {
            ConfigAccess::local_as(self.fixture.node.clone(), ::identity::Did::new(self.fixture.identity.did().to_owned()).unwrap()).write("test.edit.lose_receipt", &format!(r#"mutation {{ update_AgentSessionInputEdit(docID: "{}", input: {{ receipt_json: null }}) {{ _docID }} }}"#, escape_graphql_string(doc))).await.unwrap();
        }
    }

    async fn receipt_json(&self, doc: &str) -> Option<String> {
        let value = ConfigAccess::local_as(self.fixture.node.clone(), ::identity::Did::new(self.fixture.identity.did().to_owned()).unwrap()).execute(&format!(r#"{{ AgentSessionInputEdit(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#, escape_graphql_string(doc))).await.unwrap();
        value["data"]["AgentSessionInputEdit"][0]["receipt_json"]
            .as_str()
            .map(str::to_owned)
    }
}

#[tokio::test]
async fn unanswered_sweep_discovers_malformed_command_for_transactional_validation() {
    let fixture = Fixture::empty().await;
    let (session, _, _, _) = test_signed_replacement_fields(
        &fixture.client_access(),
        fixture.identity.as_ref(),
        fixture.identity.did(),
        SESSION,
        &[],
    )
    .await;
    let identity = &fixture.identity;
    let agent_did = &fixture.agent_did;
    let doc_id = fixture.client_access().transact_write("test.edit.malformed_intent", |txn| {
        let session = &session;
        Box::pin(async move {
            let response = txn.execute_client_signed_create(
                "mutation($input: AgentSessionInputEditMutationInputArg!) { create_AgentSessionInputEdit(input: $input) { _docID } }",
                &serde_json::json!({"input": {"command_id":"malformed", "requester_did":identity.did(), "node_did":identity.did(), "session_id":SESSION, "intent_json":"not-json"}}),
                gents_protocol::schemas::AGENT_SESSION_INPUT_EDIT,
            ).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "AgentSessionInputEdit")?;
            txn.add_resource_relationship("AgentSessionInputEdit", &doc, "parent", "sessions", session).await?;
            txn.add_actor_relationship("AgentSessionInputEdit", &doc, "working_agent", &::identity::Did::new(agent_did)?).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await.unwrap();

    let unanswered = unanswered_commands(
        CommandKind::InputEdit,
        &fixture.node,
        fixture.identity.as_ref(),
        fixture.agent_identity.as_ref(),
        Sweep::Unanswered,
    )
    .await
    .unwrap();
    assert_eq!(unanswered, vec![doc_id.clone()]);
    assert!(process_command(
        &fixture.node,
        &fixture.identity,
        &doc_id,
        fixture.agent_identity.as_ref()
    )
    .await
    .is_err());
}

#[tokio::test]
async fn generated_commands_bind_to_atomic_queue_owner() {
    let cases = crate::lean_vocab_test::lean_session_input_edit_cases()
        .iter()
        .filter(|case| {
            !case.command.has_peer
                && case.authority.command_identity_unique
                && case.history.iter().all(|step| match step {
                    LeanSessionInputEditStep::Apply {
                        command, authority, ..
                    } => !command.has_peer && authority.command_identity_unique,
                    LeanSessionInputEditStep::LoseReceipts => true,
                    LeanSessionInputEditStep::OtherSession { .. } => false,
                })
        })
        .collect::<Vec<_>>();
    assert_eq!(cases.len(), 11, "local generated input-edit cases");
    for case in cases {
        let name = &case.name;
        assert!(case.initial.receipts.is_empty());
        let mut nows = case
            .history
            .iter()
            .filter_map(|step| match step {
                LeanSessionInputEditStep::Apply { now, .. } => Some(*now),
                _ => None,
            })
            .collect::<Vec<_>>();
        nows.push(case.now);
        let mut queue = ModeledQueue::seed(&case.initial.queue, nows).await;
        for step in &case.history {
            match step {
                LeanSessionInputEditStep::Apply {
                    now,
                    authority,
                    command,
                } => queue.process(*now, command, authority).await,
                LeanSessionInputEditStep::LoseReceipts => queue.lose_receipts().await,
                LeanSessionInputEditStep::OtherSession { .. } => unreachable!(),
            }
        }
        assert_eq!(
            queue.image().await,
            queue.modeled(&case.before.queue),
            "{name}: history reaches the modeled starting queue"
        );
        let mut prior = RapidHashMap::new();
        for (doc, _) in queue.commands.values() {
            prior.insert(doc.clone(), queue.receipt_json(doc).await);
        }
        queue
            .process(case.now, &case.command, &case.authority)
            .await;
        assert_eq!(
            queue.image().await,
            queue.modeled(&case.after.queue),
            "{name}: queue state"
        );
        for ((id, digest), (doc, command)) in &queue.commands {
            let native = queue.receipt_json(doc).await;
            let Some(model) = case.after.receipts.iter().find(|r| r.command_id == *id) else {
                assert!(native.is_none(), "{name}: command {id} stays undecided");
                continue;
            };
            if case.before.receipts.iter().any(|r| r.command_id == *id) {
                assert_eq!(native, prior[doc], "{name}: recorded receipt is final");
            }
            assert_eq!(model.digest, *digest, "{name}: receipt intent");
            let receipt: SessionInputEditReceipt = serde_json::from_str(
                &native.unwrap_or_else(|| panic!("{name}: modeled receipt for command {id}")),
            )
            .unwrap();
            receipt.validate_for(command).unwrap();
            assert!(queue
                .fixture
                .identity
                .verify(
                    &receipt.node_did,
                    &receipt.signing_payload(),
                    &receipt.signature
                )
                .await
                .unwrap());
            assert_eq!(
                serde_json::to_value(receipt.outcome).unwrap(),
                model.outcome,
                "{name}: outcome"
            );
            assert_eq!(
                receipt.request_ids,
                model
                    .request_ids
                    .iter()
                    .map(|id| queue.request_ids[id].clone())
                    .collect::<Vec<_>>(),
                "{name}: receipt request IDs"
            );
        }
    }
}

#[tokio::test]
async fn invalid_receipt_cannot_suppress_a_valid_edit_and_stale_edit_never_writes_replacement() {
    let fixture = Fixture::new().await;
    let first = fixture.command().await;
    let stale = fixture.command().await;
    let first_id = fixture.submit(&first).await;
    let stale_id = fixture.submit(&stale).await;
    ConfigAccess::local(fixture.node.clone()).write("test.forged.receipt", &format!(r#"mutation {{ update_AgentSessionInputEdit(docID: "{}", input: {{ receipt_json: "{{}}" }}) {{ _docID }} }}"#, escape_graphql_string(&first_id))).await.unwrap();
    process_command(
        &fixture.node,
        &fixture.identity,
        &first_id,
        fixture.agent_identity.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(
        fixture.receipt(&first_id).await.outcome,
        SessionInputEditOutcome::Applied
    );
    let before = fixture.requests().await;
    process_command(
        &fixture.node,
        &fixture.identity,
        &stale_id,
        fixture.agent_identity.as_ref(),
    )
    .await
    .unwrap();
    let rejected = fixture.receipt(&stale_id).await;
    rejected.validate_for(&stale).unwrap();
    assert_eq!(rejected.outcome, SessionInputEditOutcome::Rejected);
    assert_eq!(
        fixture.requests().await,
        before,
        "losing replacement must not be created"
    );
}

#[tokio::test]
async fn valid_author_signature_cannot_install_replacement_with_wrong_admission_branch() {
    let fixture = Fixture::new().await;
    let mut command = fixture.command().await;
    command.replacements[0].admission = AgentRequestAdmissionRecord::peer(fixture.identity.did());
    crate::sign_agent_request_create(fixture.identity.as_ref(), &mut command.replacements[0])
        .await
        .unwrap();
    // The re-signed replacement carries its own valid graph evidence, so the
    // rejection comes from the admission branch rather than stale evidence.
    command.replacement_evidence = vec![crate::store::config_client::prepare_request_evidence(
        fixture.identity.as_ref(),
        crate::store::config_client::client_request_input(&command.replacements[0]).unwrap(),
    )
    .await
    .unwrap()];
    command.signature = fixture
        .identity
        .sign(&command.signing_payload())
        .await
        .unwrap();
    let id = fixture.submit(&command).await;
    let before = fixture.requests().await;
    process_command(
        &fixture.node,
        &fixture.identity,
        &id,
        fixture.agent_identity.as_ref(),
    )
    .await
    .unwrap();
    let receipt = fixture.receipt(&id).await;
    receipt.validate_for(&command).unwrap();
    assert_eq!(receipt.outcome, SessionInputEditOutcome::Rejected);
    assert!(
        receipt.reason.contains("replacement admission"),
        "{}",
        receipt.reason
    );
    assert_eq!(fixture.requests().await, before);
}

#[tokio::test]
async fn edit_effect_and_receipt_rollback_together_after_a_write_failure() {
    let fixture = Fixture::new().await;
    let command = fixture.command().await;
    let id = fixture.submit(&command).await;
    let before = fixture.requests().await;
    let (result, mutations) = ConfigApplyTxn::with_successful_mutation_failure_at(
        Some(1),
        process_command(
            &fixture.node,
            &fixture.identity,
            &id,
            fixture.agent_identity.as_ref(),
        ),
    )
    .await;
    assert!(result.is_err());
    assert!(mutations > 0, "fault must interrupt an actual mutation");
    assert_eq!(fixture.requests().await, before);
    let row = ConfigAccess::local(fixture.node.clone()).execute(&format!(r#"{{ AgentSessionInputEdit(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#, escape_graphql_string(&id))).await.unwrap();
    assert!(row["data"]["AgentSessionInputEdit"][0]["receipt_json"].is_null());
    process_command(
        &fixture.node,
        &fixture.identity,
        &id,
        fixture.agent_identity.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(
        fixture.receipt(&id).await.outcome,
        SessionInputEditOutcome::Applied
    );
}

#[tokio::test]
async fn cryptographically_invalid_receipt_cannot_suppress_valid_edit() {
    let fixture = Fixture::new().await;
    let command = fixture.command().await;
    let id = fixture.submit(&command).await;
    let mut forged = SessionInputEditReceipt {
        version: SESSION_INPUT_EDIT_VERSION,
        command_id: command.command_id.clone(),
        command_digest: command.computed_digest(),
        requester_did: command.requester_did.clone(),
        node_did: command.node_did.clone(),
        session_id: command.session_id.clone(),
        outcome: SessionInputEditOutcome::Rejected,
        reason: "forged rejection".into(),
        request_doc_ids: vec![],
        request_ids: vec![],
        processed_at: Utc::now().to_rfc3339(),
        signature: vec![],
    };
    forged.signature = fixture
        .identity
        .sign(&forged.signing_payload())
        .await
        .unwrap();
    forged.signature[0] ^= 1;
    forged.validate_for(&command).unwrap();
    ConfigAccess::local(fixture.node.clone()).write("test.invalid_signature_receipt", &format!(
        r#"mutation {{ update_AgentSessionInputEdit(docID: "{}", input: {{ receipt_json: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(&id), escape_graphql_string(&serde_json::to_string(&forged).unwrap())
    )).await.unwrap();
    process_command(
        &fixture.node,
        &fixture.identity,
        &id,
        fixture.agent_identity.as_ref(),
    )
    .await
    .unwrap();
    let actual = fixture.receipt(&id).await;
    actual.validate_for(&command).unwrap();
    assert_eq!(actual.outcome, SessionInputEditOutcome::Applied);
    assert!(fixture
        .identity
        .verify(
            &actual.node_did,
            &actual.signing_payload(),
            &actual.signature
        )
        .await
        .unwrap());
    assert_eq!(fixture.requests().await.as_array().unwrap().len(), 2);
}
