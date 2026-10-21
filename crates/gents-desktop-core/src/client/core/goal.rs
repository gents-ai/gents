use anyhow::{Context, Result};
use chrono::Utc;
#[cfg(test)]
use gents::goal::bind_goal_input;
use gents_protocol::goal_control::{
    GoalOperation, GoalRevision, SessionGoalControl, SessionGoalControlReceipt,
    SessionGoalObservation, SESSION_GOAL_CONTROL_VERSION,
};
use gents_protocol::request_input::{GoalAssignmentInput, QueueDelivery, RequestInput};

use super::writes::ensure_peer_chat_ready_at;
use super::ClientCore;
use crate::client::mutations::{prepare_request_submission, SubmitRequestOptions};

impl ClientCore {
    pub(super) async fn prepare_goal_input(
        &self,
        node_did: &str,
        session_id: &str,
        input: RequestInput,
    ) -> Result<RequestInput> {
        if input.goal_assignment.is_some() {
            return Ok(input);
        }
        let requester_did = self
            .sync_state
            .records()
            .iter()
            .find(|record| record.node_did == node_did)
            .map(|record| self.requester_did(record))
            .unwrap_or_else(|| self.node_identity.did().to_owned());
        gents::goal::prepare_goal_input(
            &self
                .operator_observation_graphql(node_did)
                .map(gents::config_client::ConfigAccess::Graphql)
                .unwrap_or_else(|| gents::config_client::ConfigAccess::local(self.node_arc())),
            &self.node_identity,
            node_did,
            session_id,
            Some(&requester_did),
            input,
        )
        .await
    }

    pub async fn session_goal_observation(
        &self,
        node_did: &str,
        session_id: &str,
    ) -> Result<Option<SessionGoalObservation>> {
        let record = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node_did)
            .context("no saved deployment route owns this node")?;
        gents::goal::load_session_goal_observation(
            &self
                .operator_observation_graphql(node_did)
                .map(gents::config_client::ConfigAccess::Graphql)
                .unwrap_or_else(|| gents::config_client::ConfigAccess::local(self.node_arc())),
            &self.node_identity,
            node_did,
            session_id,
            Some(&self.requester_did(&record)),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn control_session_goal(
        &self,
        node_did: &str,
        session_id: &str,
        session_doc_id: Option<String>,
        expected: Option<GoalRevision>,
        operation: GoalOperation,
        agent_id: Option<&str>,
        mut input: RequestInput,
    ) -> Result<SessionGoalControlReceipt> {
        let peer = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer.as_ref(), Utc::now())?;
        if matches!(
            operation,
            GoalOperation::Stop {} | GoalOperation::Create { .. }
        ) {
            crate::local_runtime::ensure_goal_stop_supported(peer.as_ref()).await?;
        }
        crate::local_runtime::ensure_request_input_supported(peer.as_ref(), &input).await?;
        let (signer, admission, requester_did) =
            self.request_authority(node_did, peer.as_ref()).await?;
        let command_id = uuid::Uuid::new_v4().to_string();
        let initial_request = if let GoalOperation::Create { objective, .. } = &operation {
            let goal_id = if expected.is_some() {
                gents::goal::replacement_goal_identity(node_did, session_id, &command_id).goal_id
            } else {
                gents::goal::deterministic_goal_id(node_did, session_id)
            };
            input.goal_assignment = Some(GoalAssignmentInput {
                goal_id,
                root_request_doc_id: None,
            });
            let input = self
                .prepare_user_message_input_with_delivery(
                    node_did,
                    session_id,
                    input,
                    QueueDelivery::Queue,
                )
                .await?;
            let snapshot = self.store.snapshot();
            let (_, request) = prepare_request_submission(
                self.node.as_ref(),
                snapshot.as_ref(),
                session_id,
                node_did,
                &requester_did,
                signer.as_ref(),
                admission,
                objective,
                agent_id,
                SubmitRequestOptions {
                    input,
                    retry_key: Some(format!("session-goal:{command_id}")),
                    ..Default::default()
                },
                None,
            )
            .await?;
            Some(Box::new(request))
        } else {
            None
        };
        let now = Utc::now();
        let command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id,
            requester_did,
            requester_peer_id: (signer.did() != node_did).then(|| self.local_peer_id().to_owned()),
            node_did: node_did.to_owned(),
            session_id: session_id.to_owned(),
            session_doc_id,
            issued_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
            expected,
            operation,
            initial_request,
            signature: Vec::new(),
        };
        gents::goal::submit_session_goal_control(
            &self.session_queue_access(node_did),
            signer.as_ref(),
            command,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::goal::SessionGoalObservationState;
    use gents_protocol::goal_control::ObservedGoal;
    use serde_json::{json, Value};

    fn reference(value: &Value) -> Option<GoalAssignmentInput> {
        (!value.is_null()).then(|| GoalAssignmentInput {
            goal_id: format!("goal:{}", value["goal"].as_u64().unwrap()),
            root_request_doc_id: value["root"]
                .as_u64()
                .map(|root| format!("request-document:{root}")),
        })
    }

    fn observation(reference: Option<GoalAssignmentInput>) -> SessionGoalObservation {
        SessionGoalObservation {
            version: SESSION_GOAL_CONTROL_VERSION,
            node_did: "did:key:source".into(),
            session_id: "chat".into(),
            session_doc_id: "session-document".into(),
            requester_did: Some("did:key:source".into()),
            observed_at: "2026-10-21T00:00:00Z".into(),
            goal: reference.map(|reference| {
                let goal: gents::goal::GoalDocument = serde_json::from_value(json!({
                    "_docID": "goal-document", "node_did": "did:key:source",
                    "session_id": "chat", "goal_id": reference.goal_id,
                    "objective": "Preserve this assignment", "status": "active"
                }))
                .unwrap();
                ObservedGoal {
                    revision: GoalRevision {
                        document: goal.doc_id.clone(),
                        composite: "source-composite".into(),
                    },
                    assignment_root_request_doc_id: reference.root_request_doc_id,
                    snapshot: gents::goal::snapshot(&goal, Utc::now()),
                }
            }),
            signature: Vec::new(),
        }
    }

    #[test]
    fn generated_goal_input_bindings_wait_for_verified_source_state() {
        let contracts: Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = contracts["goal_input_binding_cases"].as_array().unwrap();
        assert_eq!(cases.len(), 12);
        for case in cases {
            let mut input: RequestInput = serde_json::from_value(json!({
                "captured_context": [{"kind":"file", "source":"editor",
                    "capturedAt":"2026-10-21T00:00:00Z", "uri":"file:///workspace/main.rs",
                    "terminalName":null, "workspaceUri":"file:///workspace", "version":4,
                    "unsaved":true, "range":null, "content":"private unsaved capture"}]
            }))
            .unwrap();
            input.goal_assignment = reference(&case["captured"]);
            let original = input.clone();
            let state = if !case["session_readable"].as_bool().unwrap() {
                SessionGoalObservationState::NoReadableSession
            } else if !case["observation_verified"].as_bool().unwrap() {
                SessionGoalObservationState::Unknown
            } else {
                SessionGoalObservationState::Observed(Box::new(observation(reference(
                    &case["observed"],
                ))))
            };
            let actual = bind_goal_input(input, state);
            if case["expected"]["kind"] == "pending" {
                assert!(
                    actual.unwrap_err().to_string().contains("still syncing"),
                    "{case}"
                );
            } else {
                let mut expected = original;
                expected.goal_assignment = reference(&case["expected"]["reference"]);
                assert_eq!(actual.unwrap(), expected, "{case}");
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn followup_preparation_verifies_the_hosted_assignment_before_queueing() -> Result<()> {
        use crate::client::{ClientCoreOptions, DesktopPaths};
        use gents::NodeIdentity;
        use std::sync::Arc;
        use wiremock::{
            matchers::{body_string_contains, method, path},
            Mock, MockServer, ResponseTemplate,
        };

        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let source =
            gents::identity::KeyIdentity::load_or_create(root.path().join("source.key"), None)?;
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(root.path().join("client")),
            ClientCoreOptions::local_only(),
        )
        .await?;
        core.persist_local_standard_peer(
            "Hosted runtime",
            "endpoint:hosted-goal-owner",
            source.did(),
            &format!("{}/api/v0/graphql", server.uri()),
            "/tmp/disposable-goal-owner",
        )
        .await?;
        let expected = GoalAssignmentInput {
            goal_id: "goal:2".into(),
            root_request_doc_id: Some("request-document:200".into()),
        };
        for mode in [
            "missing",
            "tampered",
            "foreign-session",
            "absent",
            "legacy",
            "assigned",
        ] {
            server.reset().await;
            Mock::given(method("POST"))
                .and(path("/api/v0/tx"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":17})))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/api/v0/tx/17"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(body_string_contains("AgentSession"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":{"AgentSession":[{
                    "_docID":"session-document", "session_id":"chat", "node_did":source.did(),
                    "requester_did":source.did(), "agent_id":"engineer", "created_at":"2026-10-21T00:00:00Z"
                }]}})))
                .mount(&server).await;
            let mut signed = observation((mode != "absent").then(|| expected.clone()));
            signed.node_did = source.did().into();
            signed.requester_did = Some(source.did().into());
            if let Some(goal) = &mut signed.goal {
                goal.snapshot.node_did = source.did().into();
                if mode == "legacy" {
                    goal.assignment_root_request_doc_id = None;
                }
            }
            if mode == "foreign-session" {
                signed.session_doc_id = "another-physical-session".into();
            }
            signed.signature = source.sign(&signed.signing_payload()?).await?;
            if mode == "tampered" {
                signed.goal.as_mut().unwrap().assignment_root_request_doc_id =
                    Some("forged-root".into());
            }
            let rows = if mode == "missing" {
                json!([])
            } else {
                json!([{
                    "_docID":"observation-document", "node_did":source.did(), "session_id":"chat",
                    "session_doc_id":"session-document", "requester_did":source.did(),
                    "observation_json":serde_json::to_string(&signed)?
                }])
            };
            Mock::given(method("POST"))
                .and(body_string_contains("GoalObservation"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"data":{"GoalObservation":rows}})),
                )
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(body_string_contains("AgentRequest"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"data":{"AgentRequest":[]}})),
                )
                .mount(&server)
                .await;

            let actual = core
                .prepare_user_message_input(source.did(), "chat", RequestInput::default())
                .await;
            if ["missing", "tampered", "foreign-session"].contains(&mode) {
                let error = actual.unwrap_err();
                assert!(
                    error.to_string().contains("still syncing"),
                    "{mode}: {error:#}"
                );
                let requests = server.received_requests().await.unwrap();
                assert!(
                    !requests
                        .iter()
                        .any(|request| String::from_utf8_lossy(&request.body)
                            .contains("AgentRequest")),
                    "{mode}"
                );
            } else {
                let input = actual?;
                assert_eq!(
                    input.goal_assignment,
                    (mode == "assigned").then(|| expected.clone()),
                    "{mode}"
                );
                assert!(input.queue.is_some(), "{mode}");
            }
        }
        server.reset().await;
        let opening = RequestInput {
            goal_assignment: Some(GoalAssignmentInput {
                root_request_doc_id: None,
                ..expected
            }),
            ..Default::default()
        };
        assert_eq!(
            core.prepare_goal_input(source.did(), "chat", opening.clone())
                .await?,
            opening
        );
        assert!(server.received_requests().await.unwrap().is_empty());
        Arc::new(core).shutdown().await?;
        Ok(())
    }
}
