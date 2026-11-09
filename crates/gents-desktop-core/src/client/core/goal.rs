use anyhow::Result;
use chrono::Utc;
#[cfg(test)]
use gents::goal::bind_goal_input;
use gents::goal::SessionGoalObservationState;
#[cfg(test)]
use gents_protocol::goal_control::SessionGoalObservation;
use gents_protocol::goal_control::{
    GoalOperation, GoalRevision, SessionGoalControl, SessionGoalControlReceipt,
    SESSION_GOAL_CONTROL_VERSION,
};
use gents_protocol::request_input::{GoalAssignmentInput, QueueDelivery, RequestInput};

use super::writes::ensure_peer_chat_ready_at;
use super::ClientCore;

mod delivery;
use crate::client::mutations::{prepare_request_submission, SubmitRequestOptions};
pub use delivery::{GoalSubmissionReference, RetainedGoalSubmission};

impl ClientCore {
    pub(super) async fn prepare_goal_input(
        &self,
        node_did: &str,
        session_id: &str,
        input: RequestInput,
    ) -> Result<RequestInput> {
        gents::goal::prepare_client_goal_input(
            &self.session_queue_access(node_did)?,
            &self.node_identity,
            node_did,
            session_id,
            input,
        )
        .await
    }

    /// The hosted session's Goal state from one read transaction, never
    /// collapsed: an unknown projection is not a session without a Goal.
    pub async fn session_goal_observation(
        &self,
        node_did: &str,
        session_id: &str,
    ) -> Result<SessionGoalObservationState> {
        gents::goal::load_client_session_goal_observation_state(
            &self.session_queue_access(node_did)?,
            &self.node_identity,
            node_did,
            session_id,
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
        agent_did: Option<&str>,
        input: RequestInput,
    ) -> Result<SessionGoalControlReceipt> {
        let (signer, command) = self
            .prepare_session_goal_command(
                node_did,
                session_id,
                session_doc_id,
                expected,
                operation,
                agent_did,
                input,
                uuid::Uuid::new_v4().to_string(),
            )
            .await?;
        gents::goal::submit_prepared_session_goal_control(
            &self.session_queue_access(node_did)?,
            signer.as_ref(),
            &command,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn prepare_session_goal_command(
        &self,
        node_did: &str,
        session_id: &str,
        session_doc_id: Option<String>,
        expected: Option<GoalRevision>,
        operation: GoalOperation,
        agent_did: Option<&str>,
        mut input: RequestInput,
        command_id: String,
    ) -> Result<(
        std::sync::Arc<dyn gents::store::identity::NodeIdentity>,
        SessionGoalControl,
    )> {
        let peer = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer.as_ref(), Utc::now())?;
        if matches!(operation, GoalOperation::Stop {})
            || (matches!(operation, GoalOperation::Create { .. }) && expected.is_some())
        {
            crate::local_runtime::ensure_goal_stop_supported(peer.as_ref()).await?;
        }
        crate::local_runtime::ensure_request_input_supported(peer.as_ref(), &input).await?;
        let (signer, admission, requester_did) =
            self.request_authority(node_did, peer.as_ref()).await?;
        let requester_did = gents::session::client_requester_scope(
            &self.session_queue_access(node_did)?,
            node_did,
            session_id,
            &requester_did,
        )
        .await?;
        let mut session_doc_id = session_doc_id;
        let initial_request = if let GoalOperation::Create { objective, .. } = &operation {
            let goal_id = if expected.is_some() {
                gents::goal::replacement_goal_identity(node_did, session_id, &command_id).goal_id
            } else {
                gents_protocol::goal::deterministic_goal_id(node_did, session_id)
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
                &self.session_queue_access(node_did)?,
                snapshot.as_ref(),
                session_id,
                node_did,
                &requester_did,
                signer.as_ref(),
                admission,
                objective,
                agent_did,
                SubmitRequestOptions {
                    input,
                    retry_key: Some(format!("session-goal:{command_id}")),
                    ..Default::default()
                },
                None,
            )
            .await?;
            if session_doc_id.is_none() {
                anyhow::ensure!(
                    expected.is_none(),
                    "replacing a goal requires its observed session"
                );
                session_doc_id = Some(
                    gents::request::lifecycle::reserve_goal_session(
                        &self.session_queue_access(node_did)?,
                        &request,
                    )
                    .await?,
                );
            }
            Some(Box::new(request))
        } else {
            None
        };
        let access = self.session_queue_access(node_did)?;
        let (session, agent_doc_id) = access
            .transact_readonly("goal.command.selected_session", |txn| {
                Box::pin(async move {
                    let session =
                        gents::store::config_client::agent_session::load_agent_session_owner_in_txn(txn, node_did, session_id)
                            .await?
                            .ok_or_else(|| anyhow::anyhow!("selected session is not available"))?;
                    let (agent_doc_id, _) =
                        gents::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            gents::Collection::Agent,
                            node_did,
                            &session.session.agent_did,
                        )
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("selected agent is not available"))?;
                    Ok::<_, anyhow::Error>((session, agent_doc_id))
                })
            })
            .await?;
        anyhow::ensure!(
            session_doc_id.as_deref() == Some(session.doc_id.as_str()),
            "session changed; refresh before changing its goal"
        );
        anyhow::ensure!(
            initial_request
                .as_ref()
                .is_none_or(|request| request.agent_did == session.session.agent_did
                    && request.agent_doc_id.as_deref() == Some(agent_doc_id.as_str())
                    && request.session_doc_id.as_deref() == Some(session.doc_id.as_str())),
            "goal request targets changed; refresh before starting its goal"
        );
        let now = Utc::now();
        let command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id,
            requester_did: signer.did().to_owned(),
            requester_peer_id: None,
            node_did: node_did.to_owned(),
            agent_did: session.session.agent_did,
            agent_doc_id,
            session_id: session_id.to_owned(),
            session_doc_id,
            issued_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
            expected,
            operation,
            initial_request,
            native_evidence: None,
            signature: Vec::new(),
        };
        let command =
            gents::goal::prepare_session_goal_control(&access, signer.as_ref(), command).await?;
        Ok((signer, command))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            agent_did: "did:key:agent".into(),
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
                SessionGoalObservationState::Unknown(
                    gents::goal::SessionGoalUnknownCause::ProjectionUnverified,
                )
            } else {
                SessionGoalObservationState::Observed(Box::new(observation(reference(
                    &case["observed"],
                ))))
            };
            let actual = bind_goal_input(input, state);
            if case["expected"]["kind"] == "pending" {
                assert_eq!(
                    actual.unwrap_err(),
                    gents::goal::GoalInputRefused::ProjectionUnverified,
                    "{case}"
                );
            } else {
                let mut expected = original;
                expected.goal_assignment = reference(&case["expected"]["reference"]);
                assert_eq!(actual.unwrap(), expected, "{case}");
            }
        }
    }

    /// A send to an existing hosted chat binds its Goal assignment only from
    /// the Agent's published, signed projection, read as the User.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn followup_preparation_verifies_the_hosted_assignment_before_queueing() -> Result<()> {
        use crate::client::{ClientCoreOptions, DesktopPaths};
        use crate::test_fixtures::ServedRuntimeHome;
        use gents::store::collection::Collection;
        use gents::store::graphql::escape_graphql_string;
        use std::sync::Arc;

        let root = tempfile::tempdir()?;
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(root.path().join("client")),
            ClientCoreOptions::local_only(),
        )
        .await?;
        let runtime =
            ServedRuntimeHome::start_unpublished_for(Arc::new(core.node_identity().clone())).await;
        runtime.publish_node(json!({})).await;
        runtime.connect(&core).await;
        let node_did = runtime.node_did().to_owned();
        let binding = runtime.provision_agent().await;
        runtime
            .apply_as_user(
                "test.goal_followup.agent",
                vec![
                    (
                        Collection::InferenceBackend,
                        json!({"node_did": node_did, "backend_id": "backend", "name": "Backend",
                            "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
                            "auth": {"kind": "unauthenticated"}}),
                    ),
                    (
                        Collection::InferenceProfile,
                        json!({"node_did": node_did, "profile_id": "profile",
                            "backend_id": "backend", "model_name": "model"}),
                    ),
                    (Collection::Agent, runtime.agent_value(&binding, "profile")),
                ],
            )
            .await;
        runtime
            .agent_session(&binding, "chat", core.node_identity().did())
            .await;
        let agent_access = runtime.agent_access(&binding).await;
        let agent =
            gents::store::identity::resolve_agent_identity(runtime.runtime().as_ref(), &binding)?;
        let project = || {
            gents::goal::project_session_goal(
                runtime.node(),
                runtime.runtime(),
                &agent,
                "chat",
                Some(core.node_identity().did()),
            )
        };
        let send = || core.prepare_user_message_input(&node_did, "chat", RequestInput::default());
        let refused = |result: Result<RequestInput>, expected: gents::goal::GoalInputRefused| {
            let error = result.expect_err("an unknown Goal state refuses the send");
            assert_eq!(
                gents_protocol::error_chain::find_source::<gents::goal::GoalInputRefused>(
                    error.as_ref()
                )
                .copied(),
                Some(expected),
                "{error:#}"
            );
        };

        refused(
            send().await,
            gents::goal::GoalInputRefused::ProjectionMissing,
        );

        project().await?;
        let input = send().await?;
        assert_eq!(input.goal_assignment, None, "verified absence");
        assert!(input.queue.is_some(), "verified absence queues");

        let goal = gents::goal::set_goal_from_access(
            &agent_access,
            &node_did,
            "chat",
            Some("Finish the hosted work"),
            None,
            None,
            None,
        )
        .await?;
        project().await?;
        let input = send().await?;
        assert_eq!(
            input.goal_assignment, None,
            "an unbound Goal cannot supply a physical root"
        );

        agent_access
            .write(
                "test.goal_followup.bind_root",
                &format!(
                    r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ assignment_root_request_doc_id: "opening-request-document" }}) {{ _docID }} }}"#,
                    escape_graphql_string(&goal.doc_id)
                ),
            )
            .await?;
        project().await?;
        let expected = GoalAssignmentInput {
            goal_id: goal.goal_id.clone(),
            root_request_doc_id: Some("opening-request-document".into()),
        };
        let input = send().await?;
        assert_eq!(input.goal_assignment, Some(expected.clone()), "assigned");
        assert!(input.queue.is_some(), "assigned queues");

        let response = agent_access
            .execute("{ GoalObservation { _docID observation_json } }")
            .await?;
        let row = &response["data"]["GoalObservation"][0];
        let mut forged: SessionGoalObservation =
            serde_json::from_str(row["observation_json"].as_str().unwrap())?;
        forged.goal.as_mut().unwrap().assignment_root_request_doc_id = Some("forged-root".into());
        agent_access
            .write(
                "test.goal_followup.tamper",
                &format!(
                    r#"mutation {{ update_GoalObservation(docID: "{}", input: {{ observation_json: "{}" }}) {{ _docID }} }}"#,
                    escape_graphql_string(row["_docID"].as_str().unwrap()),
                    escape_graphql_string(&serde_json::to_string(&forged)?)
                ),
            )
            .await?;
        refused(
            send().await,
            gents::goal::GoalInputRefused::ProjectionUnverified,
        );

        let opening = RequestInput {
            goal_assignment: Some(GoalAssignmentInput {
                root_request_doc_id: None,
                ..expected
            }),
            ..Default::default()
        };
        assert_eq!(
            core.prepare_goal_input(&node_did, "chat", opening.clone())
                .await?,
            opening,
            "a captured reference is immutable intent"
        );
        Arc::new(core).shutdown().await?;
        runtime.shutdown().await;
        Ok(())
    }

    /// The desktop reads its runtime as the User, so the runtime key's lock
    /// state never gates or unlocks the read, and every read is signed. A
    /// session row ACP filters out reads as no readable session (DefraDB
    /// returns an empty list), never "no Goal"; a refusal stays an error.
    #[tokio::test]
    async fn cold_operator_goal_observation_stays_unknown_without_unlocking() -> Result<()> {
        use crate::client::{ClientCoreOptions, DesktopPaths};
        use identity::Identity as _;
        use std::sync::Arc;
        use wiremock::{
            matchers::{body_string_contains, method, path},
            Mock, MockServer, ResponseTemplate,
        };

        let server = MockServer::start().await;
        let root = tempfile::tempdir()?;
        let key = root.path().join("runtime.key");
        let source_key = key.clone();
        let did = tokio::task::spawn_blocking(move || -> Result<String> {
            let identity = gents::store::identity::load_or_create_file_identity(&source_key)?;
            Ok(identity.did()?.to_string())
        })
        .await??;
        let endpoint = format!("{}/api/v0/graphql", server.uri());
        tokio::fs::write(
            root.path().join("init.json"),
            json!({
                "node_name":"Goal runtime", "node_did":did, "key_path":key,
            })
            .to_string(),
        )
        .await?;
        tokio::fs::write(
            root.path().join("runtime.json"),
            json!({
                "node_name":"Goal runtime", "node_did":did, "graphql":endpoint,
                "p2p_transport":"iroh",
            })
            .to_string(),
        )
        .await?;
        let original_key = tokio::fs::read(&key).await?;
        assert!(!gents::store::identity::can_mint_defradb_bearer(&did));
        Mock::given(method("POST"))
            .and(path("/api/v0/tx"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"goal-read"})))
            .mount(&server)
            .await;
        for verb in ["POST", "DELETE"] {
            Mock::given(method(verb))
                .and(path("/api/v0/tx/goal-read"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path("/api/v0/graphql"))
            .and(body_string_contains("warm-goal-session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"data": null, "errors": [{"message": "document access denied"}]}),
            ))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v0/graphql"))
            .and(body_string_contains("cold-goal-session"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data":{"AgentSession":[]}})),
            )
            .mount(&server)
            .await;
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(root.path().join("client")),
            ClientCoreOptions {
                operator_identity_interaction:
                    crate::local_runtime::OperatorIdentityInteraction::Prevent,
                ..ClientCoreOptions::local_only()
            },
        )
        .await?;
        core.persist_local_standard_peer(
            "Goal runtime",
            "endpoint:cold-goal",
            &did,
            &endpoint,
            root.path().to_str().unwrap(),
        )
        .await?;
        let cold = core
            .session_goal_observation(&did, "cold-goal-session")
            .await;
        let remained_cold = !gents::store::identity::can_mint_defradb_bearer(&did);
        let cold_send = core
            .prepare_goal_input(&did, "cold-goal-session", RequestInput::default())
            .await;
        let source_key = key.clone();
        let _unlocked = tokio::task::spawn_blocking(move || {
            gents::store::identity::KeyIdentity::load_or_create(source_key, None)
        })
        .await??;
        assert!(gents::store::identity::can_mint_defradb_bearer(&did));
        let warm = core
            .session_goal_observation(&did, "warm-goal-session")
            .await;
        let requests = server.received_requests().await.unwrap();
        Arc::new(core).shutdown().await?;

        assert!(
            remained_cold,
            "background observation must not unlock the runtime identity"
        );
        assert!(
            tokio::fs::read(&key).await? == original_key,
            "runtime key must remain unchanged"
        );
        assert!(
            matches!(cold, Ok(SessionGoalObservationState::NoReadableSession)),
            "a filtered session row is no readable session: {cold:?}"
        );
        assert_eq!(
            cold_send?,
            RequestInput::default(),
            "a send to an unreadable session captures no inherited assignment"
        );
        let cold_reads = requests
            .iter()
            .filter(|request| String::from_utf8_lossy(&request.body).contains("cold-goal-session"))
            .collect::<Vec<_>>();
        assert_eq!(
            cold_reads.len(),
            2,
            "each cold read issues exactly its one session query"
        );
        assert!(
            requests
                .iter()
                .all(|request| request.headers.contains_key("authorization")),
            "every runtime read is signed as the User"
        );
        assert!(
            warm.is_err(),
            "an authenticated source refusal must remain explicit"
        );
        Ok(())
    }
}
