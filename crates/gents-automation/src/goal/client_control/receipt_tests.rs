use super::contract_tests::{persist_command, receipt};
use super::*;
use crate::goal::operator_resume::support::{execute, Fixture, SESSION};
use crate::goal::session_goal_control::process_command_at;
use gents_lean_vocab::{
    LeanGoalClientReceiptCaseBeforeGoal, LeanGoalClientReceiptCaseCommand,
    LeanGoalClientReceiptCaseExpectedGoal, LeanGoalClientReceiptCaseFacts,
    LeanGoalClientReceiptCaseHistoryItemCommand, LeanGoalClientReceiptCaseHistoryItemFacts,
    LeanGoalClientReceiptCaseInitial, LeanGoalClientReceiptCaseInitialGoal,
};
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

trait ReceiptCommandModel {
    fn digest(&self) -> u64;
    fn expected(&self) -> (&str, &str);
    fn expires_at(&self) -> u64;
    fn has_peer(&self) -> bool;
    fn id(&self) -> u64;
    fn issued_at(&self) -> u64;
    fn operation_kind(&self) -> &str;
    fn session_document(&self) -> &str;
}

impl ReceiptCommandModel for LeanGoalClientReceiptCaseCommand {
    fn digest(&self) -> u64 {
        self.digest
    }

    fn expected(&self) -> (&str, &str) {
        (&self.expected.document, &self.expected.composite)
    }

    fn expires_at(&self) -> u64 {
        self.expires_at
    }

    fn has_peer(&self) -> bool {
        self.has_peer
    }

    fn id(&self) -> u64 {
        self.id
    }

    fn issued_at(&self) -> u64 {
        self.issued_at
    }

    fn operation_kind(&self) -> &str {
        &self.operation.kind
    }

    fn session_document(&self) -> &str {
        &self.session_document
    }
}

impl ReceiptCommandModel for LeanGoalClientReceiptCaseHistoryItemCommand {
    fn digest(&self) -> u64 {
        self.digest
    }

    fn expected(&self) -> (&str, &str) {
        (&self.expected.document, &self.expected.composite)
    }

    fn expires_at(&self) -> u64 {
        self.expires_at
    }

    fn has_peer(&self) -> bool {
        self.has_peer
    }

    fn id(&self) -> u64 {
        self.id
    }

    fn issued_at(&self) -> u64 {
        self.issued_at
    }

    fn operation_kind(&self) -> &str {
        &self.operation.kind
    }

    fn session_document(&self) -> &str {
        &self.session_document
    }
}

trait ReceiptFactsModel {
    fn command_identity_unique(&self) -> bool;
    fn session_owned(&self) -> bool;
    fn signature_valid(&self) -> bool;
    fn unfinished_request(&self) -> bool;
}

impl ReceiptFactsModel for LeanGoalClientReceiptCaseFacts {
    fn command_identity_unique(&self) -> bool {
        self.command_identity_unique
    }

    fn session_owned(&self) -> bool {
        self.authority.session_owned
    }

    fn signature_valid(&self) -> bool {
        self.authority.signature_valid
    }

    fn unfinished_request(&self) -> bool {
        self.control.unfinished_request
    }
}

impl ReceiptFactsModel for LeanGoalClientReceiptCaseHistoryItemFacts {
    fn command_identity_unique(&self) -> bool {
        self.command_identity_unique
    }

    fn session_owned(&self) -> bool {
        self.authority.session_owned
    }

    fn signature_valid(&self) -> bool {
        self.authority.signature_valid
    }

    fn unfinished_request(&self) -> bool {
        self.control.unfinished_request
    }
}

trait GoalState {
    fn execution(&self) -> Value;
    fn objective(&self) -> &str;
    fn composite(&self) -> &str;
}

impl GoalState for LeanGoalClientReceiptCaseInitialGoal {
    fn execution(&self) -> Value {
        let execution = &self.execution;
        serde_json::json!({
            "blocked_audits": execution.blocked_audits,
            "children": execution.children,
            "last_continued_from": execution.last_continued_from,
            "latest_request": execution.latest_request,
            "sequence": execution.sequence,
            "status": execution.status,
            "token_budget": execution.token_budget,
            "tokens_used": execution.tokens_used,
            "wrapup_completed": execution.wrapup_completed,
            "wrapup_requested": execution.wrapup_requested,
        })
    }

    fn objective(&self) -> &str {
        &self.objective
    }

    fn composite(&self) -> &str {
        &self.revision.composite
    }
}

impl GoalState for LeanGoalClientReceiptCaseBeforeGoal {
    fn execution(&self) -> Value {
        let execution = &self.execution;
        serde_json::json!({
            "blocked_audits": execution.blocked_audits,
            "children": execution.children,
            "last_continued_from": execution.last_continued_from,
            "latest_request": execution.latest_request,
            "sequence": execution.sequence,
            "status": execution.status,
            "token_budget": execution.token_budget,
            "tokens_used": execution.tokens_used,
            "wrapup_completed": execution.wrapup_completed,
            "wrapup_requested": execution.wrapup_requested,
        })
    }

    fn objective(&self) -> &str {
        &self.objective
    }

    fn composite(&self) -> &str {
        &self.revision.composite
    }
}

impl GoalState for LeanGoalClientReceiptCaseExpectedGoal {
    fn execution(&self) -> Value {
        let execution = &self.execution;
        serde_json::json!({
            "blocked_audits": execution.blocked_audits,
            "children": execution.children,
            "last_continued_from": execution.last_continued_from,
            "latest_request": execution.latest_request,
            "sequence": execution.sequence,
            "status": execution.status,
            "token_budget": execution.token_budget,
            "tokens_used": execution.tokens_used,
            "wrapup_completed": execution.wrapup_completed,
            "wrapup_requested": execution.wrapup_requested,
        })
    }

    fn objective(&self) -> &str {
        &self.objective
    }

    fn composite(&self) -> &str {
        &self.revision.composite
    }
}

struct Driver {
    fixture: Fixture,
    session_doc_id: String,
    revisions: BTreeMap<String, String>,
    commands: BTreeMap<u64, (u64, String, SessionGoalControl)>,
}

impl Driver {
    async fn new(initial: &LeanGoalClientReceiptCaseInitial) -> Self {
        let execution = initial.goal.execution();
        let fixture = Fixture::new_with_parent_state(&execution, false).await;
        execute(&fixture.agent_access(), &format!(r#"mutation {{ update_Goal(docID: "{}", input: {{ objective: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&fixture.goal.doc_id), escape_graphql_string(initial.goal.objective()))).await;
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
            revisions: BTreeMap::from([(initial.goal.composite().to_owned(), current)]),
            commands: BTreeMap::new(),
        }
    }

    async fn prepared_command<C>(&self, model: &C) -> SessionGoalControl
    where
        C: ReceiptCommandModel,
    {
        assert!(!model.has_peer());
        assert_eq!(model.session_document(), "session-document");
        assert_eq!(model.expected().0, "goal-document");
        let mut command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id: format!("receipt-command-{}", model.id()),
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
            issued_at: stamp(model.issued_at()).to_rfc3339(),
            expires_at: stamp(model.expires_at()).to_rfc3339(),
            expected: Some(GoalRevision {
                document: self.fixture.goal.doc_id.clone(),
                composite: self.revisions[model.expected().1].clone(),
            }),
            operation: operation_of(model.operation_kind()),
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

    async fn command<C>(&mut self, model: &C) -> (String, SessionGoalControl)
    where
        C: ReceiptCommandModel,
    {
        let command = self.prepared_command(model).await;
        if let Some((digest, doc, original)) = self.commands.get(&model.id()) {
            assert_eq!(*digest, model.digest());
            assert_eq!(
                original, &command,
                "a replay must preserve the complete signed wire record"
            );
            return (doc.clone(), command);
        }
        let access = self.fixture.user_access();
        let doc_id = persist_command(&access, &command).await;
        self.commands
            .insert(model.id(), (model.digest(), doc_id.clone(), command.clone()));
        (doc_id, command)
    }

    async fn apply<C, F>(
        &mut self,
        command: &C,
        facts: &F,
        now: u64,
        next: &str,
        commit: bool,
    ) where
        C: ReceiptCommandModel,
        F: ReceiptFactsModel,
    {
        assert!(commit);
        assert!(facts.command_identity_unique());
        assert!(facts.signature_valid());
        assert!(facts.session_owned());
        assert!(facts.unfinished_request());
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
        self.revisions.insert(next.to_owned(), current);
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

    async fn assert_state<G>(&self, goal: &G, receipts: &[(u64, u64, String)], name: &str)
    where
        G: GoalState,
    {
        assert_eq!(
            self.fixture.observe().await, goal.execution(),
            "{name}: execution"
        );
        let current_goal = self.fixture.current_goal().await;
        assert_eq!(
            current_goal.objective, goal.objective(),
            "{name}: objective"
        );
        assert_eq!(
            current_goal.doc_id, self.fixture.goal.doc_id,
            "{name}: goal identity"
        );
        let current = self
            .fixture
            .goal_commit(&current_goal.doc_id)
            .await
            .cid;
        assert_eq!(
            current, self.revisions[goal.composite()],
            "{name}: goal revision"
        );
        let access = self.fixture.agent_access();
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
            let modeled = receipts.iter().find(|row| row.0 == *id);
            let actual = receipt(&access, doc).await;
            match modeled {
                Some((_, modeled_digest, modeled_outcome)) => {
                    assert_eq!(modeled_digest, digest);
                    let actual = actual.expect("modeled receipt must be durable");
                    actual.validate_for(command).unwrap();
                    let outcome = match actual.outcome {
                        GoalControlOutcome::Applied { .. } => "applied",
                        GoalControlOutcome::Rejected { .. } => "rejected",
                    };
                    assert_eq!(modeled_outcome, outcome, "{name}: receipt {id}");
                }
                None => assert!(actual.is_none(), "{name}: receipt {id} stays absent"),
            }
        }
    }
}

fn receipt_rows(receipts: &[Value]) -> Vec<(u64, u64, String)> {
    receipts
        .iter()
        .map(|row| {
            (
                row["command_id"].as_u64().unwrap(),
                row["digest"].as_u64().unwrap(),
                row["outcome"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn generated_goal_receipt_traces_preserve_results_and_reject_erased_stale_intents() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().goal_client_receipt_cases;
    assert_eq!(cases.len(), 4);
    for case in cases {
        let mut driver = Driver::new(&case.initial).await;
        driver
            .assert_state(
                &case.initial.goal,
                &receipt_rows(&case.initial.receipts),
                &case.name,
            )
            .await;
        for step in &case.history {
            match step.kind.as_str() {
                "apply" => {
                    driver
                        .apply(
                            step.command.as_ref().unwrap(),
                            step.facts.as_ref().unwrap(),
                            step.now.unwrap(),
                            step.next_composite.as_deref().unwrap(),
                            step.commit.unwrap(),
                        )
                        .await
                }
                "lose_receipts" => driver.lose_receipts().await,
                kind => panic!("unknown goal receipt step kind: {kind}"),
            }
        }
        let before_receipts = case
            .before
            .receipts
            .iter()
            .map(|row| (row.command_id, row.digest, row.outcome.clone()))
            .collect::<Vec<_>>();
        driver
            .assert_state(&case.before.goal, &before_receipts, &case.name)
            .await;
        let access = driver.fixture.user_access();
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
                let modeled = case.recovered.as_ref().unwrap();
                assert_eq!(modeled.command_id, case.command.id);
                assert_eq!(modeled.digest, case.command.digest);
                receipt.validate_for(&command).unwrap();
                let outcome = match receipt.outcome {
                    GoalControlOutcome::Applied { .. } => "applied",
                    GoalControlOutcome::Rejected { .. } => "rejected",
                };
                assert_eq!(modeled.outcome, outcome, "{}", case.name);
            }
            None => assert!(case.recovered.is_none(), "{}", case.name),
        }
        assert_eq!(
            access.execute(query).await.unwrap(),
            before,
            "{}: recovery cannot publish or mutate the original intent",
            case.name
        );
        driver
            .assert_state(&case.before.goal, &before_receipts, &case.name)
            .await;
        driver
            .apply(
                &case.command,
                &case.facts,
                case.now,
                &case.next_composite,
                case.commit,
            )
            .await;
        let expected_receipts = case
            .expected
            .receipts
            .iter()
            .map(|row| (row.command_id, row.digest, row.outcome.clone()))
            .collect::<Vec<_>>();
        driver
            .assert_state(&case.expected.goal, &expected_receipts, &case.name)
            .await;
        driver.fixture.node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn modeled_publication_retries_pending_receipt_relationships() {
    let seed = &gents_lean_vocab::lean_contract_snapshot().goal_client_receipt_cases[0];
    for trace in gents_lean_vocab::families::identity_publication_receipt::cases() {
        let mut driver = Driver::new(&seed.initial).await;
        let (command_doc, command) = driver.command(&seed.command).await;
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
                    pending: agent_access.execute(&query).await.unwrap()["data"]
                        ["AgentSessionGoalReceipt"][0]["relationships_pending"]
                        .as_bool()
                        .unwrap(),
                    visible: !user_access.execute(&query).await.unwrap()["data"]
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
