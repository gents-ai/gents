use super::contract_tests::{persist_command, receipt};
use super::*;
use crate::agent::p2p_reconcile::session_goal_control::process_command_at;
use crate::config_client::ConfigAccess;
use crate::goal::operator_resume::support::{execute, Fixture, SESSION};
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
        execute(&fixture.node, &format!(r#"mutation {{ update_Goal(docID: "{}", input: {{ objective: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&fixture.goal.doc_id), escape_graphql_string(initial["goal"]["objective"].as_str().unwrap()))).await;
        crate::session::ensure_session_with_agent_id_and_requester_did(
            &fixture.node,
            SESSION,
            fixture.identity.did(),
            "contract-behavior",
            Some(fixture.identity.did()),
        )
        .await
        .unwrap();
        let access = ConfigAccess::local(fixture.node.clone());
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
        let current = crate::graphql::newest_document_composite_commit(
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

    async fn command(&mut self, model: &Command) -> (String, SessionGoalControl) {
        assert!(!model.has_peer);
        assert_eq!(model.session_document, "session-document");
        assert_eq!(model.expected.document, "goal-document");
        let mut command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id: format!("receipt-command-{}", model.id),
            requester_did: self.fixture.identity.did().into(),
            requester_peer_id: None,
            node_did: self.fixture.identity.did().into(),
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
            signature: Vec::new(),
        };
        command.signature = self
            .fixture
            .identity
            .sign(&command.signing_payload().unwrap())
            .await
            .unwrap();
        if let Some((digest, doc, original)) = self.commands.get(&model.id) {
            assert_eq!(*digest, model.digest);
            assert_eq!(
                original, &command,
                "a replay must preserve the complete signed wire record"
            );
            return (doc.clone(), command);
        }
        let access = ConfigAccess::local(self.fixture.node.clone());
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
        let access = ConfigAccess::local(self.fixture.node.clone());
        let previous = receipt(&access, &doc_id).await;
        let identity: Arc<dyn NodeIdentity> = self.fixture.identity.clone();
        assert_eq!(
            process_command_at(&self.fixture.node, &identity, &doc_id, stamp(now))
                .await
                .unwrap(),
            None
        );
        let result = receipt(&access, &doc_id).await.unwrap();
        result.validate_for(&command).unwrap();
        assert!(identity
            .verify(
                identity.did(),
                &result.signing_payload().unwrap(),
                &result.signature
            )
            .await
            .unwrap());
        if let Some(previous) = previous {
            assert_eq!(result, previous, "recorded receipts are read-only");
        }
        let current = crate::graphql::newest_document_composite_commit(
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
        ConfigAccess::local(self.fixture.node.clone()).transact_write("test.erase_goal_receipts", |txn| Box::pin(async {
            for id in &ids {
                txn.execute(&format!(r#"mutation {{ update_AgentSessionGoalControl(docID: "{}", input: {{ receipt_json: null }}) {{ _docID }} }}"#,
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
        let goal = load_canonical_goal(&self.fixture.node, self.fixture.identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            goal.objective,
            expected["goal"]["objective"].as_str().unwrap(),
            "{name}: objective"
        );
        assert_eq!(
            goal.doc_id, self.fixture.goal.doc_id,
            "{name}: goal identity"
        );
        let current = crate::graphql::newest_document_composite_commit(
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
        driver.fixture.node.shutdown().await;
    }
}
