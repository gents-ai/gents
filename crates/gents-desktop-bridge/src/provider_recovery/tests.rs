use super::*;
use serde_json::json;
use std::sync::Arc;

/// A served runtime the User configures and recovers through its signed
/// endpoint, as `desktop_provider_recovery_*` reach it.
struct Fixture {
    runtime: gents::client_test_support::ServedRuntimeHome,
    engineer: gents_protocol::agent_identity::AgentIdentityBinding,
    identity: Arc<dyn gents_store::identity::NodeIdentity>,
    access: ConfigAccess,
}

impl Fixture {
    async fn new() -> Self {
        let runtime = gents::client_test_support::ServedRuntimeHome::start().await;
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
            let backend: gents_documents::InferenceBackend = serde_json::from_value(json!({
                "node_did": owner, "backend_id": id, "name": name,
                "provider_kind": "AnthropicApiKey", "endpoint": "https://api.anthropic.com/v1",
                "enabled": enabled, "auth": {"kind":"api_key", "key":format!("TOKEN-SECRET-{id}")},
            }))
            .unwrap();
            gents_documents::write_inference_backend_document(
                &access,
                &backend,
                &gents_inference::resolved::ProviderProfilePolicy,
            )
            .await
            .unwrap();
            // Catalog discovery is the runtime's own observation.
            observer
                .transact_write("test.recovery.catalog", |txn| {
                    let backend = &backend;
                    Box::pin(async move {
                        let (_, stored) = gents_documents::read_desired_state_record_in_txn(
                            txn,
                            gents_store::collection::Collection::InferenceBackend,
                            &backend.node_did,
                            &backend.backend_id,
                        )
                        .await?
                        .expect("stored backend");
                        let stored: gents_documents::InferenceBackend =
                            serde_json::from_value(stored)?;
                        gents_inference::backends::registry::record_model_catalog_in_txn(
                            txn,
                            &stored,
                            gents_documents::BackendModelCatalog {
                                node_did: backend.catalog_scope().map(str::to_owned),
                                observed_at: chrono::Utc::now().to_rfc3339(),
                                models: vec![gents_documents::AdvertisedModel {
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
        let engineer = runtime.provision_agent().await;
        let fixture = Self {
            runtime,
            engineer,
            identity,
            access,
        };
        fixture.bind_profile("main").await;
        for binding in [&fixture.engineer, &fixture.runtime.provision_agent().await] {
            let agent =
                serde_json::from_value(fixture.runtime.agent_value(binding, "profile-a")).unwrap();
            gents_documents::write_agent_document(
                &fixture.access,
                &agent,
                &gents_inference::resolved::ProviderProfilePolicy,
            )
            .await
            .unwrap();
        }
        fixture
    }

    async fn bind_profile(&self, backend_id: &str) {
        let profile = serde_json::from_value(json!({
            "node_did": self.runtime.node_did(), "profile_id": "profile-a",
            "backend_id": backend_id, "model_name": "model-a",
        }))
        .unwrap();
        gents_documents::write_inference_profile_document(
            &self.access,
            &profile,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await
        .unwrap();
    }

    async fn seed_blocked_session(
        &self,
        session_id: &str,
        request_id: &str,
        reset: bool,
    ) -> ProviderRecoveryRequest {
        let owner = self.runtime.node_did().to_owned();
        let reason = if reset {
            "HttpError: Invalid status code 429 Too Many Requests with message: {\"error\":{\"type\":\"usage_limit_reached\",\"resets_at\":4102444800}}"
        } else {
            "HttpError: Invalid status code 429 Too Many Requests with message: {\"error\":{\"type\":\"usage_limit_reached\"}}"
        };
        let mut request = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            request_id,
            gents_protocol::request_admission::RequestScope::new(
                &owner,
                self.identity.did(),
                &self.engineer.agent_did,
                session_id,
            ),
            "Finish the work",
            "interactive",
            "2026-10-23T11:00:00Z",
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(
                self.identity.did(),
            ),
        );
        let session_doc =
            gents_session::request::lifecycle::reserve_goal_session(&self.access, &request)
                .await
                .unwrap();
        request.session_doc_id = Some(session_doc.clone());
        request.agent_doc_id = Some(self.runtime.agent_doc_id(&self.engineer).await);
        gents_session::request::admission::sign_agent_request_create(
            self.identity.as_ref(),
            &mut request,
        )
        .await
        .unwrap();
        gents_session::request::lifecycle::submit_client_request(&self.access, &request)
            .await
            .unwrap();
        let request_doc = self
            .runtime
            .user_access()
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{request_id}"}}}}) {{ _docID }} }}"#
            ))
            .await
            .unwrap()["data"]["AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        let engineer_did = self.engineer.agent_did.clone();
        let (recorded_reason, limit) =
            gents_inference::admission::recovery_fixtures::recorded_failure(
                reason,
                chrono::Utc::now(),
                gents_loop::provider_limit::LONG_RATE_LIMIT_WAIT,
            );
        let calls = [
            json!({"call_id": format!("failed-{request_id}"), "request_id": request_id, "request_doc_id": request_doc,
                "session_id": session_id, "node_did": owner, "agent_did": engineer_did,
                "backend_id": "main", "call_kind": "inference", "call_state": "failed", "attempt": 1,
                "failure_reason": recorded_reason,
                "provider_limit": limit.map(|limit| limit.verdict.as_str()),
                "provider_limit_resets_at": limit.and_then(|limit| limit.resets_at).map(|at| at.to_rfc3339()),
                "ended_at": "2026-10-23T12:00:00Z"}),
            json!({"call_id": format!("foreign-{request_id}"), "request_id": request_id, "request_doc_id": request_doc,
                "session_id": session_id, "node_did": "did:key:foreign", "agent_did": engineer_did,
                "backend_id": "foreign", "call_kind": "inference", "call_state": "failed", "attempt": 2,
                "failure_reason": "unrelated failure", "ended_at": "2099-10-23T12:00:00Z"}),
        ];
        self.runtime
            .agent_access(&self.engineer)
            .await
            .transact_write("test.recovery.failed_calls", |txn| {
                let (calls, session_doc) = (&calls, &session_doc);
                Box::pin(async move {
                    for call in calls {
                        let response = txn
                            .execute_with_variables(
                                "mutation($input: InferenceCallMutationInputArg!) { create_InferenceCall(input: $input) {_docID} }",
                                &json!({"input": call}),
                            )
                            .await?;
                        let doc = gents_store::graphql::created_doc_id(&response, "InferenceCall")?;
                        txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session_doc)
                            .await?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        crate::tests::support::reject_admission(
            &self.runtime,
            &self.engineer,
            &request_doc,
            reason,
        )
        .await;
        gents_automation::goal::seed_goal_from_access(
            &self.access,
            &owner,
            session_id,
            Some("Finish the work"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        gents_automation::goal::seed_goal_from_access(
            &self.access,
            &owner,
            session_id,
            None,
            Some(gents_protocol::goal_status::GoalStatus::UsageLimited),
            None,
            None,
        )
        .await
        .unwrap();
        ProviderRecoveryRequest {
            node_did: owner,
            session_id: session_id.into(),
            request_id: request_id.into(),
        }
    }
}

#[tokio::test]
async fn native_provider_recovery_plans_moves_opts_in_and_resumes_once() {
    let fixture = Fixture::new().await;
    let no_reset = fixture
        .seed_blocked_session("session-no-reset", "stopped-no-reset", false)
        .await;
    let with_reset = fixture
        .seed_blocked_session("session-reset", "stopped-reset", true)
        .await;

    // Reset opt-in is goal-scoped and stale/ineligible actions do not write.
    let no_reset_view = plan(&fixture.access, &no_reset).await.unwrap().unwrap();
    assert!(no_reset_view.resets_at.is_none());
    let no_reset_goal_id = no_reset_view.goal.unwrap().goal_id;
    let setting = |goal_id: String| ProviderRecoveryApply {
        scope: no_reset.clone(),
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
            &setting(no_reset_goal_id)
        )
        .await
        .unwrap(),
        ProviderRecoveryResult::Applied { .. }
    ));
    assert!(
        plan(&fixture.access, &no_reset)
            .await
            .unwrap()
            .unwrap()
            .goal
            .unwrap()
            .auto_resume_at_reset
    );
    assert!(matches!(
        apply(
            &fixture.access,
            fixture.identity.as_ref(),
            &ProviderRecoveryApply {
                scope: no_reset.clone(),
                action: ProviderRecoveryAction::MoveProfile {
                    profile_id: "profile-a".into(),
                    backend_id: "disabled".into(),
                    with_compaction: false,
                },
            },
        )
        .await
        .unwrap(),
        ProviderRecoveryResult::Refused { .. }
    ));
    let mut wrong_session = no_reset.clone();
    wrong_session.session_id = "other-session".into();
    assert!(plan(&fixture.access, &wrong_session).await.is_err());

    // Plan excludes disabled or wrong-model accounts, redacts secrets, and moves one profile.
    let view = plan(&fixture.access, &with_reset).await.unwrap().unwrap();
    assert_eq!(view.reason, "usage_limit");
    assert_eq!(view.affected_agents.len(), 2);
    assert!(view.resets_at.is_some());
    assert_eq!(view.candidates.len(), 1);
    assert_eq!(view.candidates[0].label, "Backup");
    let reset_goal = view.goal.as_ref().unwrap();
    assert!(!reset_goal.auto_resume_at_reset);
    assert!(!serde_json::to_string(&view)
        .unwrap()
        .contains("TOKEN-SECRET"));
    let receipt = apply(
        &fixture.access,
        fixture.identity.as_ref(),
        &ProviderRecoveryApply {
            scope: with_reset.clone(),
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
        2,
        "moving a profile does not retry the turn"
    );

    // Rebind back to `main` and verify ResumeGoal publishes one continuation on `backup`.
    fixture.bind_profile("main").await;
    let resume = ProviderRecoveryApply {
        scope: with_reset,
        action: ProviderRecoveryAction::ResumeGoal {
            goal_id: reset_goal.goal_id.clone(),
            backend_id: "backup".into(),
            with_compaction: false,
        },
    };
    let receipt = apply(&fixture.access, fixture.identity.as_ref(), &resume)
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
    let _ = apply(&fixture.access, fixture.identity.as_ref(), &resume)
        .await
        .unwrap();
    let rows = fixture
        .access
        .execute("{ AgentRequest { request_id } }")
        .await
        .unwrap();
    assert_eq!(rows["data"]["AgentRequest"].as_array().unwrap().len(), 3);
    fixture.runtime.shutdown().await;
}
