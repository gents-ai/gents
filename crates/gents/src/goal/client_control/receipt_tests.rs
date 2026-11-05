use super::contract_tests::{persist_command, receipt};
use super::*;
use crate::goal::operator_resume::support::{execute, Fixture, SESSION};
use crate::runtime::agent::p2p_reconcile::session_goal_control::process_command_at;
use crate::store::config_client::ConfigAccess;
use gents_protocol::goal_control::{GoalRevision, SESSION_GOAL_CONTROL_VERSION};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Deserialize)]
struct Contracts {
    goal_client_receipt_cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    initial: Value,
    history: Vec<Step>,
    before: Value,
    command: Command,
    facts: Value,
    now: i64,
    next_composite: String,
    commit: bool,
    expected: Value,
    recovered: Value,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Step {
    Apply {
        command: Box<Command>,
        facts: Value,
        now: i64,
        next_composite: String,
        commit: bool,
    },
    LoseReceipts,
}

#[derive(Deserialize)]
struct Command {
    id: u64,
    digest: u64,
    has_peer: bool,
    session_document: String,
    issued_at: i64,
    expires_at: i64,
    expected: GoalRevision,
    operation: GoalOperation,
}

fn stamp(value: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-17T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + chrono::Duration::seconds(value)
}

struct Driver {
    fixture: Fixture,
    session_doc_id: String,
    revisions: BTreeMap<String, String>,
    commands: BTreeMap<u64, (u64, String, SessionGoalControl)>,
}

impl Driver {
    async fn new(initial: &Value) -> Self {
        let fixture = Fixture::new_with_parent_state(&initial["goal"]["execution"], false).await;
        execute(&fixture.user_access(), &format!(r#"mutation {{ update_Goal(docID: "{}", input: {{ objective: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&fixture.goal.doc_id), escape_graphql_string(initial["goal"]["objective"].as_str().unwrap()))).await;
        let access = ConfigAccess::local_as(
            fixture.node.clone(),
            ::identity::Did::new(fixture.user.did()).unwrap(),
        );
        let session = access
            .transact_readonly("test.goal_receipt_session", |txn| {
                Box::pin(async {
                    crate::session::load_agent_session_owner_in_txn(
                        txn,
                        fixture.identity.did(),
                        SESSION,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        let current = crate::store::graphql::newest_document_composite_commit(
            &fixture.node,
            &fixture.goal.doc_id,
            "test.goal_receipt_revision",
        )
        .await
        .unwrap()
        .unwrap()
        .cid;
        Self {
            fixture,
            session_doc_id: session.doc_id,
            revisions: BTreeMap::from([(
                initial["goal"]["revision"]["composite"]
                    .as_str()
                    .unwrap()
                    .into(),
                current,
            )]),
            commands: BTreeMap::new(),
        }
    }

    async fn prepared_command(&self, model: &Command) -> SessionGoalControl {
        assert!(!model.has_peer);
        assert_eq!(model.session_document, "session-document");
        assert_eq!(model.expected.document, "goal-document");
        let mut command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id: format!("receipt-command-{}", model.id),
            requester_did: self.fixture.user.did().into(),
            requester_peer_id: None,
            node_did: self.fixture.identity.did().into(),
            agent_did: self.fixture.parent.agent_did.clone(),
            agent_doc_id: super::agent_document_for_test(
                &ConfigAccess::local_as(
                    self.fixture.node.clone(),
                    ::identity::Did::new(self.fixture.user.did()).unwrap(),
                ),
                self.fixture.identity.did(),
                &self.fixture.parent.agent_did,
            )
            .await,
            session_id: SESSION.into(),
            session_doc_id: Some(self.session_doc_id.clone()),
            issued_at: stamp(model.issued_at).to_rfc3339(),
            expires_at: stamp(model.expires_at).to_rfc3339(),
            expected: Some(GoalRevision {
                document: self.fixture.goal.doc_id.clone(),
                composite: self.revisions[&model.expected.composite].clone(),
            }),
            operation: model.operation.clone(),
            initial_request: None,
            native_evidence: None,
            signature: Vec::new(),
        };
        command.signature = self
            .fixture
            .user
            .sign(&command.signing_payload().unwrap())
            .await
            .unwrap();
        command
    }

    async fn command(&mut self, model: &Command) -> (String, SessionGoalControl) {
        let command = self.prepared_command(model).await;
        if let Some((digest, doc, original)) = self.commands.get(&model.id) {
            assert_eq!(*digest, model.digest);
            assert_eq!(
                original, &command,
                "a replay must preserve the complete signed wire record"
            );
            return (doc.clone(), command);
        }
        let access = ConfigAccess::local_as(
            self.fixture.node.clone(),
            ::identity::Did::new(self.fixture.user.did()).unwrap(),
        );
        let doc_id = persist_command(&access, &command).await;
        self.commands
            .insert(model.id, (model.digest, doc_id.clone(), command.clone()));
        (doc_id, command)
    }

    async fn apply(
        &mut self,
        command: &Command,
        facts: &Value,
        now: i64,
        next: &str,
        commit: bool,
    ) {
        assert!(commit);
        assert_eq!(facts["command_identity_unique"], true);
        for field in ["signature_valid", "session_owned", "requester_is_node"] {
            assert_eq!(facts["authority"][field], true);
        }
        assert_eq!(facts["control"]["unfinished_request"], true);
        let (doc_id, command) = self.command(command).await;
        let access = ConfigAccess::local_as(
            self.fixture.node.clone(),
            ::identity::Did::new(self.fixture.user.did()).unwrap(),
        );
        let previous = receipt(&access, &doc_id).await;
        let identity: Arc<dyn NodeIdentity> = self.fixture.identity.clone();
        assert_eq!(
            process_command_at(
                &self.fixture.node,
                &identity,
                &doc_id,
                &::identity::Did::new(&command.agent_did).unwrap(),
                stamp(now)
            )
            .await
            .unwrap(),
            None
        );
        let result = receipt(&access, &doc_id).await.unwrap();
        result.validate_for(&command).unwrap();
        assert!(identity
            .verify(
                &result.agent_did,
                &result.signing_payload().unwrap(),
                &result.signature
            )
            .await
            .unwrap());
        if let Some(previous) = previous {
            assert_eq!(result, previous, "recorded receipts are read-only");
        }
        let current = crate::store::graphql::newest_document_composite_commit(
            &self.fixture.node,
            &self.fixture.goal.doc_id,
            "test.goal_receipt_after",
        )
        .await
        .unwrap()
        .unwrap()
        .cid;
        self.revisions.insert(next.to_owned(), current);
    }

    async fn lose_receipts(&self) {
        let ids = self
            .commands
            .values()
            .map(|(_, id, _)| id.clone())
            .collect::<Vec<_>>();
        ConfigAccess::local_as(self.fixture.node.clone(), ::identity::Did::new(&self.fixture.parent.agent_did).unwrap()).transact_write("test.erase_goal_receipts", |txn| Box::pin(async {
            for id in &ids {
                txn.execute(&format!(r#"mutation {{ delete_AgentSessionGoalReceipt(filter: {{ command_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                    escape_graphql_string(id))).await?;
            }
            Ok::<_, anyhow::Error>(())
        })).await.unwrap();
    }

    async fn assert_state(&self, expected: &Value, name: &str) {
        assert_eq!(
            self.fixture.observe().await,
            expected["goal"]["execution"],
            "{name}: execution"
        );
        let goal = self.fixture.current_goal().await;
        assert_eq!(
            goal.objective,
            expected["goal"]["objective"].as_str().unwrap(),
            "{name}: objective"
        );
        assert_eq!(
            goal.doc_id, self.fixture.goal.doc_id,
            "{name}: goal identity"
        );
        let current = crate::store::graphql::newest_document_composite_commit(
            &self.fixture.node,
            &goal.doc_id,
            "test.goal_receipt_observe",
        )
        .await
        .unwrap()
        .unwrap()
        .cid;
        assert_eq!(
            current,
            self.revisions[expected["goal"]["revision"]["composite"].as_str().unwrap()],
            "{name}: goal revision"
        );
        let access = ConfigAccess::local(self.fixture.node.clone());
        let documents = access
            .execute("{ Goal { _docID } AgentRequest { _docID } }")
            .await
            .unwrap();
        assert_eq!(
            documents["data"]["Goal"].as_array().unwrap().len(),
            1,
            "{name}: no extra goal"
        );
        assert_eq!(
            documents["data"]["AgentRequest"].as_array().unwrap().len(),
            1,
            "{name}: no extra request"
        );
        for (id, (digest, doc, command)) in &self.commands {
            let modeled = expected["receipts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|receipt| receipt["command_id"] == *id);
            let actual = receipt(&access, doc).await;
            match modeled {
                Some(modeled) => {
                    assert_eq!(modeled["digest"], *digest);
                    let actual = actual.expect("modeled receipt must be durable");
                    actual.validate_for(command).unwrap();
                    let outcome = match actual.outcome {
                        GoalControlOutcome::Applied { .. } => "applied",
                        GoalControlOutcome::Rejected { .. } => "rejected",
                    };
                    assert_eq!(modeled["outcome"], outcome, "{name}: receipt {id}");
                }
                None => assert!(actual.is_none(), "{name}: receipt {id} stays absent"),
            }
        }
    }
}

#[tokio::test]
async fn generated_goal_receipt_traces_preserve_results_and_reject_erased_stale_intents() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_client_receipt_cases.len(), 4);
    for case in contracts.goal_client_receipt_cases {
        let mut driver = Driver::new(&case.initial).await;
        driver.assert_state(&case.initial, &case.name).await;
        for step in case.history {
            match step {
                Step::Apply {
                    command,
                    facts,
                    now,
                    next_composite,
                    commit,
                } => {
                    driver
                        .apply(&command, &facts, now, &next_composite, commit)
                        .await
                }
                Step::LoseReceipts => driver.lose_receipts().await,
            }
        }
        driver.assert_state(&case.before, &case.name).await;
        let access = ConfigAccess::local_as(
            driver.fixture.node.clone(),
            ::identity::Did::new(driver.fixture.user.did()).unwrap(),
        );
        let query = "{ AgentSessionGoalControl { _docID intent_json } AgentSessionGoalReceipt { _docID receipt_json } }";
        let before = access.execute(query).await.unwrap();
        let command = driver.prepared_command(&case.command).await;
        let recovered = crate::goal::recover_session_goal_control(
            &access,
            driver.fixture.user.as_ref(),
            &command,
        )
        .await
        .unwrap();
        match recovered {
            Some(receipt) => {
                assert_eq!(case.recovered["command_id"], case.command.id);
                assert_eq!(case.recovered["digest"], case.command.digest);
                receipt.validate_for(&command).unwrap();
                let outcome = match receipt.outcome {
                    GoalControlOutcome::Applied { .. } => "applied",
                    GoalControlOutcome::Rejected { .. } => "rejected",
                };
                assert_eq!(case.recovered["outcome"], outcome, "{}", case.name);
            }
            None => assert!(case.recovered.is_null(), "{}", case.name),
        }
        assert_eq!(
            access.execute(query).await.unwrap(),
            before,
            "{}: recovery cannot publish or mutate the original intent",
            case.name
        );
        driver.assert_state(&case.before, &case.name).await;
        driver
            .apply(
                &case.command,
                &case.facts,
                case.now,
                &case.next_composite,
                case.commit,
            )
            .await;
        driver.assert_state(&case.expected, &case.name).await;
        driver.fixture.node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn modeled_publication_retries_pending_receipt_relationships() {
    let snapshot = gents_lean_contract::load_contract_snapshot::<Value>().unwrap();
    let seed: Case =
        serde_json::from_value(snapshot["goal_client_receipt_cases"][0].clone()).unwrap();
    for trace in snapshot["identity_publication_cases"].as_array().unwrap() {
        let mut driver = Driver::new(&seed.initial).await;
        let (command_doc, command) = driver.command(&seed.command).await;
        let node = &driver.fixture.node;
        let actor = ::identity::Did::new(&command.agent_did).unwrap();
        let agent_access = ConfigAccess::local_as(node.clone(), actor.clone());
        let user_access = ConfigAccess::local_as(
            node.clone(),
            ::identity::Did::new(driver.fixture.user.did()).unwrap(),
        );
        let signer = crate::store::identity::RegisteredIdentity::from_registered_did(
            command.agent_did.clone(),
            None,
        )
        .unwrap();
        let mut result = gents_protocol::goal_control::SessionGoalControlReceipt {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id: command.command_id.clone(),
            command_digest: command.computed_digest().unwrap(),
            requester_did: command.requester_did.clone(),
            node_did: command.node_did.clone(),
            agent_did: command.agent_did.clone(),
            session_id: command.session_id.clone(),
            outcome: GoalControlOutcome::Rejected {
                reason: "fixture".into(),
            },
            processed_at: stamp(seed.now).to_rfc3339(),
            signature: Vec::new(),
        };
        result.signature = signer
            .sign(&result.signing_payload().unwrap())
            .await
            .unwrap();
        let written = agent_access.transact_write("test.goal_pending_receipt", |txn| {
            let command_doc = &command_doc;
            let command = &command;
            let result = &result;
            Box::pin(async move {
                txn.execute_with_variables(
                    "mutation($input: AgentSessionGoalReceiptMutationInputArg!) { create_AgentSessionGoalReceipt(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"command_doc_id": command_doc, "command_id": command.command_id,
                        "receipt_json": serde_json::to_string(result)?, "relationships_pending": true}}),
                ).await
            })
        }).await.unwrap();
        let receipt_doc =
            crate::store::graphql::created_doc_id(&written, "AgentSessionGoalReceipt").unwrap();
        let query = format!(
            r#"{{ AgentSessionGoalReceipt(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ relationships_pending }} }}"#,
            escape_graphql_string(&receipt_doc)
        );
        assert_eq!(
            agent_access.execute(&query).await.unwrap()["data"]["AgentSessionGoalReceipt"][0]
                ["relationships_pending"],
            trace["initial"]["pending"]
        );
        assert!(
            user_access.execute(&query).await.unwrap()["data"]["AgentSessionGoalReceipt"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        for step in trace["steps"].as_array().unwrap() {
            if step["parent_ok"] == true && step["reader_ok"] != true {
                agent_access
                    .add_resource_relationship(
                        "AgentSessionGoalReceipt",
                        &receipt_doc,
                        "parent",
                        "goal_controls",
                        &command_doc,
                    )
                    .await
                    .unwrap();
            }
            if step["parent_ok"] == true && step["reader_ok"] == true {
                let identity: Arc<dyn NodeIdentity> = driver.fixture.identity.clone();
                process_command_at(node, &identity, &command_doc, &actor, stamp(seed.now))
                    .await
                    .unwrap();
            }
            if step["revoke_parent"] == true {
                agent_access
                    .delete_resource_relationship(
                        "AgentSessionGoalReceipt",
                        &receipt_doc,
                        "parent",
                        "goal_controls",
                        &command_doc,
                    )
                    .await
                    .unwrap();
            }
            let agent_rows = agent_access.execute(&query).await.unwrap();
            assert_eq!(
                agent_rows["data"]["AgentSessionGoalReceipt"][0]["relationships_pending"],
                step["after"]["pending"],
                "{}",
                trace["name"]
            );
            let visible = !user_access.execute(&query).await.unwrap()["data"]
                ["AgentSessionGoalReceipt"]
                .as_array()
                .unwrap()
                .is_empty();
            let expected =
                step["after"]["parent_granted"] == true || step["after"]["reader_granted"] == true;
            assert_eq!(visible, expected, "{}", trace["name"]);
        }
        node.shutdown().await.expect("node shutdown");
    }
}
