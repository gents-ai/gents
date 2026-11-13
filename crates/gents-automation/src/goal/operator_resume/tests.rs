use super::support::*;
use super::*;
use gents_documents::read_desired_state_record_in_txn;
use gents_loop::provider_limit::{persisted_failure_reason, ProviderLimitHeaders};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use serde_json::json;

struct Limited {
    f: Fixture,
    access: ConfigAccess,
    a: String,
    b: String,
    b_credential: String,
}

impl Limited {
    async fn new(call_failure: &str) -> Self {
        let f = Fixture::new(&json!({
            "status": "usage_limited", "sequence": 0, "blocked_audits": 0,
            "wrapup_requested": false, "wrapup_completed": false,
            "tokens_used": 0, "token_budget": 1000, "last_continued_from": null
        }))
        .await;
        let accounts = f.claude_accounts().await;
        f.fail_with_call(
            PARENT,
            "inference",
            &accounts.a,
            call_failure,
            &Utc::now().to_rfc3339(),
        )
        .await;
        Self {
            f,
            access: accounts.access,
            a: accounts.a,
            b: accounts.b,
            b_credential: accounts.b_credential,
        }
    }

    async fn resume_on(&self, target: &str) -> Result<GoalResumeOnReceipt> {
        resume_goal_on_account(
            &self.access,
            self.f.user.as_ref(),
            self.f.identity.did(),
            SESSION,
            PARENT,
            target,
            false,
            &|_| Ok(Vec::new()),
        )
        .await
    }

    fn backend(&self, label: &str) -> &str {
        match label {
            "a" => &self.a,
            "b" => &self.b,
            other => panic!("unknown account label {other:?}"),
        }
    }

    fn label(&self, backend: &str) -> String {
        if backend == self.a {
            "a".to_owned()
        } else if backend == self.b {
            "b".to_owned()
        } else {
            panic!("backend {backend:?} is neither account")
        }
    }

    async fn profile_backend(&self) -> String {
        self.backend_of(PROFILE).await
    }

    async fn backend_of(&self, profile: &str) -> String {
        let did = self.f.identity.did();
        let (_, value) = self
            .access
            .transact_write("test.resume_on.read", |txn| {
                Box::pin(async move {
                    read_desired_state_record_in_txn(
                        txn,
                        Collection::InferenceProfile,
                        did,
                        profile,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        value["backend_id"].as_str().unwrap().to_owned()
    }

    async fn goal_status(&self) -> String {
        self.f.goal_status().await
    }

    async fn children(&self) -> Vec<String> {
        self.f.children().await
    }
}

fn usage_limit() -> String {
    let body = r#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed your account's rate limit. Please try again later."}}"#;
    let annotated = ProviderLimitHeaders::from_headers(
        [
            ("anthropic-ratelimit-unified-status", "rejected"),
            ("anthropic-ratelimit-unified-reset", "1790354400"),
        ],
        Utc::now(),
    )
    .annotate(body);
    persisted_failure_reason(
        &format!("Claude Messages HTTP 429 Too Many Requests body={annotated}"),
        Utc::now(),
    )
}

#[tokio::test]
async fn resume_on_moves_the_profile_and_publishes_one_child() {
    let limited = Limited::new(&usage_limit()).await;
    let receipt = limited.resume_on(&limited.b).await.unwrap();
    let switch = receipt.switch.expect("the profile moved");
    assert_eq!(
        switch.headline,
        format!("Move profile {PROFILE} to label-b (used by 1 agent)")
    );
    assert!(receipt.resume.created);
    assert_eq!(receipt.resume.goal_status, GoalStatus::Active);
    assert_eq!(limited.profile_backend().await, limited.b);
    assert_eq!(limited.goal_status().await, "active");
    assert_eq!(limited.children().await, [receipt.resume.request_id]);
}

#[tokio::test]
async fn resume_on_retry_matches_the_lean_profile_model() {
    for case in gents_lean_vocab::families::goal_operator_resume_profiles_resume_on_retry::cases()
    {
        let limited = Limited::new(&usage_limit()).await;
        if let Some(label) = &case.input.compaction {
            limited
                .f
                .compacts_on(&limited.access, limited.backend(label))
                .await;
        }
        let first = limited.resume_on(&limited.b).await.unwrap();
        let first_profile = limited.label(&limited.profile_backend().await);
        if let Some(label) = &case.input.operator_move_to {
            gents_inference::profile_switch::switch_profile_account(
                &limited.access,
                limited.f.identity.did(),
                PROFILE,
                limited.backend(label),
                false,
                &[],
            )
            .await
            .unwrap();
        }
        let retry = limited.resume_on(&limited.b).await.unwrap();
        let children = limited.children().await;
        let profile = limited.label(&limited.profile_backend().await);
        let compaction = if case.input.compaction.is_some() {
            Some(limited.label(&limited.backend_of("summ").await))
        } else {
            None
        };
        case.check(
            gents_lean_vocab::families::goal_operator_resume_profiles_resume_on_retry::RetryOutput {
                first_profile,
                retry_switched: retry.switch.is_some(),
                retry_created: retry.resume.created,
                same_request: retry.resume.request_id == first.resume.request_id,
                child_is_first: children == [first.resume.request_id.clone()],
                profile,
                compaction,
            },
        );
    }
}

#[tokio::test]
async fn generated_resume_on_cases_drive_account_preflight_and_receipt_recovery() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().goal_operator_resume_on_cases;
    assert_eq!(cases.len(), 2);
    for case in cases {
        assert!(!case.target_available);
        match case.name.as_str() {
            "unavailable_target_keeps_goal_and_publishes_nothing" => {
                assert!(case.before.children.is_empty());
                assert!(case.expected.children.is_empty());
                assert_eq!(case.outcome, "unavailable");
                let limited = Limited::new(&usage_limit()).await;
                gents_inference::profile_switch::switch_profile_account(
                    &limited.access,
                    limited.f.identity.did(),
                    PROFILE,
                    &limited.b,
                    false,
                    &[],
                )
                .await
                .unwrap();
                gents_inference::auth::credential::set_account_enabled(
                    &limited.access,
                    limited.f.identity.did(),
                    &limited.b_credential,
                    false,
                )
                .await
                .unwrap();
                let error = limited.resume_on(&limited.b).await.unwrap_err();
                assert!(format!("{error:#}").contains("target account"));
                assert!(limited.children().await.is_empty());
                assert_eq!(limited.goal_status().await, "usage_limited");
            }
            "existing_child_is_recovered_after_target_disabled" => {
                assert_eq!(case.before.children.len(), 1);
                assert_eq!(case.expected.status, case.before.status);
                assert_eq!(case.expected.blocked_audits, case.before.blocked_audits);
                assert_eq!(case.expected.wrapup_requested, case.before.wrapup_requested);
                assert_eq!(case.expected.wrapup_completed, case.before.wrapup_completed);
                assert_eq!(case.expected.sequence, case.before.sequence);
                assert_eq!(
                    case.expected.last_continued_from,
                    case.before.last_continued_from
                );
                assert_eq!(case.expected.latest_request, case.before.latest_request);
                assert_eq!(case.expected.tokens_used, case.before.tokens_used);
                assert_eq!(case.expected.token_budget, case.before.token_budget);
                assert_eq!(case.expected.children.len(), case.before.children.len());
                let expected_children = &case.expected.children;
                let before_children = &case.before.children;
                for (expected, before) in expected_children.iter().zip(before_children) {
                    assert_eq!(expected.goal, before.goal);
                    assert_eq!(expected.owner, before.owner);
                    assert_eq!(expected.session, before.session);
                    assert_eq!(expected.predecessor, before.predecessor);
                    assert_eq!(expected.predecessor_doc, before.predecessor_doc);
                    assert_eq!(expected.correlation, before.correlation);
                    assert_eq!(expected.source_document, before.source_document);
                    assert_eq!(expected.trigger_context, before.trigger_context);
                    assert_eq!(expected.workspace_fingerprint, before.workspace_fingerprint);
                    assert_eq!(expected.semantic_fingerprint, before.semantic_fingerprint);
                    assert_eq!(expected.child, before.child);
                    assert_eq!(expected.sequence, before.sequence);
                }
                assert_eq!(case.outcome, "recovered");
                let limited = Limited::new(&usage_limit()).await;
                let first = limited.resume_on(&limited.b).await.unwrap();
                gents_inference::auth::credential::set_account_enabled(
                    &limited.access,
                    limited.f.identity.did(),
                    &limited.b_credential,
                    false,
                )
                .await
                .unwrap();
                let retry = limited.resume_on(&limited.b).await.unwrap();
                assert!(!retry.resume.created);
                assert_eq!(retry.resume.request_id, first.resume.request_id);
                assert_eq!(limited.children().await, [first.resume.request_id]);
            }
            name => panic!("unmapped generated resume-on case {name}"),
        }
    }
}

#[tokio::test]
async fn moved_profile_retry_rejects_a_removed_target_account() {
    let limited = Limited::new(&usage_limit()).await;
    gents_inference::profile_switch::switch_profile_account(
        &limited.access,
        limited.f.identity.did(),
        PROFILE,
        &limited.b,
        false,
        &[],
    )
    .await
    .unwrap();
    let did = limited.f.identity.did().to_owned();
    let credential = limited.b_credential.clone();
    limited
        .access
        .transact_write("test.resume_on.remove_retry_target", move |txn| {
            let did = did.clone();
            let credential = credential.clone();
            Box::pin(async move {
                gents_inference::auth::credential::remove_account_in_txn(txn, &did, &credential)
                    .await
            })
        })
        .await
        .unwrap();

    let error = limited.resume_on(&limited.b).await.unwrap_err().to_string();
    assert!(error.contains("target account"), "{error}");
    assert_eq!(limited.profile_backend().await, limited.b);
    assert_eq!(limited.goal_status().await, "usage_limited");
    assert!(limited.children().await.is_empty());
}

#[tokio::test]
async fn resume_on_names_the_failed_step() {
    let limited = Limited::new(&usage_limit()).await;
    let error = limited.resume_on(&limited.a).await.unwrap_err().to_string();
    assert!(
        error.starts_with("switch failed; nothing changed"),
        "{error}"
    );
    assert_eq!(limited.goal_status().await, "usage_limited");
    assert_eq!(limited.profile_backend().await, limited.a);
    assert!(limited.children().await.is_empty());

    limited
        .f
        .other_request("older-active", "2019-01-01T00:00:00Z", "processing")
        .await;
    let error = limited.resume_on(&limited.b).await.unwrap_err().to_string();
    assert!(
        error.starts_with(&format!(
            "resume failed after the switch committed (profile {PROFILE} is now on label-b); \
             run the same command again with the same --from"
        )),
        "{error}"
    );
    assert_eq!(limited.profile_backend().await, limited.b);
    assert_eq!(limited.goal_status().await, "usage_limited");
    assert!(limited.children().await.is_empty());
}

#[tokio::test]
async fn resume_on_counts_the_plugin_slots_of_the_moved_profile() {
    let limited = Limited::new(&usage_limit()).await;
    let asked = std::sync::Mutex::new(Vec::new());
    let receipt = resume_goal_on_account(
        &limited.access,
        limited.f.user.as_ref(),
        limited.f.identity.did(),
        SESSION,
        PARENT,
        &limited.b,
        false,
        &|profile| {
            asked.lock().unwrap().push(profile.to_owned());
            Ok(vec!["team/ocr".into()])
        },
    )
    .await
    .unwrap();
    assert_eq!(asked.into_inner().unwrap(), [PROFILE]);
    let switch = receipt.switch.expect("the profile moved");
    assert_eq!(
        switch.headline,
        format!("Move profile {PROFILE} to label-b (used by 1 agent and 1 plugin slot)")
    );
    assert_eq!(switch.plugin_slots, ["team/ocr"]);
}

#[tokio::test]
async fn resume_on_checks_the_session_before_the_switch() {
    let limited = Limited::new(&usage_limit()).await;
    let error = resume_goal_on_account(
        &limited.access,
        limited.f.user.as_ref(),
        limited.f.identity.did(),
        "other-session",
        PARENT,
        &limited.b,
        false,
        &|_| Ok(Vec::new()),
    )
    .await
    .unwrap_err()
    .to_string();
    assert_eq!(
        error,
        "resume predecessor must uniquely belong to the goal owner and session"
    );
    assert_eq!(limited.profile_backend().await, limited.a, "nothing moved");
}

#[tokio::test]
async fn resume_on_covers_limits_only() {
    let limited = Limited::new(r#"Claude account "label-a" is disabled."#).await;
    let error = limited.resume_on(&limited.b).await.unwrap_err().to_string();
    assert!(
        error.contains("gents config profile set-account"),
        "{error}"
    );
    assert!(
        error.contains("gents automation goal resume-request"),
        "{error}"
    );
    assert_eq!(limited.profile_backend().await, limited.a);
    assert_eq!(limited.goal_status().await, "usage_limited");
    assert!(limited.children().await.is_empty());
}

#[tokio::test]
async fn client_resume_recovers_pending_relationships_without_replaying_revocations() {
    let fixture = Fixture::new(&json!({
        "status": "paused", "sequence": 0, "blocked_audits": 0,
        "wrapup_requested": false, "wrapup_completed": false,
        "tokens_used": 0, "token_budget": 1000, "last_continued_from": null
    }))
    .await;
    let user = fixture.user_access();
    let staged = user
        .transact_write("test.goal.resume_before_grants", |txn| {
            Box::pin(stage_resume_inner(
                txn,
                fixture.user.as_ref(),
                fixture.identity.did(),
                SESSION,
                PARENT,
                None,
                false,
            ))
        })
        .await
        .unwrap();
    user.transact_write("test.goal.retained_pending_resume", |txn| {
        let staged = &staged;
        let agent = &fixture.agent;
        let agent_doc_id = &fixture.agent_doc_id;
        let session_doc_id = &fixture.session_doc_id;
        Box::pin(async move {
            let doc = escape_graphql_string(&staged.doc_id);
            let marked = txn.execute_client_signed_update(
                &format!(r#"mutation($input: AgentRequestMutationInputArg!) {{ update_AgentRequest(filter: {{_docID: {{_eq: "{doc}"}}}}, input: $input) {{_docID}} }}"#),
                &json!({"input": {"relationships_pending": true}}),
                gents_protocol::schemas::AGENT_REQUEST,
            ).await?;
            anyhow::ensure!(gents_store::graphql::response_has_documents(&marked["data"]["update_AgentRequest"]), "historical request marker was not staged");
            txn.delete_actor_relationship("AgentRequest", &staged.doc_id, "working_agent", &::identity::Did::new(agent.did())?).await?;
            txn.delete_resource_relationship("AgentRequest", &staged.doc_id, "parent", "agents", agent_doc_id).await?;
            txn.delete_resource_relationship("AgentRequest", &staged.doc_id, "session", "sessions", session_doc_id).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID relationships_pending admission_kind admission_signer_did }} }}"#,
        escape_graphql_string(&staged.doc_id)
    );
    let pending = user.execute(&query).await.unwrap();
    let pending_failed = pending["data"]["AgentRequest"][0]["relationships_pending"]
        .as_bool()
        .expect("staged request records its relationship publication");
    assert_eq!(
        pending["data"]["AgentRequest"][0]["admission_kind"],
        "client"
    );
    assert_eq!(
        pending["data"]["AgentRequest"][0]["admission_signer_did"],
        fixture.user.did()
    );
    assert!(
        fixture.agent_access().execute(&query).await.unwrap()["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let recovered = resume_goal_request(
        &user,
        fixture.user.as_ref(),
        fixture.identity.did(),
        SESSION,
        PARENT,
    )
    .await
    .unwrap();
    assert!(!recovered.created);
    assert_eq!(recovered.doc_id, staged.doc_id);
    let published = fixture.agent_access().execute(&query).await.unwrap();
    assert_eq!(
        published["data"]["AgentRequest"].as_array().unwrap().len(),
        1
    );
    let pending_completed = published["data"]["AgentRequest"][0]["relationships_pending"]
        .as_bool()
        .expect("recovered request records its relationship publication");

    user.delete_actor_relationship(
        "AgentRequest",
        &staged.doc_id,
        "working_agent",
        &::identity::Did::new(fixture.agent.did()).unwrap(),
    )
    .await
    .unwrap();
    user.delete_resource_relationship(
        "AgentRequest",
        &staged.doc_id,
        "session",
        "sessions",
        &fixture.session_doc_id,
    )
    .await
    .unwrap();
    assert!(
        fixture.agent_access().execute(&query).await.unwrap()["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let again = resume_goal_request(
        &user,
        fixture.user.as_ref(),
        fixture.identity.did(),
        SESSION,
        PARENT,
    )
    .await
    .unwrap();
    assert!(!again.created);
    assert_eq!(again.doc_id, staged.doc_id);
    let completed_no_replay = user.execute(&query).await.unwrap()["data"]["AgentRequest"][0]
        ["relationships_pending"]
        .as_bool()
        .expect("revoked request records its relationship publication");
    assert!(
        fixture.agent_access().execute(&query).await.unwrap()["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for case in gents_lean_vocab::families::goal_operator_resume_contracts_publication::cases() {
        let pending = match case.input.name.as_str() {
            "pending-failed" => pending_failed,
            "pending-completed" => pending_completed,
            "completed-no-replay" => completed_no_replay,
            other => panic!("the client resume scenario has no step {other}"),
        };
        case.check(
            gents_lean_vocab::families::goal_operator_resume_contracts_publication::PublicationOutput {
                pending,
            },
        );
    }
    fixture.node.shutdown().await.unwrap();
}
