use super::contract_tests::{persist_command, receipt};
use super::*;
use crate::goal::operator_resume::support::{execute, Fixture, SESSION};
use crate::goal::session_goal_control::process_command_at;
use gents_lean_vocab::families::goal_client_mutation_contracts_local_receipt::{
    ReceiptInput, ReceiptOutput,
};
use gents_lean_vocab::family::Json;
use gents_protocol::goal_control::{GoalOperation, GoalRevision, SESSION_GOAL_CONTROL_VERSION};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

fn stamp(value: u64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-17T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + chrono::Duration::seconds(value as i64)
}

fn operation_of(kind: &str) -> GoalOperation {
    serde_json::from_value(serde_json::json!({ "kind": kind })).unwrap()
}

fn number(value: &Value, field: &str) -> u64 {
    value[field].as_u64().unwrap()
}

fn flag(value: &Value, field: &str) -> bool {
    value[field].as_bool().unwrap()
}

fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field].as_str().unwrap()
}

fn outcome_of(receipt: &gents_protocol::goal_control::SessionGoalControlReceipt) -> &'static str {
    match receipt.outcome {
        GoalControlOutcome::Applied { .. } => "applied",
        GoalControlOutcome::Rejected { .. } => "rejected",
    }
}

fn receipt_cases() -> Vec<gents_lean_vocab::family::Case<ReceiptInput, ReceiptOutput>> {
    gents_lean_vocab::families::goal_client_mutation_contracts_local_receipt::cases()
}

struct Driver {
    fixture: Fixture,
    session_doc_id: String,
    revisions: BTreeMap<String, String>,
    commands: BTreeMap<u64, (u64, String, SessionGoalControl)>,
}

impl Driver {
    async fn new(initial: &Value) -> Self {
        let goal = &initial["goal"];
        let fixture = Fixture::new_with_parent_state(&goal["execution"], false).await;
        execute(&fixture.agent_access(), &format!(r#"mutation {{ update_Goal(docID: "{}", input: {{ objective: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&fixture.goal.doc_id), escape_graphql_string(text(goal, "objective")))).await;
        let access = fixture.user_access();
        let session = access
            .transact_readonly("test.goal_receipt_session", |txn| {
                Box::pin(async {
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
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
        let current = fixture.goal_commit(&fixture.goal.doc_id).await.cid;
        Self {
            fixture,
            session_doc_id: session.doc_id,
            revisions: BTreeMap::from([(
                text(&goal["revision"], "composite").to_owned(),
                current,
            )]),
            commands: BTreeMap::new(),
        }
    }

    async fn prepared_command(&self, model: &Value) -> SessionGoalControl {
        assert!(!flag(model, "has_peer"));
        assert_eq!(text(model, "session_document"), "session-document");
        assert_eq!(text(&model["expected"], "document"), "goal-document");
        let mut command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id: format!("receipt-command-{}", number(model, "id")),
            requester_did: self.fixture.user.did().into(),
            requester_peer_id: None,
            node_did: self.fixture.identity.did().into(),
            agent_did: self.fixture.parent.agent_did.clone(),
            agent_doc_id: super::agent_document_for_test(
                &self.fixture.user_access(),
                self.fixture.identity.did(),
                &self.fixture.parent.agent_did,
            )
            .await,
            session_id: SESSION.into(),
            session_doc_id: Some(self.session_doc_id.clone()),
            issued_at: stamp(number(model, "issued_at")).to_rfc3339(),
            expires_at: stamp(number(model, "expires_at")).to_rfc3339(),
            expected: Some(GoalRevision {
                document: self.fixture.goal.doc_id.clone(),
                composite: self.revisions[text(&model["expected"], "composite")].clone(),
            }),
            operation: operation_of(text(&model["operation"], "kind")),
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

    async fn command(&mut self, model: &Value) -> (String, SessionGoalControl) {
        let command = self.prepared_command(model).await;
        let id = number(model, "id");
        if let Some((digest, doc, original)) = self.commands.get(&id) {
            assert_eq!(*digest, number(model, "digest"));
            assert_eq!(
                original, &command,
                "a replay must preserve the complete signed wire record"
            );
            return (doc.clone(), command);
        }
        let access = self.fixture.user_access();
        let doc_id = persist_command(&access, &command).await;
        self.commands
            .insert(id, (number(model, "digest"), doc_id.clone(), command.clone()));
        (doc_id, command)
    }

    fn receipt_wire(&self, receipt: &gents_protocol::goal_control::SessionGoalControlReceipt) -> Value {
        let (id, (digest, _, _)) = self
            .commands
            .iter()
            .find(|(_, (_, _, command))| command.command_id == receipt.command_id)
            .expect("a receipt names a persisted command");
        serde_json::json!({ "command_id": id, "digest": digest, "outcome": outcome_of(receipt) })
    }

    async fn apply(
        &mut self,
        command: &Value,
        facts: &Value,
        now: u64,
        next: &str,
        commit: bool,
    ) -> Value {
        assert!(commit);
        assert!(flag(facts, "command_identity_unique"));
        assert!(flag(&facts["authority"], "signature_valid"));
        assert!(flag(&facts["authority"], "session_owned"));
        assert!(flag(&facts["control"], "unfinished_request"));
        let (doc_id, command) = self.command(command).await;
        let access = self.fixture.user_access();
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
        let current = self
            .fixture
            .goal_commit(&self.fixture.goal.doc_id)
            .await
            .cid;
        if !self.revisions.values().any(|cid| *cid == current) {
            self.revisions.insert(next.to_owned(), current);
        }
        self.receipt_wire(&result)
    }

    async fn lose_receipts(&self) {
        let ids = self
            .commands
            .values()
            .map(|(_, id, _)| id.clone())
            .collect::<Vec<_>>();
        self.fixture.agent_access().transact_write("test.erase_goal_receipts", |txn| Box::pin(async {
            for id in &ids {
                txn.execute(&format!(r#"mutation {{ delete_AgentSessionGoalReceipt(filter: {{ command_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                    escape_graphql_string(id))).await?;
            }
            Ok::<_, anyhow::Error>(())
        })).await.unwrap();
    }

    async fn state(&self) -> Value {
        let current_goal = self.fixture.current_goal().await;
        let current = self
            .fixture
            .goal_commit(&current_goal.doc_id)
            .await
            .cid;
        let composite = self
            .revisions
            .iter()
            .find(|(_, cid)| **cid == current)
            .map_or(current.clone(), |(name, _)| name.clone());
        let document = if current_goal.doc_id == self.fixture.goal.doc_id {
            "goal-document".to_owned()
        } else {
            current_goal.doc_id.clone()
        };
        let access = self.fixture.agent_access();
        let documents = access
            .execute("{ Goal { _docID } AgentRequest { _docID } }")
            .await
            .unwrap();
        assert_eq!(documents["data"]["Goal"].as_array().unwrap().len(), 1);
        assert_eq!(
            documents["data"]["AgentRequest"].as_array().unwrap().len(),
            1
        );
        let mut receipts = Vec::new();
        for (_, doc, command) in self.commands.values() {
            if let Some(actual) = receipt(&access, doc).await {
                actual.validate_for(command).unwrap();
                receipts.push(self.receipt_wire(&actual));
            }
        }
        serde_json::json!({
            "goal": {
                "revision": { "document": document, "composite": composite },
                "execution": self.fixture.observe().await,
                "objective": current_goal.objective,
            },
            "receipts": receipts,
        })
    }
}

#[tokio::test]
async fn generated_goal_receipt_traces_preserve_results_and_reject_erased_stale_intents() {
    for case in receipt_cases() {
        let input = &case.input;
        let mut driver = Driver::new(&input.initial.0).await;
        assert_eq!(driver.state().await, input.initial.0, "{}", input.name);
        for step in input.history.0.as_array().unwrap() {
            match text(step, "kind") {
                "apply" => {
                    driver
                        .apply(
                            &step["command"],
                            &step["facts"],
                            number(step, "now"),
                            text(step, "next_composite"),
                            flag(step, "commit"),
                        )
                        .await;
                }
                "lose_receipts" => driver.lose_receipts().await,
                kind => panic!("unknown goal receipt step kind: {kind}"),
            }
        }
        let before = driver.state().await;
        let access = driver.fixture.user_access();
        let query = "{ AgentSessionGoalControl { _docID intent_json } AgentSessionGoalReceipt { _docID receipt_json } }";
        let intents = access.execute(query).await.unwrap();
        let command = driver.prepared_command(&input.command.0).await;
        let recovered = crate::goal::recover_session_goal_control(
            &access,
            driver.fixture.user.as_ref(),
            &command,
        )
        .await
        .unwrap()
        .map(|receipt| {
            receipt.validate_for(&command).unwrap();
            Json(driver.receipt_wire(&receipt))
        });
        assert_eq!(
            access.execute(query).await.unwrap(),
            intents,
            "{}: recovery cannot publish or mutate the original intent",
            input.name
        );
        assert_eq!(driver.state().await, before, "{}", input.name);
        let applied = driver
            .apply(
                &input.command.0,
                &input.facts.0,
                input.now,
                &input.next_composite,
                input.commit,
            )
            .await;
        let expected = driver.state().await;
        driver.fixture.node.shutdown().await.expect("node shutdown");
        case.check(ReceiptOutput {
            before: Json(before),
            recovered,
            receipt: Some(Json(applied)),
            expected: Json(expected),
        });
    }
}

#[tokio::test]
async fn modeled_publication_retries_pending_receipt_relationships() {
    let seeds = receipt_cases();
    let seed = &seeds[0].input;
    for trace in gents_lean_vocab::families::identity_publication_receipt::cases() {
        let mut driver = Driver::new(&seed.initial.0).await;
        let (command_doc, command) = driver.command(&seed.command.0).await;
        let node = &driver.fixture.node;
        let actor = ::identity::Did::new(&command.agent_did).unwrap();
        let agent_access = driver.fixture.agent_access();
        let user_access = driver.fixture.user_access();
        let signer = driver.fixture.agent.clone();
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
            gents_store::graphql::created_doc_id(&written, "AgentSessionGoalReceipt").unwrap();
        let query = format!(
            r#"{{ AgentSessionGoalReceipt(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ relationships_pending }} }}"#,
            escape_graphql_string(&receipt_doc)
        );
        let observe = || {
            let agent_access = &agent_access;
            let user_access = &user_access;
            let query = &query;
            async move {
                gents_lean_vocab::families::identity_publication_receipt::Observation {
                    pending: agent_access.execute(query).await.unwrap()["data"]
                        ["AgentSessionGoalReceipt"][0]["relationships_pending"]
                        .as_bool()
                        .unwrap(),
                    visible: !user_access.execute(query).await.unwrap()["data"]
                        ["AgentSessionGoalReceipt"]
                        .as_array()
                        .unwrap()
                        .is_empty(),
                }
            }
        };
        let initial = observe().await;
        let mut after = Vec::new();
        for step in &trace.input.steps {
            if step.parent_ok && !step.reader_ok {
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
            if step.parent_ok && step.reader_ok {
                let identity: Arc<dyn NodeIdentity> = driver.fixture.identity.clone();
                process_command_at(node, &identity, &command_doc, &actor, stamp(seed.now))
                    .await
                    .unwrap();
            }
            if step.revoke_parent {
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
            after.push(observe().await);
        }
        trace.check(
            gents_lean_vocab::families::identity_publication_receipt::ReceiptOutput {
                initial,
                after,
            },
        );
        node.shutdown().await.expect("node shutdown");
    }
}
