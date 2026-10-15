use super::support::*;
use super::*;
use crate::lifecycle::queue::goal_continuation_identity;
use gents_loop::provider_limit::{
    classify_provider_limit, persisted_failure_reason, ProviderLimit, ProviderLimitHeaders,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct ResetCase {
    name: String,
    before: Value,
    facts: Facts,
    commit: bool,
    expected: Value,
    outcome: String,
}

#[derive(Deserialize)]
struct Facts {
    opted_in: bool,
    reset_at: Option<i64>,
    limit_started_at: i64,
    now: i64,
    profile_names_account: bool,
    account_enabled: bool,
    backend_enabled: bool,
}

#[derive(Deserialize)]
struct Contracts {
    goal_reset_resume_cases: Vec<ResetCase>,
}

/// Lean times are seconds after this instant.
pub(super) fn at(seconds: i64) -> DateTime<Utc> {
    "2030-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + chrono::Duration::seconds(seconds)
}

/// Stamp the fixture's accounts connected at `seconds`: a sign-in records the
/// wall clock, which must not decide whether it precedes a limited call.
pub(super) async fn connect_accounts_at(f: &Fixture, seconds: i64) {
    execute(
        &f.node,
        &format!(
            r#"mutation {{ update_OAuthCredential(filter: {{ node_did: {{ _eq: "{}" }} }}, input: {{ connected_at: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(f.identity.did()),
            at(seconds).to_rfc3339()
        ),
    )
    .await;
}

/// The text a usage-limited call records: the Anthropic rejected-headers
/// reset when one is reported, else a body with no reset.
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
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_reset_resume_cases.len(), 11);
    let mut seen = std::collections::BTreeSet::new();
    for case in contracts.goal_reset_resume_cases {
        assert!(seen.insert(case.name.clone()), "duplicate generated case");
        assert!(case.commit, "{}: every reset case commits", case.name);
        let f = Fixture::new(&case.before).await;
        if !case.before["children"].as_array().unwrap().is_empty() {
            f.seed_child(false).await;
        }
        let first_child = goal_continuation_identity(&f.goal.goal_id, PARENT, 1)
            .unwrap()
            .request_id;
        // The limited request is the latest one while the Goal is limited.
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
        let accounts = f.claude_accounts().await;
        connect_accounts_at(&f, -1).await;
        let facts = &case.facts;
        let failure = usage_limit(facts.reset_at);
        f.fail_with_call(
            stopped,
            "inference",
            &accounts.a,
            &failure,
            &at(facts.limit_started_at).to_rfc3339(),
        )
        .await;
        // The goal source records the limit on the Goal it stops.
        let Some(ProviderLimit::UsageExhausted(limit)) =
            classify_provider_limit(&failure, Utc::now())
        else {
            panic!("{}: not a usage limit", case.name);
        };
        execute(
            &f.node,
            &format!(
                r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ last_failure: "{}" }}) {{ _docID }} }}"#,
                escape_graphql_string(&f.goal.doc_id),
                escape_graphql_string(&limit.to_string())
            ),
        )
        .await;
        let did = f.identity.did();
        if facts.opted_in {
            set_goal_from_access(&accounts.access, did, SESSION, None, None, None, Some(true))
                .await
                .unwrap();
        }
        if !facts.profile_names_account {
            crate::config_client::switch_profile_account(
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
            crate::oauth_credential::set_account_enabled(
                &accounts.access,
                did,
                &accounts.a_credential,
                false,
            )
            .await
            .unwrap();
        }
        if !facts.backend_enabled {
            execute(
                &f.node,
                &format!(
                    r#"mutation {{ update_InferenceBackend(filter: {{ node_did: {{ _eq: "{}" }}, backend_id: {{ _eq: "{}" }} }}, input: {{ enabled: false }}) {{ _docID }} }}"#,
                    escape_graphql_string(did),
                    escape_graphql_string(&accounts.a)
                ),
            )
            .await;
        }
        assert_eq!(f.observe().await, case.before, "{} initial", case.name);

        let goal = load_canonical_goal(&f.node, did, SESSION)
            .await
            .unwrap()
            .unwrap();
        let result = resume_at_reset(&f.node, f.identity.as_ref(), &goal, at(facts.now))
            .await
            .unwrap();
        match case.outcome.as_str() {
            "created" => {
                let receipt = result.unwrap_or_else(|| panic!("{}: no resume", case.name));
                assert!(receipt.created, "{}", case.name);
                assert_eq!(receipt.goal_status, GoalStatus::Active);
                let sequence = case.expected["sequence"].as_i64().unwrap();
                let expected =
                    goal_continuation_identity(&goal.goal_id, stopped, sequence).unwrap();
                assert_eq!(receipt.request_id, expected.request_id);
            }
            "deferred" | "illegal" => assert!(result.is_none(), "{}: {result:?}", case.name),
            outcome => panic!("unmapped outcome {outcome}"),
        }
        if repeat {
            assert_eq!(
                observe_repeat(&f, &first_child).await,
                case.expected,
                "{} durable result",
                case.name
            );
        } else {
            assert_eq!(
                f.observe().await,
                case.expected,
                "{} durable result",
                case.name
            );
        }
        f.node.shutdown().await;
    }
}

/// `Fixture::observe` checks every child against `PARENT`; a second-generation
/// child descends from the first child, so this projects the Goal and each
/// child's lineage instead (request 10 = `PARENT`, 20 = the first child, 40 =
/// its continuation).
async fn observe_repeat(f: &Fixture, first_child: &str) -> Value {
    let number = |id: &str| match id {
        PARENT => 10,
        id if id == first_child => 20,
        id if id.starts_with("goal-cont-") => 40,
        id => panic!("unknown request {id}"),
    };
    let g = load_canonical_goal(&f.node, f.identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    let rows = request_rows(&f.node).await;
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
