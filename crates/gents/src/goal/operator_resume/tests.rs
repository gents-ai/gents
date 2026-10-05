use super::support::*;
use super::*;
use crate::config_client::{read_desired_state_record_in_txn, ConfigAccess};
use crate::Collection;
use gents_loop::provider_limit::{persisted_failure_reason, ProviderLimitHeaders};
use serde_json::json;

#[derive(serde::Deserialize)]
struct GeneratedResumeOnCase {
    name: String,
    before: serde_json::Value,
    target_available: bool,
    expected: serde_json::Value,
    outcome: String,
}

#[derive(serde::Deserialize)]
struct GeneratedResumeOnContracts {
    goal_operator_resume_on_cases: Vec<GeneratedResumeOnCase>,
}

/// A `usage_limited` Goal whose latest request (`PARENT`) failed on a limited
/// call on Claude account A; the behavior's profile is on A, and B is
/// another enabled Claude account. `call_failure` is the failed call's text.
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
        self.f.compacts_on(&self.access, &self.a).await;
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
    crate::oauth_credential::set_account_enabled(
        &limited.access,
        limited.f.identity.did(),
        &limited.b_credential,
        false,
    )
    .await
    .unwrap();
    let retry = limited.resume_on(&limited.b).await.unwrap();
    assert!(retry.switch.is_none(), "no second switch write");
    assert!(!retry.resume.created);
    assert_eq!(retry.resume.request_id, first.resume.request_id);
    assert_eq!(limited.profile_backend().await, limited.b);
    assert_eq!(limited.children().await, [first.resume.request_id]);
}

#[tokio::test]
async fn completed_resume_retry_does_not_switch_a_profile_moved_again() {
    let limited = Limited::new(&usage_limit()).await;
    let first = limited.resume_on(&limited.b).await.unwrap();
    crate::config_client::switch_profile_account(
        &limited.access,
        limited.f.identity.did(),
        PROFILE,
        &limited.a,
        false,
        &[],
    )
    .await
    .unwrap();

    let retry = limited.resume_on(&limited.b).await.unwrap();
    assert!(
        retry.switch.is_none(),
        "a canonical receipt needs no new switch"
    );
    assert!(!retry.resume.created);
    assert_eq!(retry.resume.request_id, first.resume.request_id);
    assert_eq!(limited.profile_backend().await, limited.a);
    assert_eq!(limited.children().await, [first.resume.request_id]);
}

#[tokio::test]
async fn generated_resume_on_cases_drive_account_preflight_and_receipt_recovery() {
    let contracts: GeneratedResumeOnContracts =
        gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_operator_resume_on_cases.len(), 2);
    for case in contracts.goal_operator_resume_on_cases {
        assert!(!case.target_available);
        match case.name.as_str() {
            "unavailable_target_keeps_goal_and_publishes_nothing" => {
                assert!(case.before["children"].as_array().unwrap().is_empty());
                assert!(case.expected["children"].as_array().unwrap().is_empty());
                assert_eq!(case.outcome, "unavailable");
                let limited = Limited::new(&usage_limit()).await;
                crate::config_client::switch_profile_account(
                    &limited.access,
                    limited.f.identity.did(),
                    PROFILE,
                    &limited.b,
                    false,
                    &[],
                )
                .await
                .unwrap();
                crate::oauth_credential::set_account_enabled(
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
                assert_eq!(case.before["children"].as_array().unwrap().len(), 1);
                assert_eq!(case.expected, case.before);
                assert_eq!(case.outcome, "recovered");
                let limited = Limited::new(&usage_limit()).await;
                let first = limited.resume_on(&limited.b).await.unwrap();
                crate::oauth_credential::set_account_enabled(
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
async fn moved_profile_retry_rejects_a_disabled_target_account() {
    let limited = Limited::new(&usage_limit()).await;
    crate::config_client::switch_profile_account(
        &limited.access,
        limited.f.identity.did(),
        PROFILE,
        &limited.b,
        false,
        &[],
    )
    .await
    .unwrap();
    crate::oauth_credential::set_account_enabled(
        &limited.access,
        limited.f.identity.did(),
        &limited.b_credential,
        false,
    )
    .await
    .unwrap();

    let error = limited.resume_on(&limited.b).await.unwrap_err().to_string();
    assert!(error.contains("target account"), "{error}");
    assert_eq!(limited.profile_backend().await, limited.b);
    assert_eq!(limited.goal_status().await, "usage_limited");
    assert!(limited.children().await.is_empty());
}

#[tokio::test]
async fn moved_profile_retry_rejects_a_removed_target_account() {
    let limited = Limited::new(&usage_limit()).await;
    crate::config_client::switch_profile_account(
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
        .transact("test.resume_on.remove_retry_target", move |txn| {
            let did = did.clone();
            let credential = credential.clone();
            Box::pin(async move {
                crate::oauth_credential::remove_account_in_txn(txn, &did, &credential).await
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
async fn resume_on_counts_the_plugin_slots_of_the_moved_profile() {
    let limited = Limited::new(&usage_limit()).await;
    let asked = std::cell::RefCell::new(Vec::new());
    let receipt = resume_goal_on_account(
        &limited.access,
        limited.f.identity.as_ref(),
        limited.f.identity.did(),
        SESSION,
        PARENT,
        &limited.b,
        false,
        &|profile| {
            asked.borrow_mut().push(profile.to_owned());
            Ok(vec!["team/ocr".into()])
        },
    )
    .await
    .unwrap();
    assert_eq!(asked.into_inner(), [PROFILE]);
    let switch = receipt.switch.expect("the profile moved");
    assert_eq!(
        switch.headline,
        format!("Move profile {PROFILE} to label-b (used by 1 behavior and 1 plugin slot)")
    );
    assert_eq!(switch.plugin_slots, ["team/ocr"]);
}

#[tokio::test]
async fn resume_on_checks_the_session_before_the_switch() {
    let limited = Limited::new(&usage_limit()).await;
    let error = resume_goal_on_account(
        &limited.access,
        limited.f.identity.as_ref(),
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
    assert!(error.contains("gents goal resume-request"), "{error}");
    assert_eq!(limited.profile_backend().await, limited.a);
    assert_eq!(limited.goal_status().await, "usage_limited");
    assert!(limited.children().await.is_empty());
}
