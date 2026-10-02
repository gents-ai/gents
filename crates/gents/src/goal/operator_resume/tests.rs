use super::support::*;
use super::*;
use crate::claude_oauth::CLAUDE_OAUTH_PROVIDER;
use crate::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, write_inference_backend_document,
    ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::oauth_credential::{preset_account_backend, store_sign_in};
use crate::Collection;
use gents_loop::provider_limit::{persisted_failure_reason, ProviderLimitHeaders};
use serde_json::json;

const PROFILE: &str = "contract-behavior:inference";

/// A `usage_limited` Goal whose latest request (`PARENT`) failed on a limited
/// call on Claude account A; the behavior's profile is on A, and B is
/// another enabled Claude account. `call_failure` is the failed call's text.
struct Limited {
    f: Fixture,
    access: ConfigAccess,
    a: String,
    b: String,
}

impl Limited {
    async fn new(call_failure: &str) -> Self {
        let f = Fixture::new(&json!({
            "status": "usage_limited", "sequence": 0, "blocked_audits": 0,
            "wrapup_requested": false, "wrapup_completed": false,
            "tokens_used": 0, "token_budget": 1000, "last_continued_from": null
        }))
        .await;
        let did = f.identity.did().to_owned();
        let access = ConfigAccess::Local(f.node.clone());
        let preset =
            preset_account_backend(&did, CLAUDE_OAUTH_PROVIDER, None, "Claude".into()).unwrap();
        write_inference_backend_document(&access, &preset)
            .await
            .unwrap();
        let mut backends = Vec::new();
        for who in ["a", "b"] {
            let credential = crate::claude_oauth::credential_from_login_tokens(
                &did,
                CLAUDE_OAUTH_PROVIDER,
                &crate::claude_oauth::ClaudeLoginTokens {
                    access_token: format!("access-SECRET-{who}"),
                    refresh_token: format!("refresh-SECRET-{who}"),
                    expires_in: Some(3600),
                    scope: None,
                    account_id: Some("IDENTITY".into()),
                    organization_uuid: Some("org-1".into()),
                    account_uuid: Some(format!("account-{who}")),
                },
                Utc::now() - chrono::Duration::hours(1),
            );
            let label = format!("label-{who}");
            let stored = store_sign_in(&access, credential, Some(&label))
                .await
                .unwrap()
                .credential;
            backends.push(match stored.account_ref.as_deref() {
                Some(account_ref) => format!("{CLAUDE_OAUTH_PROVIDER}-{account_ref}"),
                None => CLAUDE_OAUTH_PROVIDER.to_owned(),
            });
        }
        let (a, b) = (backends[0].clone(), backends[1].clone());
        let profile = json!({"agent_did": did, "profile_id": PROFILE, "backend_id": a, "model_name": "test-model"});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::InferenceProfile,
            add: profile.clone(),
            update: profile,
        }])
        .unwrap();
        access
            .transact("test.resume_on.profile", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
        let failure = escape_graphql_string(call_failure);
        let at = Utc::now().to_rfc3339();
        execute(
            &f.node,
            &format!(
                r#"mutation {{
                    update_AgentRequest(filter: {{ request_id: {{ _eq: "{PARENT}" }} }}, input: {{
                        lifecycle_state: "failed" failure_reason: "{failure}"
                    }}) {{ _docID }}
                    create_InferenceCall(input: {{
                        call_id: "call-1" request_id: "{PARENT}" call_seq: 1
                        backend_id: "{a}" behavior_id: "contract-behavior" agent_did: "{did}"
                        call_kind: "inference" attempt: 1 call_state: "failed"
                        failure_reason: "{failure}"
                        queued_at: "{at}" started_at: "{at}" ended_at: "{at}"
                    }}) {{ _docID }}
                }}"#
            ),
        )
        .await;
        Self { f, access, a, b }
    }

    async fn resume_on(&self, target: &str) -> Result<GoalResumeOnReceipt> {
        resume_goal_on_account(
            &self.access,
            self.f.identity.as_ref(),
            self.f.identity.did(),
            SESSION,
            PARENT,
            target,
            false,
            &|_| Ok(Vec::new()),
        )
        .await
    }

    /// The behavior's context compacts with `summ`, also on A.
    async fn compacts_on_a(&self) {
        let did = self.f.identity.did();
        let documents = [
            (
                Collection::InferenceProfile,
                json!({"agent_did": did, "profile_id": "summ", "backend_id": self.a, "model_name": "model-s"}),
            ),
            (
                Collection::Compaction,
                json!({"agent_did": did, "compaction_id": "compaction-c", "inference_profile_id": "summ"}),
            ),
            (
                Collection::AgentContext,
                json!({"agent_did": did, "context_id": "contract-behavior:context", "tools_id": "contract-behavior:tools", "compaction_id": "compaction-c"}),
            ),
        ];
        let plan = DesiredStateApplyPlan::new(
            documents
                .into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
        )
        .unwrap();
        self.access
            .transact("test.resume_on.compaction", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
    }

    async fn profile_backend(&self) -> String {
        self.backend_of(PROFILE).await
    }

    async fn backend_of(&self, profile: &str) -> String {
        let did = self.f.identity.did();
        let (_, value) = self
            .access
            .transact("test.resume_on.read", |txn| {
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
        load_canonical_goal(&self.f.node, self.f.identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap()
            .status
    }

    async fn children(&self) -> Vec<String> {
        request_rows(&self.f.node)
            .await
            .into_iter()
            .filter(|row| {
                row.retry_key
                    .as_deref()
                    .is_some_and(|key| key.starts_with("goal-continuation:"))
            })
            .map(|row| row.request_id)
            .collect()
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
        format!("Move profile {PROFILE} to label-b (used by 1 behavior)")
    );
    assert!(receipt.resume.created);
    assert_eq!(receipt.resume.goal_status, GoalStatus::Active);
    assert_eq!(limited.profile_backend().await, limited.b);
    assert_eq!(limited.goal_status().await, "active");
    assert_eq!(limited.children().await, [receipt.resume.request_id]);
}

#[tokio::test]
async fn resume_on_retry_returns_the_same_child() {
    let limited = Limited::new(&usage_limit()).await;
    let first = limited.resume_on(&limited.b).await.unwrap();
    let retry = limited.resume_on(&limited.b).await.unwrap();
    assert!(retry.switch.is_none(), "no second switch write");
    assert!(!retry.resume.created);
    assert_eq!(retry.resume.request_id, first.resume.request_id);
    assert_eq!(limited.profile_backend().await, limited.b);
    assert_eq!(limited.children().await, [first.resume.request_id]);
}

#[tokio::test]
async fn resume_on_retry_leaves_the_compaction_profile() {
    let limited = Limited::new(&usage_limit()).await;
    limited.compacts_on_a().await;
    let first = limited.resume_on(&limited.b).await.unwrap();
    assert_eq!(limited.profile_backend().await, limited.b);
    let retry = limited.resume_on(&limited.b).await.unwrap();
    assert!(
        retry.switch.is_none(),
        "no second switch write: {:?}",
        retry.switch
    );
    assert_eq!(retry.resume.request_id, first.resume.request_id);
    assert_eq!(limited.backend_of("summ").await, limited.a, "summ stays");
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
async fn resume_on_covers_limits_only() {
    let limited = Limited::new(r#"Claude account "label-a" is disabled."#).await;
    let error = limited.resume_on(&limited.b).await.unwrap_err().to_string();
    assert!(
        error.contains("gents config profile set-account"),
        "{error}"
    );
    assert!(error.contains("gents goal resume-request"), "{error}");
    assert_eq!(limited.profile_backend().await, limited.a);
    assert_eq!(limited.goal_status().await, "usage_limited");
    assert!(limited.children().await.is_empty());
}
