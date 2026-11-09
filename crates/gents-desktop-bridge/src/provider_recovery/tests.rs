use super::*;
use serde_json::json;
use std::sync::Arc;

/// A served runtime the User configures and recovers through its signed
/// endpoint, as `desktop_provider_recovery_*` reach it.
struct Fixture {
    _runtime: gents_desktop_core::test_fixtures::ServedRuntimeHome,
    identity: Arc<dyn gents::NodeIdentity>,
    access: ConfigAccess,
    scope: ProviderRecoveryRequest,
}

impl Fixture {
    async fn new(reset: bool) -> Self {
        let runtime = gents_desktop_core::test_fixtures::ServedRuntimeHome::start().await;
        let identity = runtime.user().clone();
        let owner = runtime.node_did().to_owned();
        let access = runtime.user_access();
        let observer = runtime.runtime_access();
        for (id, name, enabled, model) in [
            ("main", "Work", true, "model-a"),
            ("backup", "Backup", true, "model-a"),
            ("disabled", "Disabled", false, "model-a"),
            ("wrong-model", "Wrong model", true, "model-b"),
        ] {
            let backend: gents::InferenceBackend = serde_json::from_value(json!({
                "node_did": owner, "backend_id": id, "name": name,
                "provider_kind": "AnthropicApiKey", "endpoint": "https://api.anthropic.com/v1",
                "enabled": enabled, "auth": {"kind":"api_key", "key":format!("TOKEN-SECRET-{id}")},
            }))
            .unwrap();
            gents::documents::write_inference_backend_document(&access, &backend)
                .await
                .unwrap();
            // Catalog discovery is the runtime's own observation.
            observer
                .transact_write("test.recovery.catalog", |txn| {
                    let backend = &backend;
                    Box::pin(async move {
                        // Discovery observes the stored connection, whose key
                        // the runtime moved into a node credential.
                        let (_, stored) =
                            gents::store::config_client::read_desired_state_record_in_txn(
                                txn,
                                gents::Collection::InferenceBackend,
                                &backend.node_did,
                                &backend.backend_id,
                            )
                            .await?
                            .expect("stored backend");
                        let stored: gents::InferenceBackend = serde_json::from_value(stored)?;
                        gents::record_model_catalog_in_txn(
                            txn,
                            &stored,
                            gents::documents::BackendModelCatalog {
                                node_did: backend.catalog_scope().map(str::to_owned),
                                observed_at: chrono::Utc::now().to_rfc3339(),
                                models: vec![gents::documents::AdvertisedModel {
                                    model_name: model.into(),
                                    display_name: None,
                                    context_window: None,
                                    max_context_window: None,
                                    max_output_tokens: None,
                                    reasoning_efforts: None,
                                }],
                            },
                        )
                        .await
                    })
                })
                .await
                .unwrap();
        }
        let profile = serde_json::from_value(json!({
            "node_did":owner, "profile_id":"profile-a", "backend_id":"main", "model_name":"model-a",
        }))
        .unwrap();
        gents::documents::write_inference_profile_document(&access, &profile)
            .await
            .unwrap();
        let engineer = runtime.provision_agent().await;
        for binding in [&engineer, &runtime.provision_agent().await] {
            let agent = serde_json::from_value(runtime.agent_value(binding, "profile-a")).unwrap();
            gents::documents::write_agent_document(&access, &agent)
                .await
                .unwrap();
        }
        let reason = if reset {
            "HttpError: Invalid status code 429 Too Many Requests with message: {\"error\":{\"type\":\"usage_limit_reached\",\"resets_at\":4102444800}}"
        } else {
            "HttpError: Invalid status code 429 Too Many Requests with message: {\"error\":{\"type\":\"usage_limit_reached\"}}"
        };
        let mut request = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "stopped",
            &owner,
            identity.did(),
            &engineer.agent_did,
            "session",
            "Finish the work",
            "interactive",
            "2026-10-23T11:00:00Z",
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(identity.did()),
        );
        request.session_doc_id = Some(
            runtime
                .agent_session(&engineer, "session", identity.did())
                .await,
        );
        request.agent_doc_id = Some(runtime.agent_doc_id(&engineer).await);
        gents::sign_agent_request_create(identity.as_ref(), &mut request)
            .await
            .unwrap();
        runtime
            .user_graphql_access()
            .write(
                "test.recovery.request",
                &request.graphql_mutation().unwrap(),
            )
            .await
            .unwrap();
        let did = gents::store::graphql::escape_graphql_string(&owner);
        let engineer_did = gents::store::graphql::escape_graphql_string(&engineer.agent_did);
        let reason = gents::store::graphql::escape_graphql_string(reason);
        // The working Agent records the failed turn and its inference calls.
        runtime.agent_access(&engineer).await.write("test.recovery.failed_request", &format!(r#"mutation {{
            update_AgentRequest(filter: {{request_id: {{_eq: "stopped"}}}}, input: {{lifecycle_state: "failed"}}) {{_docID}}
            own_call: create_InferenceCall(input: {{call_id: "failed-call", request_id: "stopped", node_did: "{did}",
                agent_did: "{engineer_did}", backend_id: "main", call_kind: "inference", call_state: "failed",
                attempt: 1, failure_reason: "{reason}", ended_at: "2026-10-23T12:00:00Z"}}) {{_docID}}
            foreign_call: create_InferenceCall(input: {{call_id: "foreign-call", request_id: "stopped", node_did: "did:key:foreign",
                agent_did: "{engineer_did}", backend_id: "foreign", call_kind: "inference", call_state: "failed",
                attempt: 2, failure_reason: "unrelated failure", ended_at: "2099-10-23T12:00:00Z"}}) {{_docID}}
        }}"#)).await.unwrap();
        gents::goal::set_goal_from_access(
            &access,
            &owner,
            "session",
            Some("Finish the work"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        gents::goal::set_goal_from_access(
            &access,
            &owner,
            "session",
            None,
            Some(gents::goal::GoalStatus::UsageLimited),
            None,
            None,
        )
        .await
        .unwrap();
        let scope = ProviderRecoveryRequest {
            node_did: owner.clone(),
            session_id: "session".into(),
            request_id: "stopped".into(),
        };
        Self {
            _runtime: runtime,
            identity,
            access,
            scope,
        }
    }
}

#[tokio::test]
async fn native_plan_excludes_disabled_or_wrong_model_accounts_and_moves_one_profile() {
    let fixture = Fixture::new(true).await;
    let view = plan(&fixture.access, &fixture.scope)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(view.reason, "usage_limit");
    assert_eq!(view.affected_agents.len(), 2);
    assert!(view.resets_at.is_some());
    assert_eq!(view.candidates.len(), 1);
    assert_eq!(view.candidates[0].label, "Backup");
    assert!(!view.goal.as_ref().unwrap().auto_resume_at_reset);
    assert!(!serde_json::to_string(&view)
        .unwrap()
        .contains("TOKEN-SECRET"));
    let receipt = apply(
        &fixture.access,
        fixture.identity.as_ref(),
        &ProviderRecoveryApply {
            scope: fixture.scope.clone(),
            action: ProviderRecoveryAction::MoveProfile {
                profile_id: "profile-a".into(),
                backend_id: "backup".into(),
                with_compaction: false,
            },
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        receipt,
        ProviderRecoveryResult::Applied {
            request_id: None,
            ..
        }
    ));
    let response = fixture
        .access
        .execute("{ InferenceProfile { profile_id backend_id } AgentRequest {request_id} }")
        .await
        .unwrap();
    assert_eq!(
        response["data"]["InferenceProfile"][0]["backend_id"],
        "backup"
    );
    assert_eq!(
        response["data"]["AgentRequest"].as_array().unwrap().len(),
        1,
        "moving a profile does not retry the turn"
    );
}

#[tokio::test]
async fn native_reset_opt_in_is_goal_scoped_and_stale_or_ineligible_actions_do_not_write() {
    let fixture = Fixture::new(false).await;
    let view = plan(&fixture.access, &fixture.scope)
        .await
        .unwrap()
        .unwrap();
    assert!(view.resets_at.is_none());
    let goal_id = view.goal.unwrap().goal_id;
    let setting = |goal_id: String| ProviderRecoveryApply {
        scope: fixture.scope.clone(),
        action: ProviderRecoveryAction::SetAutoResume {
            goal_id,
            enabled: true,
        },
    };
    assert!(matches!(
        apply(
            &fixture.access,
            fixture.identity.as_ref(),
            &setting("stale-goal".into())
        )
        .await
        .unwrap(),
        ProviderRecoveryResult::Stale { .. }
    ));
    assert!(matches!(
        apply(
            &fixture.access,
            fixture.identity.as_ref(),
            &setting(goal_id)
        )
        .await
        .unwrap(),
        ProviderRecoveryResult::Applied { .. }
    ));
    let view = plan(&fixture.access, &fixture.scope)
        .await
        .unwrap()
        .unwrap();
    assert!(view.goal.unwrap().auto_resume_at_reset);
    let refused = ProviderRecoveryApply {
        scope: fixture.scope.clone(),
        action: ProviderRecoveryAction::MoveProfile {
            profile_id: "profile-a".into(),
            backend_id: "disabled".into(),
            with_compaction: false,
        },
    };
    assert!(matches!(
        apply(&fixture.access, fixture.identity.as_ref(), &refused)
            .await
            .unwrap(),
        ProviderRecoveryResult::Refused { .. }
    ));
    let mut wrong_session = fixture.scope.clone();
    wrong_session.session_id = "other-session".into();
    assert!(plan(&fixture.access, &wrong_session).await.is_err());
}

#[tokio::test]
async fn native_resume_publishes_one_goal_continuation_on_the_selected_account() {
    let fixture = Fixture::new(true).await;
    let goal_id = plan(&fixture.access, &fixture.scope)
        .await
        .unwrap()
        .unwrap()
        .goal
        .unwrap()
        .goal_id;
    let request = ProviderRecoveryApply {
        scope: fixture.scope.clone(),
        action: ProviderRecoveryAction::ResumeGoal {
            goal_id,
            backend_id: "backup".into(),
            with_compaction: false,
        },
    };
    let receipt = apply(&fixture.access, fixture.identity.as_ref(), &request)
        .await
        .unwrap();
    assert!(
        matches!(
            receipt,
            ProviderRecoveryResult::Applied {
                request_id: Some(_),
                ..
            }
        ),
        "{receipt:?}"
    );
    let _ = apply(&fixture.access, fixture.identity.as_ref(), &request)
        .await
        .unwrap();
    let rows = fixture
        .access
        .execute("{ AgentRequest { request_id } }")
        .await
        .unwrap();
    assert_eq!(rows["data"]["AgentRequest"].as_array().unwrap().len(), 2);
}
