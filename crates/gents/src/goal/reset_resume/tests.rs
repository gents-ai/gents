use super::contract_tests::{at, usage_limit};
use super::support::*;
use super::*;
use crate::lifecycle::queue::goal_continuation_identity;
use gents_loop::provider_limit::{classify_provider_limit, ProviderLimit};
use serde_json::json;

/// The reported reset, in seconds after 2030-01-01T00:00:00Z.
const T: i64 = 3600;

/// An opted-in Goal stopped by `PARENT`, whose call of `kind` on Claude
/// account A started at 0 and reported a reset; B is another enabled account.
struct Reset {
    f: Fixture,
    accounts: ClaudeAccounts,
}

impl Reset {
    async fn new(kind: &str, reset_at: Option<i64>) -> Self {
        let f = Fixture::new(&json!({
            "status": "usage_limited", "sequence": 0, "blocked_audits": 0,
            "wrapup_requested": false, "wrapup_completed": false,
            "tokens_used": 0, "token_budget": 1000, "last_continued_from": null
        }))
        .await;
        let accounts = f.claude_accounts().await;
        if kind == "compaction" {
            f.compacts_on(&accounts.access, &accounts.a).await;
        }
        let reset = Self { f, accounts };
        reset.limit(PARENT, kind, 0, reset_at).await;
        reset.opt_in(true).await;
        reset
    }

    async fn opt_in(&self, on: bool) {
        set_goal_from_access(
            &self.accounts.access,
            self.f.identity.did(),
            SESSION,
            None,
            None,
            None,
            Some(on),
        )
        .await
        .unwrap();
    }

    /// `request_id` failed on a call of `kind` started at `started` that
    /// reported `reset_at`, and the Goal is usage-limited on it, as the goal
    /// source leaves it.
    async fn limit(&self, request_id: &str, kind: &str, started: i64, reset_at: Option<i64>) {
        let failure = usage_limit(reset_at);
        self.f
            .fail_with_call(
                request_id,
                kind,
                &self.accounts.a,
                &failure,
                &at(started).to_rfc3339(),
            )
            .await;
        let Some(ProviderLimit::UsageExhausted(limit)) =
            classify_provider_limit(&failure, Utc::now())
        else {
            panic!("not a usage limit: {failure}");
        };
        execute(
            &self.f.node,
            &format!(
                r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ status: "usage_limited", last_failure: "{}" }}) {{ _docID }} }}"#,
                escape_graphql_string(&self.f.goal.doc_id),
                escape_graphql_string(&limit.to_string())
            ),
        )
        .await;
    }

    async fn goal(&self) -> GoalDocument {
        load_canonical_goal(&self.f.node, self.f.identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap()
    }

    /// A resume at `now` that reads the Goal afresh, as after a restart.
    async fn resume(&self, now: i64) -> Option<GoalResumeReceipt> {
        self.resume_with(&self.goal().await, now).await
    }

    async fn resume_with(&self, goal: &GoalDocument, now: i64) -> Option<GoalResumeReceipt> {
        resume_at_reset(&self.f.node, self.f.identity.as_ref(), goal, at(now))
            .await
            .unwrap()
    }

    /// Nothing resumed, even long after the reset.
    async fn waits(&self) {
        assert!(self.resume(T).await.is_none());
        assert!(self.resume(10 * T).await.is_none());
        assert_eq!(self.f.goal_status().await, "usage_limited");
        assert!(self.f.children().await.is_empty());
    }
}

#[tokio::test]
async fn resumes_once_at_the_reset() {
    let reset = Reset::new("inference", Some(T)).await;
    // One rescan's Goal row, held across the calls.
    let goal = reset.goal().await;
    assert!(reset.resume_with(&goal, T - 1).await.is_none());
    assert!(reset.f.children().await.is_empty());
    let receipt = reset
        .resume_with(&goal, T)
        .await
        .expect("resumed at the reset");
    assert!(receipt.created);
    assert_eq!(receipt.goal_status, GoalStatus::Active);
    assert_eq!(reset.f.goal_status().await, "active");
    assert_eq!(
        reset.f.children().await,
        std::slice::from_ref(&receipt.request_id)
    );
    assert!(reset.resume_with(&goal, T + 1).await.is_none());
    assert_eq!(reset.f.children().await, [receipt.request_id]);
}

#[tokio::test]
async fn resumes_after_a_compaction_limit() {
    let reset = Reset::new("compaction", Some(T)).await;
    assert!(reset.resume(T - 1).await.is_none());
    let receipt = reset.resume(T).await.expect("resumed at the reset");
    assert!(receipt.created);
    assert_eq!(reset.f.children().await, [receipt.request_id]);
}

#[tokio::test]
async fn waits_without_opt_in() {
    let reset = Reset::new("inference", Some(T)).await;
    reset.opt_in(false).await;
    reset.waits().await;
}

#[tokio::test]
async fn waits_without_a_reported_reset() {
    Reset::new("inference", None).await.waits().await;
}

#[tokio::test]
async fn waits_after_the_profile_moved() {
    let reset = Reset::new("inference", Some(T)).await;
    let accounts = &reset.accounts;
    crate::config_client::switch_profile_account(
        &accounts.access,
        reset.f.identity.did(),
        PROFILE,
        &accounts.b,
        false,
        &[],
    )
    .await
    .unwrap();
    reset.waits().await;
}

#[tokio::test]
async fn waits_after_the_account_was_disabled() {
    let reset = Reset::new("inference", Some(T)).await;
    crate::oauth_credential::set_account_enabled(
        &reset.accounts.access,
        reset.f.identity.did(),
        &reset.accounts.a_credential,
        false,
    )
    .await
    .unwrap();
    reset.waits().await;
}

#[tokio::test]
async fn waits_after_the_account_was_removed() {
    let reset = Reset::new("inference", Some(T)).await;
    let (did, credential) = (reset.f.identity.did(), reset.accounts.a_credential.as_str());
    reset
        .accounts
        .access
        .transact("test.remove_account", |txn| {
            Box::pin(crate::oauth_credential::remove_account_in_txn(
                txn, did, credential,
            ))
        })
        .await
        .unwrap();
    reset.waits().await;
}

#[tokio::test]
async fn restart_before_and_after_the_reset() {
    let reset = Reset::new("inference", Some(T)).await;
    assert!(reset.resume(T - 60).await.is_none());
    let receipt = reset.resume(T + 60).await.expect("resumed after the reset");
    assert!(receipt.created);
    assert!(reset.resume(T + 120).await.is_none());
    assert_eq!(reset.f.children().await, [receipt.request_id]);
}

#[tokio::test]
async fn repeat_limit_schedules_at_its_new_reset() {
    let reset = Reset::new("inference", Some(T)).await;
    let first = reset.resume(T).await.expect("first resume");
    // The child stops on a new limit with a later reset.
    reset
        .limit(&first.request_id, "inference", T, Some(3 * T))
        .await;
    assert!(reset.resume(3 * T - 1).await.is_none());
    let second = reset.resume(3 * T).await.expect("resumed at the new reset");
    let goal_id = &reset.f.goal.goal_id;
    assert_eq!(
        second.request_id,
        goal_continuation_identity(goal_id, &first.request_id, 2)
            .unwrap()
            .request_id
    );
    // That child stops again reporting the same reset: no resume.
    reset
        .limit(&second.request_id, "inference", 3 * T, Some(3 * T))
        .await;
    assert!(reset.resume(10 * T).await.is_none());
    assert_eq!(reset.f.goal_status().await, "usage_limited");
    assert_eq!(
        reset.f.children().await,
        [second.request_id, first.request_id]
    );
}
