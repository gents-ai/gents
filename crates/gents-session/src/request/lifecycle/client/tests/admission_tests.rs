use super::*;
use gents_lean_vocab::families::contracts_atomic_admission_client as family;
use gents_store::config_client::{ConfigApplyTxn, RelationshipMatch};

/// The served node, its User and selected Agent, and the numbers the model
/// gives them.
struct Served {
    db: gents_inference::test_support::TestDb,
    runtime: KeyIdentity,
    user: KeyIdentity,
    access: ConfigAccess,
    agent_did: String,
    other_agent_did: String,
}

impl Served {
    fn actor(&self, did: &str) -> u64 {
        if did == self.user.did() {
            2
        } else if did == self.agent_did {
            3
        } else {
            0
        }
    }

    fn agent(&self, number: u64) -> &str {
        if number == 3 {
            &self.agent_did
        } else {
            &self.other_agent_did
        }
    }

    fn create(&self, session_id: &str, request_id: &str, agent: u64) -> AgentRequestCreate {
        AgentRequestCreate::base(
            RequestPurpose::Normal,
            request_id,
            RequestScope::new(
                self.runtime.did(),
                self.user.did(),
                self.agent(agent),
                session_id,
            ),
            "admit",
            "interactive",
            "2026-11-04T00:00:00Z",
            AgentRequestAdmissionRecord::client(self.user.did()),
        )
    }

    async fn existing_targets(&self, session_id: &str) -> (String, String) {
        let (node, agent) = (self.runtime.did(), self.agent_did.as_str());
        self.access
            .transact_readonly("test.admission.targets", |txn| {
                Box::pin(async move {
                    let session =
                        gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                            txn, node, session_id,
                        )
                        .await?
                        .context("admission case session is missing")?;
                    let (agent, _) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Agent,
                        node,
                        agent,
                    )
                    .await?
                    .context("admission case Agent is missing")?;
                    Ok::<_, anyhow::Error>((session.doc_id, agent))
                })
            })
            .await
            .unwrap()
    }

    async fn sign(&self, create: &mut AgentRequestCreate, targets: (String, String)) {
        create.session_doc_id = Some(targets.0);
        create.agent_doc_id = Some(targets.1);
        crate::request::admission::sign_agent_request_create(&self.user, create)
            .await
            .unwrap();
    }

    async fn owner(&self, collection: &'static str, doc: &str) -> Option<u64> {
        let doc = doc.to_owned();
        let owners = self
            .access
            .transact_readonly("test.admission.owner", |txn| {
                let doc = doc.clone();
                Box::pin(async move { txn.document_owners(collection, &[doc]).await })
            })
            .await
            .unwrap();
        owners
            .first()
            .and_then(|owner| owner.owner.as_ref())
            .map(|owner| self.actor(owner.as_str()))
    }

    async fn relationships(
        &self,
        collection: &str,
        doc: &str,
        docs: &[(&str, u64)],
    ) -> (Option<u64>, Option<u64>, Option<u64>) {
        let rows = self
            .access
            .list_document_relationships(collection, doc)
            .await
            .unwrap();
        let target = |relation: &str| {
            rows.iter().find(|row| row.relation == relation).map(|row| {
                docs.iter()
                    .find(|(doc, _)| row.subject.contains(doc))
                    .map_or(0, |(_, number)| *number)
            })
        };
        let worker = rows
            .iter()
            .find(|row| row.relation == "working_agent")
            .map(|row| self.actor(&row.subject));
        (target("parent"), target("session"), worker)
    }

    /// The stored session and the newest request of `session_id`, named by
    /// the case's numbers.
    async fn observe(
        &self,
        session_id: &str,
        targets: &(String, String),
        input: &serde_json::Value,
    ) -> family::AdmissionObservation {
        let escaped = gents_store::graphql::escape_graphql_string(session_id);
        let rows = self
            .access
            .execute(&format!(
                r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ _docID provenance }}
                     AgentRequest(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ _docID request_id }} }}"#
            ))
            .await
            .unwrap();
        let number = |field: &str| input[field].as_u64();
        let session_doc = number("session_doc").unwrap();
        let docs = [
            (targets.0.as_str(), session_doc),
            (targets.1.as_str(), number("agent_doc").unwrap()),
        ];
        let sessions = rows["data"]["AgentSession"].as_array().unwrap();
        assert!(sessions.len() <= 1, "one session per scope");
        let session = sessions.first();
        let session_doc_id = session.map(|row| row["_docID"].as_str().unwrap().to_owned());
        let (session_parent, session_worker) = match &session_doc_id {
            Some(doc) => {
                let (parent, _, worker) = self.relationships("AgentSession", doc, &docs).await;
                (parent, worker)
            }
            None => (None, None),
        };
        let requests = rows["data"]["AgentRequest"].as_array().unwrap();
        let named = |suffix: &str| {
            requests
                .iter()
                .find(|row| row["request_id"] == format!("{session_id}{suffix}"))
        };
        let (request, request_doc) = match (named("-request"), named("-setup")) {
            (Some(row), _) => (Some(row), number("request_doc")),
            (None, Some(row)) => (Some(row), input["before"]["request_doc"].as_u64()),
            (None, None) => (None, None),
        };
        let request_doc_id = request.map(|row| row["_docID"].as_str().unwrap().to_owned());
        let (request_owner, request_parent, request_worker) = match &request_doc_id {
            Some(doc) => {
                let (_, session, worker) = self.relationships("AgentRequest", doc, &docs).await;
                (self.owner("AgentRequest", doc).await, session, worker)
            }
            None => (None, None, None),
        };
        family::AdmissionObservation {
            session_owner: match &session_doc_id {
                Some(doc) => self.owner("AgentSession", doc).await,
                None => None,
            },
            session_doc: session_doc_id.map(|doc| if doc == targets.0 { session_doc } else { 0 }),
            session_parent,
            session_worker,
            receipt: session.is_some_and(|row| !row["provenance"]["creation_receipt"].is_null()),
            request_doc: request.and(request_doc),
            request_owner,
            request_parent,
            request_worker,
        }
    }
}

/// Each case runs the User's served admission transaction in its own session
/// scope: the before state is built by production owners (an earlier
/// submission, or a session whose relationships are unpublished), a denied
/// grant fails one relationship change, and an aborted transaction fails
/// after its last write and before commit.
#[tokio::test]
async fn client_admission_matches_the_lean_model() {
    let home = tempfile::tempdir().unwrap();
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let other = KeyIdentity::load_or_create(home.path().join("other.key"), None).unwrap();
    let (db, runtime) = serve_client_node(
        "client-admission-matches-the-lean-model",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    grant_operator_user(&node, &runtime, &user).await;
    let agent = gents_store::identity::provision_agent_identity(&runtime).unwrap();
    let access = db.client_access(&user);
    seed_owned_agent(&access, runtime.did(), &agent).await;
    grant_agent_baseline(&node, &runtime, &agent.agent_did).await;
    let served = Served {
        db,
        runtime,
        user,
        access,
        agent_did: agent.agent_did.clone(),
        other_agent_did: other.did().to_owned(),
    };
    let cases = family::cases::<String>();
    assert_eq!(cases.len(), 9);
    for case in cases {
        let input = serde_json::to_value(&case.input).unwrap();
        let session_id = case.input.name.as_str();
        let before = &input["before"];
        if !before["session"].is_null() {
            let mut setup = served.create(session_id, &format!("{session_id}-setup"), 3);
            if before["session_parent"].is_null() {
                let setup = &setup;
                served
                    .access
                    .transact_write("test.admission.incomplete_session", |txn| {
                        Box::pin(async move {
                            crate::session::ensure_session_in_txn(txn, new_request_session(setup))
                                .await
                        })
                    })
                    .await
                    .unwrap();
            } else {
                let targets = prepare_client_targets(&served.access, &setup)
                    .await
                    .unwrap();
                served.sign(&mut setup, targets).await;
                submit_client_request(&served.access, &setup).await.unwrap();
            }
        }
        let agent_number = input["requested"]["agent"].as_u64().unwrap();
        let mut create = served.create(session_id, &format!("{session_id}-request"), agent_number);
        let reserved = case.input.reserved;
        let creates_session = before["session"].is_null() && !reserved;
        let targets = if reserved {
            reserve_client_targets(&served.access, &create)
                .await
                .unwrap()
        } else if creates_session {
            prepare_client_targets(&served.access, &create)
                .await
                .unwrap()
        } else {
            served.existing_targets(session_id).await
        };
        served.sign(&mut create, targets.clone()).await;
        let submission = submit_client_request(&served.access, &create);
        let result = if !case.input.authorized {
            let grant = if creates_session {
                RelationshipMatch::first("AgentSession", "parent")
            } else {
                RelationshipMatch::first("AgentRequest", "parent")
            };
            let (result, trace) =
                ConfigApplyTxn::with_relationship_change_failure(Some(grant), submission).await;
            assert!(!trace.is_empty(), "{session_id} reaches its grant");
            result
        } else if !case.input.commit {
            let last_write = if creates_session { 2 } else { 1 };
            let (result, trace) = ConfigApplyTxn::with_successful_mutation_failure_and_trace(
                Some(last_write),
                submission,
            )
            .await;
            assert_eq!(
                trace,
                vec![Some("request.client.submit"); last_write],
                "{session_id} aborts after its last write"
            );
            result
        } else {
            submission.await
        };
        if let Err(error) = &result {
            assert!(!error.is_unconfirmed(), "{session_id}: {error:#}");
        }
        case.check(served.observe(session_id, &targets, &input).await);
    }
    served
        .db
        .node
        .shutdown()
        .await
        .expect("shut down test node");
}
