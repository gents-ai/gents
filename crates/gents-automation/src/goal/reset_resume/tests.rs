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
        seed_goal_from_access(
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
        gents_inference::profile_switch::switch_profile_account(
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
        match resume_at_reset(
            &self.f.node,
            self.f.identity.as_ref(),
            self.f.agent.as_ref(),
            goal,
            at(now),
        )
        .await
        .unwrap()
        {
            ResetResume::Resumed(receipt) => Some(receipt),
            ResetResume::NotUsageLimited | ResetResume::Deferred => None,
        }
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
async fn reset_account_facts_drive_resume_at_reset() {
    for case in gents_lean_vocab::families::goal_reset_account_trace::cases::<String, GoalStatus>()
    {
        let input = &case.input;
        let reset = Reset::new(&input.kind, Some(T)).await;
        if input.kind != "compaction" {
            reset
                .f
                .compacts_on(&reset.accounts.access, &reset.accounts.a)
                .await;
        }
        connect_accounts_at(&reset.f, input.connected_at).await;
        if !input.inference_names_account {
            reset.move_to_b(PROFILE).await;
        }
        if !input.compaction_names_account {
            reset.move_to_b("summ").await;
        }
        let mut steps = Vec::new();
        for now in &input.resume_at {
            let receipt = reset.resume(i64::try_from(*now).unwrap()).await;
            let children = reset.f.children().await;
            if let Some(receipt) = &receipt {
                assert!(children.contains(&receipt.request_id), "{input:?}");
            }
            steps.push(gents_lean_vocab::families::goal_reset_account_trace::Step {
                resumed: receipt.is_some_and(|receipt| receipt.created),
                status: reset.goal().await.parsed_status().unwrap(),
                continuations: children.len() as u64,
            });
        }
        case.check(steps);
        reset.f.node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn account_removal_follows_the_lean_reset_model() {
    for case in gents_lean_vocab::families::goal_reset_resume_account_resumes::cases() {
        let reset = Reset::new("inference", Some(T)).await;
        if !case.input.account_present {
            let admin = ConfigAccess::local_as(
                reset.f.node.clone(),
                ::identity::Did::new(reset.f.identity.did()).unwrap(),
            );
            admin
                .grant_node_permission(
                    &::identity::Did::new(reset.f.user.did()).unwrap(),
                    gents_store::config_client::NodePermission::DocumentDelete,
                )
                .await
                .unwrap();
            let (did, credential) = (reset.f.identity.did(), reset.accounts.a_credential.as_str());
            reset
                .accounts
                .access
                .transact_write("test.remove_account", |txn| {
                    Box::pin(gents_inference::auth::credential::remove_account_in_txn(
                        txn, did, credential,
                    ))
                })
                .await
                .unwrap();
        }
        let result = reset.resume(case.input.now as i64).await;
        case.check(
            gents_lean_vocab::families::goal_reset_resume_account_resumes::ResumeOutput {
                created: result.as_ref().is_some_and(|receipt| receipt.created),
                goal_usage_limited: reset.f.goal_status().await == "usage_limited",
                child_count: reset.f.children().await.len() as u64,
            },
        );
    }
}
