use super::contract_tests::{at, connect_accounts_at, usage_limit};
use super::support::*;
use super::*;
use gents_loop::provider_limit::{classify_provider_limit, ProviderLimit};
use serde_json::json;

const T: i64 = 3600;

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
        connect_accounts_at(&f, -1).await;
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
            &self.f.agent_access(),
            &format!(
                r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ status: "usage_limited", last_failure: "{}" }}) {{ _docID }} }}"#,
                escape_graphql_string(&self.f.goal.doc_id),
                escape_graphql_string(&limit.to_string())
            ),
        )
        .await;
    }

    async fn move_to_b(&self, profile: &str) {
        crate::inference::profile_switch::switch_profile_account(
            &self.accounts.access,
            self.f.identity.did(),
            profile,
            &self.accounts.b,
            false,
            &[],
        )
        .await
        .unwrap();
    }

    async fn goal(&self) -> GoalDocument {
        self.f.current_goal().await
    }

    async fn resume(&self, now: i64) -> Option<GoalResumeReceipt> {
        self.resume_with(&self.goal().await, now).await
    }

    async fn resume_with(&self, goal: &GoalDocument, now: i64) -> Option<GoalResumeReceipt> {
        resume_at_reset(
            &self.f.node,
            self.f.identity.as_ref(),
            self.f.agent.as_ref(),
            goal,
            at(now),
        )
        .await
        .unwrap()
    }

    async fn waits(&self) {
        assert!(self.resume(T).await.is_none());
        assert!(self.resume(10 * T).await.is_none());
        assert_eq!(self.f.goal_status().await, "usage_limited");
        assert!(self.f.children().await.is_empty());
    }
}

#[tokio::test]
async fn a_held_row_waits_once_the_opt_in_is_off() {
    let reset = Reset::new("inference", Some(T)).await;
    let goal = reset.goal().await;
    reset.opt_in(false).await;
    assert!(reset.resume_with(&goal, T).await.is_none());
    assert_eq!(reset.f.goal_status().await, "usage_limited");
    assert!(reset.f.children().await.is_empty());
}

#[tokio::test]
async fn a_held_row_waits_for_the_latest_reset() {
    let reset = Reset::new("inference", Some(T)).await;
    let goal = reset.goal().await;
    let first = reset.resume(T).await.expect("first resume");
    reset
        .limit(&first.request_id, "inference", T, Some(3 * T))
        .await;
    assert!(reset.resume_with(&goal, 2 * T).await.is_none());
    assert_eq!(reset.f.goal_status().await, "usage_limited");
    assert_eq!(reset.f.children().await, [first.request_id]);
}

#[tokio::test]
async fn resumes_after_a_compaction_limit() {
    let reset = Reset::new("compaction", Some(T)).await;
    reset.move_to_b(PROFILE).await;
    assert!(reset.resume(T - 1).await.is_none());
    let receipt = reset.resume(T).await.expect("resumed at the reset");
    assert!(receipt.created);
    assert_eq!(reset.f.children().await, [receipt.request_id]);
}

#[tokio::test]
async fn waits_after_the_compaction_profile_moved() {
    let reset = Reset::new("compaction", Some(T)).await;
    reset.move_to_b("summ").await;
    reset.waits().await;
}

#[tokio::test]
async fn waits_for_a_sign_in_after_the_limited_call() {
    let reset = Reset::new("inference", Some(T)).await;
    connect_accounts_at(&reset.f, 1).await;
    reset.waits().await;
}

#[tokio::test]
async fn waits_after_the_account_was_removed() {
    let reset = Reset::new("inference", Some(T)).await;
    let admin = ConfigAccess::local_as(
        reset.f.node.clone(),
        ::identity::Did::new(reset.f.identity.did()).unwrap(),
    );
    admin
        .grant_node_permission(
            &::identity::Did::new(reset.f.user.did()).unwrap(),
            crate::store::config_client::NodePermission::DocumentDelete,
        )
        .await
        .unwrap();
    let (did, credential) = (reset.f.identity.did(), reset.accounts.a_credential.as_str());
    reset
        .accounts
        .access
        .transact_write("test.remove_account", |txn| {
            Box::pin(crate::inference::auth::credential::remove_account_in_txn(
                txn, did, credential,
            ))
        })
        .await
        .unwrap();
    reset.waits().await;
}
