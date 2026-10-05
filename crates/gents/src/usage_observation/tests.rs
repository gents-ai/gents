use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use defra_node::EmbeddedNode;
use gents_loop::account_usage::{UsageCredits, UsagePlan, UsageReport, UsageSource, UsageWindow};
use serde_json::{json, Value};

use super::*;
use crate::oauth_credential::{
    oauth_credential_id, resolve_oauth_credential, test_support, upsert_oauth_credential,
    AccountPick,
};

const A: &str = "did:key:z6MkUsageA";
const B: &str = "did:key:z6MkUsageB";
const CODEX: &str = "chatgpt-codex";

async fn node() -> Arc<EmbeddedNode> {
    Arc::new(test_support::test_node().await)
}

fn local(node: &Arc<EmbeddedNode>) -> ConfigAccess {
    ConfigAccess::Local(node.clone())
}

fn credential(agent_did: &str, account_ref: Option<&str>, key: Option<&str>) -> OAuthCredential {
    OAuthCredential {
        doc_id: None,
        credential_id: match account_ref {
            Some(account_ref) => format!("{}:{account_ref}", oauth_credential_id(agent_did, CODEX)),
            None => oauth_credential_id(agent_did, CODEX),
        },
        agent_did: agent_did.to_string(),
        provider: CODEX.to_string(),
        access_token: "access-TEST".into(),
        refresh_token: "refresh-TEST".into(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: Utc::now() + Duration::hours(1),
        last_refresh: None,
        enabled: true,
        account_ref: account_ref.map(str::to_string),
        connected_at: None,
        provider_account_key: key.map(str::to_string),
        label: None,
    }
}

async fn seed(node: &Arc<EmbeddedNode>, row: &OAuthCredential) -> String {
    upsert_oauth_credential(node, row)
        .await
        .expect("seed credential")
}

fn window(label: &str, used_pct: f64, observed_at: DateTime<Utc>) -> UsageWindow {
    UsageWindow {
        label: label.to_string(),
        window_minutes: Some(300),
        used_pct,
        resets_at: None,
        source: UsageSource::Header,
        observed_at,
    }
}

fn report(windows: Vec<UsageWindow>) -> UsageReport {
    UsageReport {
        windows,
        ..UsageReport::default()
    }
}

fn backend(
    agent_did: &str,
    provider_kind: &str,
    backend_id: &str,
    auth: Value,
) -> InferenceBackend {
    serde_json::from_value(json!({
        "agent_did": agent_did,
        "backend_id": backend_id,
        "name": backend_id,
        "provider_kind": provider_kind,
        "endpoint": "http://127.0.0.1:9/v1",
        "auth": auth,
    }))
    .expect("backend")
}

fn api_key_account(agent_did: &str) -> UsageAccount {
    UsageAccount::Backend {
        agent_did: agent_did.to_string(),
        provider: "OpenAiCompatible".to_string(),
        backend_id: "backend-usage-a".to_string(),
    }
}

/// Every stored row as `(agent_did, provider, usage_key)`.
async fn rows(node: &Arc<EmbeddedNode>) -> Vec<(String, String, String)> {
    let response = node
        .execute("{ ProviderAccountUsage { agent_did provider usage_key } }")
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.expect("data");
    let mut rows: Vec<_> = data["ProviderAccountUsage"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| {
            let field = |name: &str| row[name].as_str().unwrap_or_default().to_string();
            (field("agent_did"), field("provider"), field("usage_key"))
        })
        .collect();
    rows.sort();
    rows
}

fn used(stored: &StoredUsage, label: &str) -> Option<f64> {
    stored
        .report
        .windows
        .iter()
        .find(|window| window.label == label)
        .map(|window| window.used_pct)
}

#[tokio::test]
async fn store_records_and_loads_a_credential_account() {
    let node = node().await;
    let row = credential(A, None, Some("acct-key-a"));
    seed(&node, &row).await;
    let account = UsageAccount::for_credential(&row);
    let now = Utc::now();

    record_usage(&node, &account, report(vec![window("primary", 12.0, now)]))
        .await
        .expect("record");

    let stored = load_usage(&local(&node), &account)
        .await
        .expect("load")
        .expect("stored usage");
    assert_eq!(used(&stored, "primary"), Some(12.0));
    assert_eq!(stored.observed_at, Some(now));
    assert_eq!(
        rows(&node).await,
        vec![(A.into(), CODEX.into(), "acct-key-a".into())]
    );
}

#[tokio::test]
async fn store_two_nodes_same_key_keep_separate_rows() {
    let node = node().await;
    let (row_a, row_b) = (
        credential(A, None, Some("acct-key-a")),
        credential(B, None, Some("acct-key-a")),
    );
    seed(&node, &row_a).await;
    seed(&node, &row_b).await;
    let now = Utc::now();
    let (account_a, account_b) = (
        UsageAccount::for_credential(&row_a),
        UsageAccount::for_credential(&row_b),
    );
    record_usage(
        &node,
        &account_a,
        report(vec![window("primary", 10.0, now)]),
    )
    .await
    .unwrap();
    record_usage(
        &node,
        &account_b,
        report(vec![window("primary", 90.0, now)]),
    )
    .await
    .unwrap();
    record_usage(
        &node,
        &api_key_account(A),
        report(vec![window("requests", 1.0, now)]),
    )
    .await
    .unwrap();
    record_usage(
        &node,
        &api_key_account(B),
        report(vec![window("requests", 2.0, now)]),
    )
    .await
    .unwrap();

    let access = local(&node);
    let load = |account| {
        let access = &access;
        async move { load_usage(access, &account).await.unwrap().expect("stored") }
    };
    assert_eq!(used(&load(account_a).await, "primary"), Some(10.0));
    assert_eq!(used(&load(account_b).await, "primary"), Some(90.0));
    assert_eq!(used(&load(api_key_account(A)).await, "requests"), Some(1.0));
    assert_eq!(used(&load(api_key_account(B)).await, "requests"), Some(2.0));
    assert_eq!(rows(&node).await.len(), 4);
}

#[tokio::test]
async fn store_older_report_does_not_replace_newer_windows() {
    let node = node().await;
    let row = credential(A, None, Some("acct-key-a"));
    seed(&node, &row).await;
    let account = UsageAccount::for_credential(&row);
    let now = Utc::now();
    record_usage(&node, &account, report(vec![window("primary", 50.0, now)]))
        .await
        .unwrap();
    record_usage(
        &node,
        &account,
        report(vec![
            window("primary", 10.0, now - Duration::minutes(1)),
            window("secondary", 5.0, now - Duration::minutes(1)),
        ]),
    )
    .await
    .unwrap();

    let stored = load_usage(&local(&node), &account).await.unwrap().unwrap();
    assert_eq!(used(&stored, "primary"), Some(50.0));
    assert_eq!(used(&stored, "secondary"), Some(5.0));
    assert_eq!(stored.observed_at, Some(now));
}

#[tokio::test]
async fn store_key_falls_back_to_the_account_reference() {
    let node = node().await;
    let original = credential(A, None, None);
    let second = credential(A, Some("acct-ref-2"), None);
    let original_doc = seed(&node, &original).await;
    let second_doc = seed(&node, &second).await;
    let now = Utc::now();
    for row in [&original, &second] {
        record_usage(
            &node,
            &UsageAccount::for_credential(row),
            report(vec![window("primary", 1.0, now)]),
        )
        .await
        .unwrap();
    }

    let keys: Vec<String> = rows(&node).await.into_iter().map(|row| row.2).collect();
    assert_eq!(
        keys,
        vec![
            format!("ref:acct-ref-2:{second_doc}"),
            format!("ref:original:{original_doc}")
        ]
    );
    assert!(keys.iter().all(|key| !key.contains("did:")));
}

#[tokio::test]
async fn store_key_is_read_when_the_write_happens() {
    let node = node().await;
    let mut row = credential(A, None, None);
    let doc = seed(&node, &row).await;
    let account = UsageAccount::for_credential(&row);
    let now = Utc::now();
    record_usage(
        &node,
        &account,
        report(vec![window("primary", 10.0, now - Duration::minutes(1))]),
    )
    .await
    .unwrap();

    row.provider_account_key = Some("acct-key-a".into());
    seed(&node, &row).await;
    record_usage(
        &node,
        &account,
        report(vec![window("secondary", 20.0, now)]),
    )
    .await
    .unwrap();

    let keys: Vec<String> = rows(&node).await.into_iter().map(|row| row.2).collect();
    assert_eq!(
        keys,
        vec!["acct-key-a".to_string(), format!("ref:original:{doc}")]
    );
    let stored = load_usage(&local(&node), &account).await.unwrap().unwrap();
    assert_eq!(used(&stored, "primary"), Some(10.0));
    assert_eq!(used(&stored, "secondary"), Some(20.0));
}

#[tokio::test]
async fn store_new_first_account_never_shows_the_removed_accounts_usage() {
    let node = node().await;
    let now = Utc::now();
    let mut removed = credential(A, None, None);
    removed.connected_at = Some(now - Duration::minutes(30));
    let doc_id = seed(&node, &removed).await;
    let account = UsageAccount::for_credential(&removed);
    let old = now - Duration::minutes(20);
    let mut old_report = report(vec![window("primary", 70.0, old)]);
    old_report.credits = Some(UsageCredits {
        has_credits: Some(true),
        unlimited: Some(false),
        balance: Some("1.00".into()),
        observed_at: old,
    });
    old_report.plan = Some(UsagePlan {
        name: "plus".into(),
        observed_at: old,
    });
    write(&node, &account, old_report, Some((old, None)))
        .await
        .unwrap();
    assert!(load_usage(&local(&node), &account).await.unwrap().is_some());

    let deleted = node
        .execute(&format!(
            r#"mutation {{ delete_OAuthCredential(docID: "{doc_id}") {{ _docID }} }}"#
        ))
        .await;
    assert!(!deleted.has_errors(), "{:?}", deleted.errors);
    let mut fresh = credential(A, None, None);
    fresh.connected_at = Some(now - Duration::minutes(5));
    fresh.access_token = "access-TEST-2".into();
    seed(&node, &fresh).await;

    let access = local(&node);
    let stored = load_usage(&access, &account).await.unwrap();
    assert_eq!(
        stored, None,
        "the removed account's usage shows: {stored:?}"
    );
    let for_backend = usage_for_backend(
        &access,
        A,
        &backend(
            A,
            "ChatGptCodex",
            "backend-usage-a",
            json!({ "kind": "principal_oauth" }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(for_backend, None);
}

/// Remove-all then sign in again within the same second: `connected_at` has
/// seconds precision, so only a key the new sign-in does not inherit keeps
/// the removed account's usage apart.
#[tokio::test]
async fn store_next_original_account_never_shows_the_removed_accounts_usage() {
    let node = node().await;
    let access = local(&node);
    let removed = crate::oauth_credential::store_sign_in(&access, credential(A, None, None), None)
        .await
        .unwrap()
        .credential;
    let account = UsageAccount::for_credential(&removed);
    record_usage(
        &node,
        &account,
        report(vec![window("primary", 70.0, Utc::now())]),
    )
    .await
    .unwrap();
    assert!(load_usage(&access, &account).await.unwrap().is_some());

    let credential_id = removed.credential_id.as_str();
    access
        .transact("test.remove_account", |txn| {
            Box::pin(async move {
                crate::oauth_credential::remove_account_in_txn(txn, A, credential_id).await
            })
        })
        .await
        .unwrap();
    let mut next = credential(A, None, None);
    next.access_token = "access-TEST-2".into();
    crate::oauth_credential::store_sign_in(&access, next, None)
        .await
        .unwrap();

    let stored = load_usage(&access, &account).await.unwrap();
    assert_eq!(
        stored, None,
        "the removed account's usage shows: {stored:?}"
    );
}

#[tokio::test]
async fn store_disabled_account_writes_and_loads_nothing() {
    let node = node().await;
    let mut row = credential(A, None, Some("acct-key-a"));
    row.enabled = false;
    seed(&node, &row).await;
    let account = UsageAccount::for_credential(&row);

    record_usage(
        &node,
        &account,
        report(vec![window("primary", 1.0, Utc::now())]),
    )
    .await
    .unwrap();

    assert!(rows(&node).await.is_empty());
    let access = local(&node);
    assert_eq!(load_usage(&access, &account).await.unwrap(), None);
    let for_backend = usage_for_backend(
        &access,
        A,
        &backend(
            A,
            "ChatGptCodex",
            "backend-usage-a",
            json!({ "kind": "principal_oauth" }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(for_backend, None);
}

#[tokio::test]
async fn store_unchanged_report_within_a_minute_is_not_rewritten() {
    let node = node().await;
    let row = credential(A, None, Some("acct-key-a"));
    seed(&node, &row).await;
    let account = UsageAccount::for_credential(&row);
    let access = local(&node);
    let first = Utc::now() - Duration::minutes(5);
    record_usage(
        &node,
        &account,
        report(vec![window("primary", 10.0, first)]),
    )
    .await
    .unwrap();
    let observed_at = |stored: Option<StoredUsage>| stored.unwrap().observed_at;

    let ten = first + Duration::seconds(10);
    record_usage(&node, &account, report(vec![window("primary", 10.0, ten)]))
        .await
        .unwrap();
    assert_eq!(
        observed_at(load_usage(&access, &account).await.unwrap()),
        Some(first),
        "unchanged values 10 s later are not rewritten"
    );

    let late = first + Duration::seconds(61);
    record_usage(&node, &account, report(vec![window("primary", 10.0, late)]))
        .await
        .unwrap();
    assert_eq!(
        observed_at(load_usage(&access, &account).await.unwrap()),
        Some(late)
    );

    let changed = late + Duration::seconds(10);
    record_usage(
        &node,
        &account,
        report(vec![window("primary", 11.0, changed)]),
    )
    .await
    .unwrap();
    let stored = load_usage(&access, &account).await.unwrap().unwrap();
    assert_eq!(stored.observed_at, Some(changed));
    assert_eq!(used(&stored, "primary"), Some(11.0));
}

#[tokio::test]
async fn store_api_key_backend_uses_the_backend_id() {
    let node = node().await;
    let now = Utc::now();
    record_usage(
        &node,
        &api_key_account(A),
        report(vec![window("tokens", 3.0, now)]),
    )
    .await
    .unwrap();

    assert_eq!(
        rows(&node).await,
        vec![(
            A.into(),
            "OpenAiCompatible".into(),
            "backend-usage-a".into()
        )]
    );
    let stored = usage_for_backend(
        &local(&node),
        A,
        &backend(
            A,
            "OpenAiCompatible",
            "backend-usage-a",
            json!({ "kind": "api_key", "key": "sk-TEST" }),
        ),
    )
    .await
    .unwrap()
    .expect("stored");
    assert_eq!(used(&stored, "tokens"), Some(3.0));
}

#[tokio::test]
async fn store_usage_for_backend_resolves_the_account_and_plan() {
    let node = node().await;
    seed(&node, &credential(A, None, Some("acct-key-a"))).await;
    let mut second = credential(A, Some("acct-ref-2"), Some("acct-key-b"));
    second.chatgpt_plan_type = Some("plus".into());
    seed(&node, &second).await;
    let now = Utc::now();
    record_usage(
        &node,
        &UsageAccount::for_credential(&second),
        report(vec![window("primary", 42.0, now)]),
    )
    .await
    .unwrap();

    let stored = usage_for_backend(
        &local(&node),
        A,
        &backend(
            A,
            "ChatGptCodex",
            "backend-usage-a",
            json!({ "kind": "principal_oauth", "account_ref": "acct-ref-2" }),
        ),
    )
    .await
    .unwrap()
    .expect("the referenced account's usage");
    assert_eq!(used(&stored, "primary"), Some(42.0));
    assert_eq!(
        stored.report.plan.map(|plan| plan.name),
        Some("plus".into())
    );
}

#[tokio::test]
async fn store_never_writes_the_credential_row() {
    let node = node().await;
    let row = credential(A, None, None);
    seed(&node, &row).await;
    let access = local(&node);
    let read = || resolve_oauth_credential(&access, A, CODEX, AccountPick::Reference(None));
    let before = read().await.unwrap().expect("row");

    record_usage(
        &node,
        &UsageAccount::for_credential(&row),
        report(vec![window("primary", 5.0, Utc::now())]),
    )
    .await
    .unwrap();

    assert_eq!(rows(&node).await.len(), 1);
    assert_eq!(read().await.unwrap().expect("row"), before);
}

const CODEX_USAGE_BODY: &str = r#"{"plan_type":"plus","rate_limit":{"primary_window":{"used_percent":12,"limit_window_seconds":18000,"reset_at":4102444800}}}"#;
const OPENROUTER_KEY_BODY: &str =
    r#"{"data":{"limit":20,"limit_remaining":15,"limit_reset":"daily"}}"#;

/// A signed-in account of `provider` for `agent_did`, expiring at `expires_at`.
async fn sign_in(
    node: &Arc<EmbeddedNode>,
    agent_did: &str,
    provider: &str,
    expires_at: DateTime<Utc>,
    account_id: Option<&str>,
) {
    test_support::seed_credential(node, agent_did, provider, expires_at).await;
    if let Some(account_id) = account_id {
        let mut row = resolve_oauth_credential(
            &local(node),
            agent_did,
            provider,
            AccountPick::Reference(None),
        )
        .await
        .unwrap()
        .expect("row");
        row.account_id = Some(account_id.to_string());
        upsert_oauth_credential(node, &row).await.unwrap();
    }
}

/// The server origin of a `one_shot_token_server` URL.
fn origin(url: &str) -> String {
    url.trim_end_matches("/v1/oauth/token").to_string()
}

fn first_line(request: &str) -> &str {
    request.lines().next().unwrap_or_default()
}

fn api_key_backend(endpoint: String, backend_id: &str, auth: Value) -> InferenceBackend {
    let mut backend = backend(A, "OpenRouter", backend_id, auth);
    backend.endpoint = endpoint;
    backend
}

fn openrouter_account(backend_id: &str) -> UsageAccount {
    UsageAccount::Backend {
        agent_did: A.to_string(),
        provider: "OpenRouter".to_string(),
        backend_id: backend_id.to_string(),
    }
}

async fn read(
    node: &Arc<EmbeddedNode>,
    agent_did: &str,
    backend: &InferenceBackend,
    trigger: UsageTrigger,
    endpoints: &UsageEndpoints,
    now: DateTime<Utc>,
) -> UsageRead {
    read_account_usage(node.clone(), agent_did, backend, trigger, endpoints, now)
        .await
        .expect("read")
}

/// The server got no connection within a short wait.
async fn never_called(handle: tokio::task::JoinHandle<String>) {
    let mut handle = handle;
    let waited = tokio::time::timeout(std::time::Duration::from_millis(300), &mut handle).await;
    assert!(waited.is_err(), "the server was called: {waited:?}");
    handle.abort();
}

#[tokio::test]
async fn read_codex_sends_account_header_to_the_backend_host_and_stores_endpoint_windows() {
    let did = "did:key:z6MkUsageReadCodex";
    let node = node().await;
    sign_in(
        &node,
        did,
        CODEX,
        Utc::now() + Duration::hours(1),
        Some("acct-ws"),
    )
    .await;
    let now = Utc::now();
    let mut codex = backend(
        did,
        "ChatGptCodex",
        "backend-usage-a",
        json!({ "kind": "principal_oauth" }),
    );
    for (step, suffix) in [(0, "/codex"), (1, "/codex/")] {
        let (url, handle) = test_support::one_shot_token_server(200, CODEX_USAGE_BODY).await;
        codex.endpoint = format!("{}{suffix}", origin(&url));
        let at = now + Duration::minutes(6 * step);
        assert_eq!(
            read(
                &node,
                did,
                &codex,
                UsageTrigger::Open,
                &UsageEndpoints::default(),
                at
            )
            .await,
            UsageRead::Read,
            "{suffix}"
        );
        let request = handle.await.unwrap();
        assert_eq!(first_line(&request), "GET /wham/usage HTTP/1.1", "{suffix}");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("chatgpt-account-id: acct-ws"),
            "{request}"
        );
    }

    let stored = usage_for_backend(&local(&node), did, &codex)
        .await
        .unwrap()
        .expect("stored");
    let primary = &stored.report.windows[0];
    assert_eq!(
        (primary.label.as_str(), primary.used_pct),
        ("primary", 12.0)
    );
    assert_eq!(primary.source, UsageSource::Endpoint);
    assert!(stored.read_at.is_some());
    assert_eq!(stored.read_error, None);
}

#[tokio::test]
async fn read_grok_sends_identity_headers_and_credits_query() {
    let did = "did:key:z6MkUsageReadGrok";
    let node = node().await;
    let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
    sign_in(&node, did, provider, Utc::now() + Duration::hours(1), None).await;
    let (url, handle) = test_support::one_shot_token_server(
        200,
        r#"{"config":{"creditUsagePercent":30,"currentPeriod":{"start":"2100-01-01T00:00:00Z","end":"2100-01-08T00:00:00Z"}}}"#,
    )
    .await;
    let endpoints = UsageEndpoints {
        grok_billing: format!("{}/v1/billing?format=credits", origin(&url)),
        ..UsageEndpoints::default()
    };
    let grok = backend(
        did,
        "XaiGrokOAuth",
        "backend-usage-grok",
        json!({ "kind": "principal_oauth" }),
    );

    assert_eq!(
        read(
            &node,
            did,
            &grok,
            UsageTrigger::Open,
            &endpoints,
            Utc::now()
        )
        .await,
        UsageRead::Read
    );
    let request = handle.await.unwrap();
    assert_eq!(
        first_line(&request),
        "GET /v1/billing?format=credits HTTP/1.1"
    );
    let lower = request.to_ascii_lowercase();
    assert!(
        lower.contains("x-xai-token-auth: xai-grok-cli"),
        "{request}"
    );
    assert!(
        lower.contains("authorization: bearer access-test"),
        "{request}"
    );
    assert_eq!(
        UsageEndpoints::default().grok_billing,
        "https://cli-chat-proxy.grok.com/v1/billing?format=credits"
    );
    let stored = usage_for_backend(&local(&node), did, &grok)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.report.windows[0].used_pct, 30.0);
}

#[tokio::test]
async fn read_openrouter_uses_the_backend_key_and_endpoint() {
    let node = node().await;
    let (url, handle) = test_support::one_shot_token_server(200, OPENROUTER_KEY_BODY).await;
    let openrouter = api_key_backend(
        format!("{}/api/v1", origin(&url)),
        "backend-usage-or",
        json!({ "kind": "api_key", "key": "sk-or-TEST" }),
    );

    assert_eq!(
        read(
            &node,
            A,
            &openrouter,
            UsageTrigger::Open,
            &UsageEndpoints::default(),
            Utc::now()
        )
        .await,
        UsageRead::Read
    );
    let request = handle.await.unwrap();
    assert_eq!(first_line(&request), "GET /api/v1/key HTTP/1.1");
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-or-test"),
        "{request}"
    );
    let stored = usage_for_backend(&local(&node), A, &openrouter)
        .await
        .unwrap()
        .unwrap();
    let daily = &stored.report.windows[0];
    assert_eq!((daily.label.as_str(), daily.used_pct), ("daily", 25.0));
}

#[tokio::test]
async fn read_claude_on_open_then_skips_a_recent_read() {
    let did = "did:key:z6MkUsageReadClaudeOpen";
    let node = node().await;
    let provider = crate::claude_oauth::CLAUDE_OAUTH_PROVIDER;
    let now = Utc::now();
    sign_in(&node, did, provider, now + Duration::hours(1), None).await;
    let (url, handle) = test_support::one_shot_token_server(
        200,
        r#"{"five_hour":{"utilization":25.0,"resets_at":"2100-01-01T00:00:00Z"}}"#,
    )
    .await;
    let endpoints = UsageEndpoints {
        claude_usage: format!("{}/api/oauth/usage", origin(&url)),
        ..UsageEndpoints::default()
    };
    let claude = backend(
        did,
        "ClaudeCliSubscription",
        "backend-usage-claude",
        json!({ "kind": "principal_oauth" }),
    );

    assert_eq!(
        read(&node, did, &claude, UsageTrigger::Open, &endpoints, now).await,
        UsageRead::Read
    );
    assert_eq!(
        first_line(&handle.await.unwrap()),
        "GET /api/oauth/usage HTTP/1.1"
    );
    assert_eq!(
        read(
            &node,
            did,
            &claude,
            UsageTrigger::Open,
            &endpoints,
            now + Duration::minutes(1)
        )
        .await,
        UsageRead::SkippedRecent
    );
}

#[tokio::test]
async fn read_claude_on_refresh_sends_the_oauth_beta() {
    let did = "did:key:z6MkUsageReadClaude";
    let node = node().await;
    let provider = crate::claude_oauth::CLAUDE_OAUTH_PROVIDER;
    sign_in(&node, did, provider, Utc::now() + Duration::hours(1), None).await;
    let (url, handle) = test_support::one_shot_token_server(
        200,
        r#"{"five_hour":{"utilization":25.0,"resets_at":"2100-01-01T00:00:00Z"},"seven_day":null}"#,
    )
    .await;
    let endpoints = UsageEndpoints {
        claude_usage: format!("{}/api/oauth/usage", origin(&url)),
        ..UsageEndpoints::default()
    };
    let claude = backend(
        did,
        "ClaudeCliSubscription",
        "backend-usage-claude",
        json!({ "kind": "principal_oauth" }),
    );

    assert_eq!(
        read(
            &node,
            did,
            &claude,
            UsageTrigger::Refresh,
            &endpoints,
            Utc::now()
        )
        .await,
        UsageRead::Read
    );
    let request = handle.await.unwrap();
    assert_eq!(first_line(&request), "GET /api/oauth/usage HTTP/1.1");
    assert!(
        request.to_ascii_lowercase().contains(&format!(
            "anthropic-beta: {}",
            crate::claude_messages::OAUTH_BETA
        )),
        "{request}"
    );
    let stored = usage_for_backend(&local(&node), did, &claude)
        .await
        .unwrap()
        .unwrap();
    let five = &stored.report.windows[0];
    assert_eq!((five.label.as_str(), five.used_pct), ("5h", 25.0));
}

#[tokio::test]
async fn read_expired_sign_in_refreshes_through_the_owner_then_reads() {
    let did = "did:key:z6MkUsageReadRefresh";
    let node = node().await;
    sign_in(&node, did, CODEX, Utc::now() - Duration::hours(1), None).await;
    let token =
        test_support::unsigned_jwt(json!({ "exp": (Utc::now() + Duration::hours(1)).timestamp() }));
    let token_body: &'static str = Box::leak(
        json!({ "access_token": token, "refresh_token": "refresh-rotated", "expires_in": 900 })
            .to_string()
            .into_boxed_str(),
    );
    let _env = test_support::TOKEN_URL_ENV.lock().await;
    let (token_url, token_handle) = test_support::one_shot_token_server(200, token_body).await;
    std::env::set_var(REFRESH_ENV, &token_url);
    let (usage_url, usage_handle) =
        test_support::one_shot_token_server(200, CODEX_USAGE_BODY).await;
    let mut codex = backend(
        did,
        "ChatGptCodex",
        "backend-usage-a",
        json!({ "kind": "principal_oauth" }),
    );
    codex.endpoint = format!("{}/codex", origin(&usage_url));

    let result = read(
        &node,
        did,
        &codex,
        UsageTrigger::Open,
        &UsageEndpoints::default(),
        Utc::now(),
    )
    .await;
    std::env::remove_var(REFRESH_ENV);

    assert_eq!(result, UsageRead::Read);
    token_handle.await.unwrap();
    let request = usage_handle.await.unwrap();
    assert_eq!(first_line(&request), "GET /wham/usage HTTP/1.1");
    assert!(request.contains(&format!("Bearer {token}")), "{request}");
    let stored = usage_for_backend(&local(&node), did, &codex)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.report.windows[0].used_pct, 12.0);
}

#[tokio::test]
async fn read_api_key_without_a_source_is_not_reported() {
    let node = node().await;
    let openai = backend(
        A,
        "OpenAiCompatible",
        "backend-usage-a",
        json!({ "kind": "api_key", "key": "sk-TEST" }),
    );
    assert_eq!(
        read(
            &node,
            A,
            &openai,
            UsageTrigger::Refresh,
            &UsageEndpoints::default(),
            Utc::now()
        )
        .await,
        UsageRead::NotReported
    );
}

const REFRESH_ENV: &str = gents_protocol::chatgpt_oauth::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR;

#[tokio::test]
async fn read_guard_recent_read_is_skipped() {
    let node = node().await;
    let now = Utc::now();
    let (url, handle) = test_support::one_shot_token_server(429, "{}").await;
    let openrouter = api_key_backend(
        format!("{}/api/v1", origin(&url)),
        "backend-usage-recent",
        json!({ "kind": "api_key", "key": "sk-or-TEST" }),
    );
    assert_eq!(
        read(
            &node,
            A,
            &openrouter,
            UsageTrigger::Open,
            &UsageEndpoints::default(),
            now
        )
        .await,
        UsageRead::Unavailable("throttled".into())
    );
    handle.await.unwrap();
    let (url, handle) = test_support::one_shot_token_server(200, OPENROUTER_KEY_BODY).await;
    let openrouter = api_key_backend(
        format!("{}/api/v1", origin(&url)),
        "backend-usage-recent",
        json!({ "kind": "api_key", "key": "sk-or-TEST" }),
    );
    assert_eq!(
        read(
            &node,
            A,
            &openrouter,
            UsageTrigger::Refresh,
            &UsageEndpoints::default(),
            now + Duration::minutes(1)
        )
        .await,
        UsageRead::SkippedRecent
    );
    never_called(handle).await;

    // A recent read skips before any token is touched: an expired sign-in
    // is not refreshed.
    let did = "did:key:z6MkUsageReadRecent";
    sign_in(&node, did, CODEX, now - Duration::hours(1), None).await;
    let account = UsageAccount::Credential {
        agent_did: did.to_string(),
        provider: CODEX.to_string(),
        account_ref: None,
    };
    write(&node, &account, UsageReport::default(), Some((now, None)))
        .await
        .unwrap();
    let _env = test_support::TOKEN_URL_ENV.lock().await;
    let (token_url, token_handle) = test_support::one_shot_token_server(200, "{}").await;
    std::env::set_var(REFRESH_ENV, &token_url);
    let codex = backend(
        did,
        "ChatGptCodex",
        "backend-usage-a",
        json!({ "kind": "principal_oauth" }),
    );
    let result = read(
        &node,
        did,
        &codex,
        UsageTrigger::Open,
        &UsageEndpoints::default(),
        now + Duration::minutes(1),
    )
    .await;
    std::env::remove_var(REFRESH_ENV);
    assert_eq!(result, UsageRead::SkippedRecent);
    never_called(token_handle).await;
}

#[tokio::test]
async fn read_guard_throttled_is_unavailable_not_exhausted() {
    let node = node().await;
    let now = Utc::now();
    record_usage(
        &node,
        &openrouter_account("backend-usage-throttled"),
        report(vec![window("daily", 40.0, now - Duration::minutes(10))]),
    )
    .await
    .unwrap();
    let (url, handle) =
        test_support::one_shot_token_server(429, r#"{"error":"rate limited"}"#).await;
    let openrouter = api_key_backend(
        format!("{}/api/v1", origin(&url)),
        "backend-usage-throttled",
        json!({ "kind": "api_key", "key": "sk-or-TEST" }),
    );

    assert_eq!(
        read(
            &node,
            A,
            &openrouter,
            UsageTrigger::Refresh,
            &UsageEndpoints::default(),
            now
        )
        .await,
        UsageRead::Unavailable("throttled".into())
    );
    handle.await.unwrap();
    let stored = load_usage(
        &local(&node),
        &openrouter_account("backend-usage-throttled"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(used(&stored, "daily"), Some(40.0));
    assert_eq!(stored.report.windows.len(), 1);
    assert_eq!(stored.read_error.as_deref(), Some("throttled"));
}

#[tokio::test]
async fn read_guard_malformed_stores_nothing_invented() {
    let node = node().await;
    let (url, handle) = test_support::one_shot_token_server(200, r#"{"error":"x"}"#).await;
    let openrouter = api_key_backend(
        format!("{}/api/v1", origin(&url)),
        "backend-usage-malformed",
        json!({ "kind": "api_key", "key": "sk-or-TEST" }),
    );

    assert_eq!(
        read(
            &node,
            A,
            &openrouter,
            UsageTrigger::Open,
            &UsageEndpoints::default(),
            Utc::now()
        )
        .await,
        UsageRead::Unavailable("malformed".into())
    );
    handle.await.unwrap();
    let stored = load_usage(
        &local(&node),
        &openrouter_account("backend-usage-malformed"),
    )
    .await
    .unwrap()
    .expect("the read is recorded");
    assert!(stored.report.is_empty(), "{stored:?}");
    assert_eq!(stored.read_error.as_deref(), Some("malformed"));
}

#[tokio::test]
async fn read_guard_refresh_failure_is_sign_in_expired() {
    let did = "did:key:z6MkUsageReadRefreshFail";
    let node = node().await;
    sign_in(&node, did, CODEX, Utc::now() - Duration::hours(1), None).await;
    let _env = test_support::TOKEN_URL_ENV.lock().await;
    let (token_url, token_handle) =
        test_support::one_shot_token_server(401, r#"{"error":"invalid_grant"}"#).await;
    std::env::set_var(REFRESH_ENV, &token_url);
    let (usage_url, usage_handle) =
        test_support::one_shot_token_server(200, CODEX_USAGE_BODY).await;
    let mut codex = backend(
        did,
        "ChatGptCodex",
        "backend-usage-a",
        json!({ "kind": "principal_oauth" }),
    );
    codex.endpoint = format!("{}/codex", origin(&usage_url));

    let result = read(
        &node,
        did,
        &codex,
        UsageTrigger::Refresh,
        &UsageEndpoints::default(),
        Utc::now(),
    )
    .await;
    std::env::remove_var(REFRESH_ENV);

    assert_eq!(result, UsageRead::Unavailable("sign-in expired".into()));
    token_handle.await.unwrap();
    never_called(usage_handle).await;
    assert!(rows(&node).await.is_empty());
}

#[tokio::test]
async fn read_guard_disabled_account_is_not_read() {
    let node = node().await;
    let mut openrouter = api_key_backend(
        "http://127.0.0.1:9/api/v1".to_string(),
        "backend-usage-disabled",
        json!({ "kind": "api_key", "key": "sk-or-TEST" }),
    );
    openrouter.enabled = false;
    assert_eq!(
        read(
            &node,
            A,
            &openrouter,
            UsageTrigger::Refresh,
            &UsageEndpoints::default(),
            Utc::now()
        )
        .await,
        UsageRead::Disabled
    );

    let mut row = credential(A, None, Some("acct-key-a"));
    row.enabled = false;
    seed(&node, &row).await;
    let codex = backend(
        A,
        "ChatGptCodex",
        "backend-usage-a",
        json!({ "kind": "principal_oauth" }),
    );
    assert_eq!(
        read(
            &node,
            A,
            &codex,
            UsageTrigger::Refresh,
            &UsageEndpoints::default(),
            Utc::now()
        )
        .await,
        UsageRead::Unavailable("no enabled account".into())
    );
    assert!(rows(&node).await.is_empty());
}

#[tokio::test]
async fn read_guard_unreachable_is_unavailable() {
    let node = node().await;
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let openrouter = api_key_backend(
        format!("http://127.0.0.1:{port}/api/v1"),
        "backend-usage-unreachable",
        json!({ "kind": "api_key", "key": "sk-or-TEST" }),
    );
    assert_eq!(
        read(
            &node,
            A,
            &openrouter,
            UsageTrigger::Open,
            &UsageEndpoints::default(),
            Utc::now()
        )
        .await,
        UsageRead::Unavailable("unreachable".into())
    );
}

#[tokio::test]
async fn read_guard_unresolvable_key_is_unavailable_with_no_request() {
    let node = node().await;
    let (url, handle) = test_support::one_shot_token_server(200, OPENROUTER_KEY_BODY).await;
    let openrouter = api_key_backend(
        format!("{}/api/v1", origin(&url)),
        "backend-usage-env",
        json!({ "kind": "environment", "variable": "GENTS_USAGE_TEST_UNSET_KEY" }),
    );
    assert_eq!(
        read(
            &node,
            A,
            &openrouter,
            UsageTrigger::Open,
            &UsageEndpoints::default(),
            Utc::now()
        )
        .await,
        UsageRead::Unavailable("no key".into())
    );
    never_called(handle).await;
}

fn view_now() -> DateTime<Utc> {
    "2026-10-01T16:00:00Z".parse().expect("now")
}

fn stored_with(windows: Vec<UsageWindow>) -> StoredUsage {
    StoredUsage {
        report: report(windows),
        ..StoredUsage::default()
    }
}

#[test]
fn usage_view_fresh_window_has_countdown_and_age() {
    use crate::BackendProviderKind as Kind;
    let now = view_now();
    let mut fresh = window("5h", 42.0, now - Duration::minutes(4));
    fresh.resets_at = Some(now + Duration::minutes(133));
    let view = usage_view(
        Some(&stored_with(vec![fresh])),
        Kind::ClaudeCliSubscription,
        now,
    );
    assert_eq!(view.note, None);
    let [shown] = view.windows.as_slice() else {
        panic!("one window: {view:?}");
    };
    assert_eq!(shown.label, "5h");
    assert_eq!(shown.used_pct, 42.0);
    assert_eq!(shown.resets_in_secs, Some(133 * 60));
    assert_eq!(shown.age_secs, 4 * 60);
    assert!(!shown.last_known);
    assert_eq!(shown.source, UsageSource::Header);
}

#[test]
fn usage_view_stale_window_is_last_known() {
    let now = view_now();
    let stale = window("primary", 10.0, now - Duration::minutes(20));
    let view = usage_view(
        Some(&stored_with(vec![stale])),
        crate::BackendProviderKind::ChatGptCodex,
        now,
    );
    assert!(view.windows[0].last_known, "{view:?}");
    assert_eq!(view.windows[0].age_secs, 20 * 60);
}

#[test]
fn usage_view_past_reset_or_old_windows_are_dropped_to_unknown() {
    let now = view_now();
    let mut past = window("5h", 90.0, now);
    past.resets_at = Some(now - Duration::seconds(1));
    let old = window("7d", 50.0, now - Duration::minutes(61));
    let view = usage_view(
        Some(&stored_with(vec![past, old])),
        crate::BackendProviderKind::ClaudeCliSubscription,
        now,
    );
    assert!(view.windows.is_empty(), "{view:?}");
    assert_eq!(view.note, Some("unknown"));
}

#[test]
fn usage_view_nothing_stored_is_unknown() {
    use crate::BackendProviderKind as Kind;
    for kind in [
        Kind::ChatGptCodex,
        Kind::ClaudeCliSubscription,
        Kind::XaiGrokOAuth,
        Kind::OpenRouter,
    ] {
        let view = usage_view(None, kind, view_now());
        assert!(view.windows.is_empty(), "{kind:?}");
        assert_eq!(view.note, Some("unknown"), "{kind:?}");
    }
}

#[test]
fn usage_view_openai_compatible_never_seen_is_not_reported() {
    let kind = crate::BackendProviderKind::OpenAiCompatible;
    let now = view_now();
    assert_eq!(usage_view(None, kind, now).note, Some("not reported"));
    assert_eq!(
        usage_view(Some(&StoredUsage::default()), kind, now).note,
        Some("not reported")
    );
    let seen = usage_view(
        Some(&stored_with(vec![window("requests", 7.0, now)])),
        kind,
        now,
    );
    assert_eq!(seen.note, None);
    assert_eq!(seen.windows[0].label, "requests");
}

#[test]
fn usage_view_uncapped_openrouter_is_no_cap_on_this_key() {
    let kind = crate::BackendProviderKind::OpenRouter;
    let now = view_now();
    let read = StoredUsage {
        read_at: Some(now - Duration::minutes(1)),
        ..StoredUsage::default()
    };
    assert_eq!(
        usage_view(Some(&read), kind, now).note,
        Some("no cap on this key")
    );
    let failed = StoredUsage {
        read_error: Some("throttled".into()),
        ..read.clone()
    };
    assert_eq!(usage_view(Some(&failed), kind, now).note, Some("unknown"));
    let aged_out = StoredUsage {
        report: report(vec![window("daily", 25.0, now - Duration::minutes(61))]),
        ..read
    };
    assert_eq!(usage_view(Some(&aged_out), kind, now).note, Some("unknown"));
}

#[test]
fn usage_view_plan_and_read_state_carry_over() {
    let now = view_now();
    let mut stored = stored_with(vec![window("primary", 5.0, now)]);
    stored.report.plan = Some(UsagePlan {
        name: "plus".into(),
        observed_at: now,
    });
    stored.read_at = Some(now - Duration::minutes(2));
    stored.read_error = Some("throttled".into());
    let view = usage_view(Some(&stored), crate::BackendProviderKind::ChatGptCodex, now);
    assert_eq!(view.plan.as_deref(), Some("plus"));
    assert_eq!(view.read_at, stored.read_at);
    assert_eq!(view.read_error.as_deref(), Some("throttled"));
}

#[test]
fn usage_view_claude_endpoint_windows_show_as_read() {
    let kind = crate::BackendProviderKind::ClaudeCliSubscription;
    let now = view_now();
    let mut endpoint = window("7d", 72.0, now);
    endpoint.source = UsageSource::Endpoint;
    let view = usage_view(Some(&stored_with(vec![endpoint])), kind, now);
    assert_eq!(view.windows.len(), 1, "{view:?}");
    assert_eq!(view.windows[0].label, "7d");
    assert_eq!(view.windows[0].used_pct, 72.0);
    assert_eq!(view.windows[0].source, UsageSource::Endpoint);
    assert_eq!(view.note, None);
}

const GROK: &str = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
const CLAUDE: &str = crate::claude_oauth::CLAUDE_OAUTH_PROVIDER;
const GROK_BILLING_BODY: &str = r#"{"config":{"creditUsagePercent":30,"currentPeriod":{"start":"2100-01-01T00:00:00Z","end":"2100-01-08T00:00:00Z"}}}"#;

/// A second account of `provider` with reference `account_ref`.
async fn sign_in_ref(
    node: &Arc<EmbeddedNode>,
    agent_did: &str,
    provider: &str,
    account_ref: &str,
    enabled: bool,
) {
    let mut row = credential(agent_did, Some(account_ref), None);
    row.provider = provider.to_string();
    row.credential_id = format!("{}:{account_ref}", oauth_credential_id(agent_did, provider));
    row.enabled = enabled;
    row.connected_at = Some(Utc::now());
    seed(node, &row).await;
}

fn oauth_backend(
    agent_did: &str,
    kind: &str,
    backend_id: &str,
    account_ref: Option<&str>,
    enabled: bool,
) -> InferenceBackend {
    let mut backend = backend(
        agent_did,
        kind,
        backend_id,
        json!({ "kind": "principal_oauth", "account_ref": account_ref }),
    );
    backend.enabled = enabled;
    if kind == "ClaudeCliSubscription" {
        backend.endpoint = "claude-cli://subscription".to_string();
    }
    backend
}

async fn store_backends(node: &Arc<EmbeddedNode>, backends: &[InferenceBackend]) {
    for backend in backends {
        crate::config_client::write_inference_backend_document(&local(node), backend)
            .await
            .expect("backend");
    }
}

fn grok_endpoints(url: &str) -> UsageEndpoints {
    UsageEndpoints {
        grok_billing: format!("{}/v1/billing?format=credits", origin(url)),
        ..UsageEndpoints::default()
    }
}

async fn principal_read(
    node: &Arc<EmbeddedNode>,
    agent_did: &str,
    trigger: UsageTrigger,
    provider: Option<&str>,
    endpoints: &UsageEndpoints,
) -> Vec<AccountUsageRead> {
    read_principal_usage(
        node.clone(),
        agent_did,
        trigger,
        provider,
        endpoints,
        Utc::now(),
    )
    .await
    .expect("principal read")
}

fn outcome<'a>(reads: &'a [AccountUsageRead], account_ref: Option<&str>) -> &'a UsageRead {
    let matching: Vec<_> = reads
        .iter()
        .filter(|read| read.account_ref.as_deref() == account_ref && read.backend_id.is_none())
        .collect();
    assert_eq!(matching.len(), 1, "{reads:?}");
    &matching[0].outcome
}

#[tokio::test]
async fn principal_read_skips_disabled_accounts_without_a_request() {
    let did = "did:key:z6MkUsageSurfDisabled";
    let node = node().await;
    sign_in(&node, did, GROK, Utc::now() + Duration::hours(1), None).await;
    sign_in_ref(&node, did, GROK, "acct-g2", false).await;
    store_backends(
        &node,
        &[
            oauth_backend(did, "XaiGrokOAuth", "backend-usage-a", None, true),
            oauth_backend(
                did,
                "XaiGrokOAuth",
                "backend-usage-b",
                Some("acct-g2"),
                true,
            ),
        ],
    )
    .await;
    let (url, handle) = test_support::one_shot_token_server(200, GROK_BILLING_BODY).await;

    let reads = principal_read(&node, did, UsageTrigger::Open, None, &grok_endpoints(&url)).await;

    assert_eq!(reads.len(), 2, "{reads:?}");
    assert_eq!(outcome(&reads, None), &UsageRead::Read);
    assert_eq!(outcome(&reads, Some("acct-g2")), &UsageRead::Disabled);
    let request = handle.await.unwrap();
    assert_eq!(
        first_line(&request),
        "GET /v1/billing?format=credits HTTP/1.1"
    );
}

#[tokio::test]
async fn principal_read_open_reads_claude_once_within_the_window() {
    let did = "did:key:z6MkUsageSurfClaude";
    let node = node().await;
    sign_in(&node, did, CLAUDE, Utc::now() + Duration::hours(1), None).await;
    store_backends(
        &node,
        &[oauth_backend(
            did,
            "ClaudeCliSubscription",
            "backend-usage-a",
            None,
            true,
        )],
    )
    .await;
    let (url, handle) = test_support::one_shot_token_server(
        200,
        r#"{"five_hour":{"utilization":12.0,"resets_at":"2100-01-01T00:00:00Z"}}"#,
    )
    .await;
    let endpoints = UsageEndpoints {
        claude_usage: format!("{}/api/oauth/usage", origin(&url)),
        ..UsageEndpoints::default()
    };

    let open = principal_read(&node, did, UsageTrigger::Open, None, &endpoints).await;
    assert_eq!(outcome(&open, None), &UsageRead::Read);
    assert_eq!(
        first_line(&handle.await.unwrap()),
        "GET /api/oauth/usage HTTP/1.1"
    );
    let again = principal_read(&node, did, UsageTrigger::Open, None, &endpoints).await;
    assert_eq!(outcome(&again, None), &UsageRead::SkippedRecent);
}

#[tokio::test]
async fn principal_read_reads_each_account_once() {
    let did = "did:key:z6MkUsageSurfOnce";
    let node = node().await;
    sign_in(&node, did, GROK, Utc::now() + Duration::hours(1), None).await;
    store_backends(
        &node,
        &[
            oauth_backend(did, "XaiGrokOAuth", "backend-usage-a", None, true),
            oauth_backend(did, "XaiGrokOAuth", "backend-usage-b", None, true),
        ],
    )
    .await;
    let (url, handle) = test_support::one_shot_token_server(200, GROK_BILLING_BODY).await;

    let reads = principal_read(&node, did, UsageTrigger::Open, None, &grok_endpoints(&url)).await;

    assert_eq!(reads.len(), 1, "{reads:?}");
    assert_eq!(outcome(&reads, None), &UsageRead::Read);
    handle.await.unwrap();
}

#[tokio::test]
async fn principal_read_disabled_backend_does_not_hide_the_account() {
    let did = "did:key:z6MkUsageSurfSharedBackend";
    let node = node().await;
    sign_in(&node, did, GROK, Utc::now() + Duration::hours(1), None).await;
    sign_in_ref(&node, did, GROK, "acct-g2", true).await;
    store_backends(
        &node,
        &[
            oauth_backend(did, "XaiGrokOAuth", "backend-usage-a", None, false),
            oauth_backend(did, "XaiGrokOAuth", "backend-usage-b", None, true),
            oauth_backend(
                did,
                "XaiGrokOAuth",
                "backend-usage-c",
                Some("acct-g2"),
                false,
            ),
        ],
    )
    .await;
    let url = crate::provider_http::tests::server_for(2, "200 OK", &[], GROK_BILLING_BODY).await;

    let reads = principal_read(&node, did, UsageTrigger::Open, None, &grok_endpoints(&url)).await;

    assert_eq!(reads.len(), 2, "{reads:?}");
    assert_eq!(outcome(&reads, None), &UsageRead::Read);
    assert_eq!(outcome(&reads, Some("acct-g2")), &UsageRead::Read);
}

#[tokio::test]
async fn principal_read_reads_an_enabled_account_no_backend_names() {
    let did = "did:key:z6MkUsageSurfNoBackend";
    let node = node().await;
    sign_in(&node, did, GROK, Utc::now() + Duration::hours(1), None).await;
    sign_in_ref(&node, did, GROK, "acct-g2", true).await;
    store_backends(
        &node,
        &[oauth_backend(
            did,
            "XaiGrokOAuth",
            "backend-usage-a",
            None,
            true,
        )],
    )
    .await;
    let url = crate::provider_http::tests::server_for(2, "200 OK", &[], GROK_BILLING_BODY).await;

    let reads = principal_read(&node, did, UsageTrigger::Open, None, &grok_endpoints(&url)).await;

    assert_eq!(reads.len(), 2, "{reads:?}");
    assert_eq!(outcome(&reads, None), &UsageRead::Read);
    assert_eq!(outcome(&reads, Some("acct-g2")), &UsageRead::Read);
}

#[test]
fn usage_account_for_backend_follows_principal_oauth() {
    let api_key = backend(
        A,
        "XaiGrokOAuth",
        "backend-usage-a",
        json!({ "kind": "api_key", "key": "key-SECRET" }),
    );
    assert_eq!(
        UsageAccount::for_backend(A, &api_key),
        UsageAccount::Backend {
            agent_did: A.to_string(),
            provider: "XaiGrokOAuth".to_string(),
            backend_id: "backend-usage-a".to_string(),
        }
    );
    let oauth = oauth_backend(
        A,
        "XaiGrokOAuth",
        "backend-usage-a",
        Some("acct-key-a"),
        true,
    );
    assert_eq!(
        UsageAccount::for_backend(A, &oauth),
        UsageAccount::Credential {
            agent_did: A.to_string(),
            provider: GROK.to_string(),
            account_ref: Some("acct-key-a".to_string()),
        }
    );
}

#[tokio::test]
async fn principal_read_skips_accounts_not_on_this_node_and_filters_by_provider() {
    let did = "did:key:z6MkUsageSurfFilter";
    let node = node().await;
    sign_in(&node, did, GROK, Utc::now() + Duration::hours(1), None).await;
    store_backends(
        &node,
        &[
            oauth_backend(did, "XaiGrokOAuth", "backend-usage-a", None, true),
            oauth_backend(
                did,
                "XaiGrokOAuth",
                "backend-usage-b",
                Some("acct-other"),
                true,
            ),
            InferenceBackend {
                auth: crate::document_config::BackendAuth::Unauthenticated,
                ..backend(
                    did,
                    "OpenAiCompatible",
                    "backend-usage-chat",
                    json!({ "kind": "api_key", "key": "key-SECRET" }),
                )
            },
        ],
    )
    .await;
    let (url, handle) = test_support::one_shot_token_server(200, GROK_BILLING_BODY).await;

    let reads = principal_read(&node, did, UsageTrigger::Open, None, &grok_endpoints(&url)).await;
    assert_eq!(reads.len(), 2, "{reads:?}");
    assert_eq!(outcome(&reads, None), &UsageRead::Read);
    let chat = reads
        .iter()
        .find(|read| read.backend_id.as_deref() == Some("backend-usage-chat"))
        .expect("account-free backend");
    assert_eq!(chat.outcome, UsageRead::NotReported);
    assert!(reads
        .iter()
        .all(|read| read.account_ref.as_deref() != Some("acct-other")));
    handle.await.unwrap();

    let claude_only = principal_read(
        &node,
        did,
        UsageTrigger::Open,
        Some(CLAUDE),
        &grok_endpoints(&url),
    )
    .await;
    assert!(claude_only.is_empty(), "{claude_only:?}");
}

#[tokio::test]
async fn principal_read_outcomes_hold_no_secrets() {
    let did = "did:key:z6MkUsageSurfSecrets";
    let node = node().await;
    sign_in(&node, did, GROK, Utc::now() + Duration::hours(1), None).await;
    store_backends(
        &node,
        &[oauth_backend(
            did,
            "XaiGrokOAuth",
            "backend-usage-a",
            None,
            true,
        )],
    )
    .await;
    let (url, handle) = test_support::one_shot_token_server(200, GROK_BILLING_BODY).await;

    let reads = principal_read(&node, did, UsageTrigger::Open, None, &grok_endpoints(&url)).await;

    assert_eq!(reads.len(), 1, "{reads:?}");
    handle.await.unwrap();
    let json = serde_json::to_string(&reads).unwrap();
    assert!(!json.contains("SECRET") && !json.contains("TEST"), "{json}");
    assert!(!json.contains("did:"), "{json}");
}

/// Account `acct-a` with usage under its fallback key (before its key was
/// known) and under its key, account `acct-b`, and an API-key backend.
async fn seed_remove_fixture(node: &Arc<EmbeddedNode>) -> (OAuthCredential, String) {
    let mut first = credential(A, Some("acct-a"), None);
    first.connected_at = Some(Utc::now());
    let doc_id = seed(node, &first).await;
    let account = UsageAccount::for_credential(&first);
    let now = Utc::now();
    record_usage(node, &account, report(vec![window("primary", 10.0, now)]))
        .await
        .unwrap();
    let keyed = OAuthCredential {
        provider_account_key: Some("acct-key-a".into()),
        ..first
    };
    seed(node, &keyed).await;
    record_usage(
        node,
        &account,
        report(vec![window("primary", 20.0, now + Duration::minutes(2))]),
    )
    .await
    .unwrap();
    let mut other = credential(A, Some("acct-b"), Some("acct-key-b"));
    other.connected_at = Some(Utc::now());
    seed(node, &other).await;
    record_usage(
        node,
        &UsageAccount::for_credential(&other),
        report(vec![window("primary", 30.0, now)]),
    )
    .await
    .unwrap();
    record_usage(
        node,
        &api_key_account(A),
        report(vec![window("requests", 5.0, now)]),
    )
    .await
    .unwrap();
    let fallback = format!("ref:acct-a:{doc_id}");
    let keys: Vec<_> = rows(node).await.into_iter().map(|row| row.2).collect();
    for key in [
        fallback.as_str(),
        "acct-key-a",
        "acct-key-b",
        "backend-usage-a",
    ] {
        assert!(keys.iter().any(|stored| stored == key), "{key}: {keys:?}");
    }
    (keyed, fallback)
}

async fn remove(node: &Arc<EmbeddedNode>, credential_id: &str) {
    local(node)
        .transact("test.remove_account", |txn| {
            Box::pin(async move {
                crate::oauth_credential::remove_account_in_txn(txn, A, credential_id).await
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn remove_deletes_the_accounts_usage_rows() {
    let node = node().await;
    let (removed, _) = seed_remove_fixture(&node).await;

    remove(&node, &removed.credential_id).await;

    let keys: Vec<_> = rows(&node).await.into_iter().map(|row| row.2).collect();
    assert_eq!(keys, ["backend-usage-a", "acct-key-b"]);
}

#[tokio::test]
async fn remove_of_a_disabled_account_deletes_its_usage_rows() {
    let node = node().await;
    let (removed, _) = seed_remove_fixture(&node).await;
    crate::oauth_credential::set_account_enabled(&local(&node), A, &removed.credential_id, false)
        .await
        .unwrap();

    remove(&node, &removed.credential_id).await;

    let keys: Vec<_> = rows(&node).await.into_iter().map(|row| row.2).collect();
    assert_eq!(keys, ["backend-usage-a", "acct-key-b"]);
}
