use super::support::*;
use super::*;
use crate::goal::continuation::goal_continuation_identity;
use gents_lean_vocab::families::goal_reset_resume_contracts_reset as wire;
use gents_loop::provider_limit::{
    classify_provider_limit, persisted_failure_reason, ProviderLimit, ProviderLimitHeaders,
};
use serde_json::{json, Value};

pub(super) fn at(seconds: i64) -> DateTime<Utc> {
    "2030-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + chrono::Duration::seconds(seconds)
}

pub(super) async fn connect_accounts_at(f: &Fixture, seconds: i64) {
    let did = escape_graphql_string(f.identity.did());
    let connected_at = at(seconds).to_rfc3339();
    f.user_access()
        .transact_write("test.reset.connected_accounts", |txn| {
            let did = &did;
            let connected_at = &connected_at;
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        r#"{{ OAuthCredential(filter: {{ node_did: {{ _eq: "{did}" }} }}) {{ _docID }} }}"#
                    ))
                    .await?;
                let rows = response["data"]["OAuthCredential"]
                    .as_array()
                    .context("credential query omitted rows")?;
                assert_eq!(rows.len(), 2, "the reset fixture has two signed-in accounts");
                for row in rows {
                    let doc = row["_docID"].as_str().context("credential has no document ID")?;
                    let mutation = format!(
                        r#"mutation($input: OAuthCredentialMutationInputArg!) {{ update_OAuthCredential(filter: {{ _docID: {{ _eq: "{}" }} }}, input: $input) {{ _docID }} }}"#,
                        escape_graphql_string(doc)
                    );
                    let updated = txn
                        .execute_client_signed_update(
                            &mutation,
                            &json!({"input": {"connected_at": connected_at}}),
                            gents_protocol::schemas::OAUTH_CREDENTIAL,
                        )
                        .await?;
                    anyhow::ensure!(
                        gents_protocol::graphql::extract_mutation_doc_id(&updated, "OAuthCredential")? == doc,
                        "credential update affected a different document"
                    );
                }
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
}

pub(super) async fn disable_backend(f: &Fixture, backend: &str) {
    let access = f.user_access();
    let did = f.identity.did();
    access
        .transact_write("test.disable_resume_backend", |txn| {
            Box::pin(async move {
                let (_, mut value) = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::InferenceBackend,
                    did,
                    backend,
                )
                .await?
                .expect("selected backend");
                value["enabled"] = false.into();
                let plan = gents_documents::DesiredStateApplyPlan::new(vec![
                    gents_documents::DesiredStateApplyDocument {
                        collection: gents_store::collection::Collection::InferenceBackend,
                        add: value.clone(),
                        update: value,
                    },
                ])?;
                gents_documents::apply_desired_state_plan(
                    txn,
                    &plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
}

pub(super) fn usage_limit(reset_at: Option<i64>) -> String {
    let text = match reset_at {
        Some(seconds) => {
            let reset = at(seconds).timestamp().to_string();
            let body = r#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed your account's rate limit. Please try again later."}}"#;
            let annotated = ProviderLimitHeaders::from_headers(
                [
                    ("anthropic-ratelimit-unified-status", "rejected"),
                    ("anthropic-ratelimit-unified-reset", reset.as_str()),
                ],
                Utc::now(),
            )
            .annotate(body);
            format!("Claude Messages HTTP 429 Too Many Requests body={annotated}")
        }
        None => r#"HttpError: Invalid status code 429 Too Many Requests with message: {"error":{"type":"usage_limit_reached","message":"The usage limit has been reached"}}"#.to_owned(),
    };
    persisted_failure_reason(&text, Utc::now())
}

#[tokio::test]
async fn generated_goal_reset_resume_cases_drive_real_transactions() {
    for modeled in
        gents_lean_vocab::families::goal_reset_resume_contracts_reset::cases::<String, String>()
    {
        let case = &modeled.input;
        let before = serde_json::to_value(&case.before).unwrap();
        let f = Fixture::new(&before).await;
        if !case.before.children.is_empty() {
            f.seed_child(false).await;
        }
        let first_child = goal_continuation_identity(&f.goal.goal_id, PARENT, 1)
            .unwrap()
            .request_id;
        let repeat = case.name == "repeat_limit_resumes_at_its_new_reset";
        let stopped = if repeat { first_child.as_str() } else { PARENT };
        match case.name.as_str() {
            "reset_reached_resumes"
            | "retry_after_reset_resume_is_noop"
            | "not_opted_in_waits"
            | "reset_not_reported_waits"
            | "before_reset_waits"
            | "profile_moved_waits"
            | "account_disabled_waits"
            | "backend_disabled_waits"
            | "stale_reset_waits"
            | "paused_goal_is_not_timer_resumed"
            | "repeat_limit_resumes_at_its_new_reset" => {}
            name => panic!("unmapped generated reset case {name}"),
        }
        assert!(case.commit, "{}: a reset resume commits", case.name);
        let accounts = f.claude_accounts().await;
        connect_accounts_at(&f, -1).await;
        let facts = &case.facts;
        let failure = usage_limit(facts.reset_at.map(|seconds| seconds as i64));
        f.fail_with_call(
            stopped,
            "inference",
            &accounts.a,
            &failure,
            &at(facts.limit_started_at as i64).to_rfc3339(),
        )
        .await;
        let Some(ProviderLimit::UsageExhausted(limit)) =
            classify_provider_limit(&failure, Utc::now())
        else {
            panic!("{}: not a usage limit", case.name);
        };
        execute(
            &f.agent_access(),
            &format!(
                r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ last_failure: "{}" }}) {{ _docID }} }}"#,
                escape_graphql_string(&f.goal.doc_id),
                escape_graphql_string(&limit.to_string())
            ),
        )
        .await;
        let did = f.identity.did();
        if facts.opted_in {
            seed_goal_from_access(&accounts.access, did, SESSION, None, None, None, Some(true))
                .await
                .unwrap();
        }
        if !facts.profile_names_account {
            gents_inference::profile_switch::switch_profile_account(
                &accounts.access,
                did,
                PROFILE,
                &accounts.b,
                false,
                &[],
            )
            .await
            .unwrap();
        }
        if !facts.account_enabled {
            gents_inference::auth::credential::set_account_enabled(
                &accounts.access,
                did,
                &accounts.a_credential,
                false,
            )
            .await
            .unwrap();
        }
        if !facts.backend_enabled {
            disable_backend(&f, &accounts.a).await;
        }
        assert_eq!(f.observe().await, before, "{} initial", case.name);

        let goal = f.current_goal().await;
        let result = resume_at_reset(
            &f.node,
            f.identity.as_ref(),
            f.agent.as_ref(),
            &goal,
            at(facts.now as i64),
        )
        .await
        .unwrap();
        let outcome = match result {
            ResetResume::Resumed(receipt) => {
                assert_eq!(receipt.goal_status, GoalStatus::Active);
                let sequence = f.current_goal().await.continuation_sequence();
                let expected =
                    goal_continuation_identity(&goal.goal_id, stopped, sequence).unwrap();
                assert_eq!(receipt.request_id, expected.request_id);
                if receipt.created {
                    "created"
                } else {
                    "recovered"
                }
            }
            ResetResume::Deferred => "deferred",
            ResetResume::NotUsageLimited => "illegal",
        };
        let observed = if repeat {
            observe_repeat(&f, &first_child).await
        } else {
            f.observe().await
        };
        modeled.check(wire::ResetOutput {
            expected: serde_json::from_value(observed).unwrap(),
            outcome: outcome.to_owned(),
        });
        f.node.shutdown().await.expect("node shutdown");
    }
}

async fn observe_repeat(f: &Fixture, first_child: &str) -> Value {
    let number = |id: &str| match id {
        PARENT => 10,
        id if id == first_child => 20,
        id if id.starts_with("goal-cont-") => 40,
        id => panic!("unknown request {id}"),
    };
    let g = f.current_goal().await;
    let rows = request_rows(&f.agent_access()).await;
    let children: Vec<Value> = rows
        .iter()
        .filter(|r| {
            r.retry_key
                .as_deref()
                .is_some_and(|k| k.starts_with("goal-continuation:"))
        })
        .map(|child| {
            let predecessor = number(child.caused_by_parent_request_id.as_deref().unwrap());
            let sequence = child
                .input
                .as_ref()
                .unwrap()
                .goal_continuation
                .as_ref()
                .unwrap()
                .sequence;
            json!({"goal":1,"owner":"owner","session":"session","predecessor":predecessor,
                "predecessor_doc":predecessor * 10,"correlation":"graph-correlation",
                "source_document":"source","trigger_context":"context",
                "workspace_fingerprint":"workspace-authority",
                "semantic_fingerprint":"full-semantic-fingerprint",
                "child":number(&child.request_id),"sequence":sequence})
        })
        .collect();
    json!({"status":g.status,"blocked_audits":g.consecutive_blocked_audits.unwrap_or(0),
        "wrapup_requested":g.wrapup_requested.unwrap_or(false),
        "wrapup_completed":g.wrapup_completed.unwrap_or(false),
        "sequence":g.continuation_sequence(),
        "last_continued_from":g.last_continued_from_request_id.as_deref().map(number),
        "latest_request":number(&rows.first().unwrap().request_id),"children":children,
        "tokens_used":g.tokens_used.unwrap_or(0),"token_budget":g.token_budget})
}
