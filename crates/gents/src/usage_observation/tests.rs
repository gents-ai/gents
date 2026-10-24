use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use defra_node::EmbeddedNode;
use gents_loop::account_usage::{UsageReport, UsageSource, UsageWindow};
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
    test_support::test_node().await
}

fn local(node: &Arc<EmbeddedNode>) -> ConfigAccess {
    ConfigAccess::local(node.clone())
}

fn credential(node_did: &str, account_ref: Option<&str>, key: Option<&str>) -> OAuthCredential {
    OAuthCredential {
        doc_id: None,
        credential_id: match account_ref {
            Some(account_ref) => format!("{}:{account_ref}", oauth_credential_id(node_did, CODEX)),
            None => oauth_credential_id(node_did, CODEX),
        },
        node_did: node_did.to_string(),
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

fn backend(node_did: &str, provider_kind: &str, backend_id: &str, auth: Value) -> InferenceBackend {
    serde_json::from_value(json!({
        "node_did": node_did,
        "backend_id": backend_id,
        "name": backend_id,
        "provider_kind": provider_kind,
        "endpoint": "http://127.0.0.1:9/v1",
        "auth": auth,
    }))
    .expect("backend")
}

fn used(stored: &UsageObservation, label: &str) -> Option<f64> {
    stored
        .report
        .windows
        .iter()
        .find(|window| window.label == label)
        .map(|window| window.used_pct)
}

const CODEX_USAGE_BODY: &str = r#"{"plan_type":"plus","rate_limit":{"primary_window":{"used_percent":12,"limit_window_seconds":18000,"reset_at":4102444800}}}"#;
const OPENROUTER_KEY_BODY: &str =
    r#"{"data":{"limit":20,"limit_remaining":15,"limit_reset":"daily"}}"#;

async fn sign_in(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    provider: &str,
    expires_at: DateTime<Utc>,
    account_id: Option<&str>,
) {
    test_support::seed_credential(node, node_did, provider, expires_at).await;
    if let Some(account_id) = account_id {
        let mut row = resolve_oauth_credential(
            &local(node),
            node_did,
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

async fn read(
    cache: &UsageCache,
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    backend: &InferenceBackend,
    trigger: UsageTrigger,
    endpoints: &UsageEndpoints,
    now: DateTime<Utc>,
) -> UsageRead {
    read_account_usage(
        cache,
        node.clone(),
        node_did,
        backend,
        trigger,
        endpoints,
        now,
    )
    .await
    .expect("read")
}

async fn never_called(handle: tokio::task::JoinHandle<String>) {
    let mut handle = handle;
    let waited = tokio::time::timeout(std::time::Duration::from_millis(300), &mut handle).await;
    assert!(waited.is_err(), "the server was called: {waited:?}");
    handle.abort();
}

#[tokio::test]
async fn read_codex_sends_account_header_to_the_backend_host_and_stores_endpoint_windows() {
    let cache = UsageCache::default();
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
        json!({ "kind": "node_oauth" }),
    );
    for (step, suffix) in [(0, "/codex"), (1, "/codex/")] {
        let (url, handle) = test_support::one_shot_token_server(200, CODEX_USAGE_BODY).await;
        codex.endpoint = format!("{}{suffix}", origin(&url));
        let at = now + Duration::minutes(6 * step);
        assert_eq!(
            read(
                &cache,
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

    let stored = usage_for_backend(&cache, &local(&node), did, &codex)
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
    let cache = UsageCache::default();
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
        json!({ "kind": "node_oauth" }),
    );

    assert_eq!(
        read(
            &cache,
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
    let stored = usage_for_backend(&cache, &local(&node), did, &grok)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.report.windows[0].used_pct, 30.0);
}

#[tokio::test]
async fn read_openrouter_uses_the_backend_key_and_endpoint() {
    let cache = UsageCache::default();
    let node = node().await;
    let (url, handle) = test_support::one_shot_token_server(200, OPENROUTER_KEY_BODY).await;
    let openrouter = api_key_backend(
        format!("{}/api/v1", origin(&url)),
        "backend-usage-or",
        json!({ "kind": "api_key", "key": "sk-or-TEST" }),
    );

    assert_eq!(
        read(
            &cache,
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
    let stored = usage_for_backend(&cache, &local(&node), A, &openrouter)
        .await
        .unwrap()
        .unwrap();
    let daily = &stored.report.windows[0];
    assert_eq!((daily.label.as_str(), daily.used_pct), ("daily", 25.0));
}

#[tokio::test]
async fn read_claude_on_open_then_skips_a_recent_read() {
    let cache = UsageCache::default();
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
        json!({ "kind": "node_oauth" }),
    );

    assert_eq!(
        read(
            &cache,
            &node,
            did,
            &claude,
            UsageTrigger::Open,
            &endpoints,
            now
        )
        .await,
        UsageRead::Read
    );
    assert_eq!(
        first_line(&handle.await.unwrap()),
        "GET /api/oauth/usage HTTP/1.1"
    );
    assert_eq!(
        read(
            &cache,
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
    let cache = UsageCache::default();
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
        json!({ "kind": "node_oauth" }),
    );

    assert_eq!(
        read(
            &cache,
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
    let stored = usage_for_backend(&cache, &local(&node), did, &claude)
        .await
        .unwrap()
        .unwrap();
    let five = &stored.report.windows[0];
    assert_eq!((five.label.as_str(), five.used_pct), ("5h", 25.0));
}

#[tokio::test]
async fn read_expired_sign_in_refreshes_through_the_owner_then_reads() {
    let cache = UsageCache::default();
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
        json!({ "kind": "node_oauth" }),
    );
    codex.endpoint = format!("{}/codex", origin(&usage_url));

    let result = read(
        &cache,
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
    let stored = usage_for_backend(&cache, &local(&node), did, &codex)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.report.windows[0].used_pct, 12.0);
}

#[tokio::test]
async fn read_api_key_without_a_source_is_not_reported() {
    let cache = UsageCache::default();
    let node = node().await;
    let openai = backend(
        A,
        "OpenAiCompatible",
        "backend-usage-a",
        json!({ "kind": "api_key", "key": "sk-TEST" }),
    );
    assert_eq!(
        read(
            &cache,
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
async fn read_guard_recent_open_is_skipped_but_refresh_reads_again() {
    let cache = UsageCache::default();
    let node = node().await;
    let now = Utc::now();
    let (url, handle) = test_support::one_shot_token_server(200, OPENROUTER_KEY_BODY).await;
    let openrouter = api_key_backend(
        format!("{}/api/v1", origin(&url)),
        "backend-usage-recent",
        json!({"kind":"api_key", "key":"sk-or-TEST"}),
    );
    write(
        &cache,
        &node,
        &UsageAccount::for_backend(A, &openrouter),
        UsageReport::default(),
        Some((now, None)),
    )
    .await
    .unwrap();
    assert_eq!(
        read(
            &cache,
            &node,
            A,
            &openrouter,
            UsageTrigger::Open,
            &UsageEndpoints::default(),
            now + Duration::minutes(1)
        )
        .await,
        UsageRead::SkippedRecent
    );
    assert_eq!(
        read(
            &cache,
            &node,
            A,
            &openrouter,
            UsageTrigger::Refresh,
            &UsageEndpoints::default(),
            now + Duration::minutes(1)
        )
        .await,
        UsageRead::Read
    );
    assert_eq!(
        first_line(&handle.await.unwrap()),
        "GET /api/v1/key HTTP/1.1"
    );

    let did = "did:key:z6MkUsageReadRecent";
    sign_in(&node, did, CODEX, now - Duration::hours(1), None).await;
    let account = UsageAccount::Credential {
        doc_id: None,
        node_did: did.to_string(),
        provider: CODEX.to_string(),
        account_ref: None,
    };
    write(
        &cache,
        &node,
        &account,
        UsageReport::default(),
        Some((now, None)),
    )
    .await
    .unwrap();
    let _env = test_support::TOKEN_URL_ENV.lock().await;
    let (token_url, token_handle) = test_support::one_shot_token_server(200, "{}").await;
    std::env::set_var(REFRESH_ENV, &token_url);
    let codex = backend(
        did,
        "ChatGptCodex",
        "backend-usage-a",
        json!({ "kind": "node_oauth" }),
    );
    let result = read(
        &cache,
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
    let cache = UsageCache::default();
    let node = node().await;
    let now = Utc::now();
    let (url, handle) =
        test_support::one_shot_token_server(429, r#"{"error":"rate limited"}"#).await;
    let openrouter = api_key_backend(
        format!("{}/api/v1", origin(&url)),
        "backend-usage-throttled",
        json!({ "kind": "api_key", "key": "sk-or-TEST" }),
    );
    record_usage(
        &cache,
        &node,
        &UsageAccount::for_backend(A, &openrouter),
        report(vec![window("daily", 40.0, now - Duration::minutes(10))]),
    )
    .await
    .unwrap();

    assert_eq!(
        read(
            &cache,
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
        &cache,
        &local(&node),
        &UsageAccount::for_backend(A, &openrouter),
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
    let cache = UsageCache::default();
    let node = node().await;
    let (url, handle) = test_support::one_shot_token_server(200, r#"{"error":"x"}"#).await;
    let openrouter = api_key_backend(
        format!("{}/api/v1", origin(&url)),
        "backend-usage-malformed",
        json!({ "kind": "api_key", "key": "sk-or-TEST" }),
    );

    assert_eq!(
        read(
            &cache,
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
        &cache,
        &local(&node),
        &UsageAccount::for_backend(A, &openrouter),
    )
    .await
    .unwrap()
    .expect("the read is recorded");
    assert!(stored.report.is_empty(), "{stored:?}");
    assert_eq!(stored.read_error.as_deref(), Some("malformed"));
}

#[tokio::test]
async fn read_guard_refresh_failure_is_sign_in_expired() {
    let cache = UsageCache::default();
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
        json!({ "kind": "node_oauth" }),
    );
    codex.endpoint = format!("{}/codex", origin(&usage_url));

    let result = read(
        &cache,
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
    assert!(cache.entries.lock().await.is_empty());
}

#[tokio::test]
async fn read_guard_disabled_account_is_not_read() {
    let cache = UsageCache::default();
    let node = node().await;
    let mut openrouter = api_key_backend(
        "http://127.0.0.1:9/api/v1".to_string(),
        "backend-usage-disabled",
        json!({ "kind": "api_key", "key": "sk-or-TEST" }),
    );
    openrouter.enabled = false;
    assert_eq!(
        read(
            &cache,
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
        json!({ "kind": "node_oauth" }),
    );
    assert_eq!(
        read(
            &cache,
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
    assert!(cache.entries.lock().await.is_empty());
}

#[tokio::test]
async fn read_guard_unreachable_is_unavailable() {
    let cache = UsageCache::default();
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
            &cache,
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
    let cache = UsageCache::default();
    let node = node().await;
    let (url, handle) = test_support::one_shot_token_server(200, OPENROUTER_KEY_BODY).await;
    let openrouter = api_key_backend(
        format!("{}/api/v1", origin(&url)),
        "backend-usage-env",
        json!({ "kind": "environment", "variable": "GENTS_USAGE_TEST_UNSET_KEY" }),
    );
    assert_eq!(
        read(
            &cache,
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

const GROK: &str = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
const CLAUDE: &str = crate::claude_oauth::CLAUDE_OAUTH_PROVIDER;
const GROK_BILLING_BODY: &str = r#"{"config":{"creditUsagePercent":30,"currentPeriod":{"start":"2100-01-01T00:00:00Z","end":"2100-01-08T00:00:00Z"}}}"#;

async fn sign_in_ref(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    provider: &str,
    account_ref: &str,
    enabled: bool,
) {
    let mut row = credential(node_did, Some(account_ref), None);
    row.provider = provider.to_string();
    row.credential_id = format!("{}:{account_ref}", oauth_credential_id(node_did, provider));
    row.enabled = enabled;
    row.connected_at = Some(Utc::now());
    seed(node, &row).await;
}

fn oauth_backend(
    node_did: &str,
    kind: &str,
    backend_id: &str,
    account_ref: Option<&str>,
    enabled: bool,
) -> InferenceBackend {
    let mut backend = backend(
        node_did,
        kind,
        backend_id,
        json!({ "kind": "node_oauth", "account_ref": account_ref }),
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

async fn node_read(
    cache: &UsageCache,
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    trigger: UsageTrigger,
    provider: Option<&str>,
    endpoints: &UsageEndpoints,
) -> Vec<AccountUsageRead> {
    read_node_usage(
        cache,
        node.clone(),
        node_did,
        trigger,
        provider,
        endpoints,
        Utc::now(),
    )
    .await
    .expect("node read")
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
async fn node_read_skips_disabled_accounts_without_a_request() {
    let cache = UsageCache::default();
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

    let reads = node_read(
        &cache,
        &node,
        did,
        UsageTrigger::Open,
        None,
        &grok_endpoints(&url),
    )
    .await;

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
async fn node_read_open_reads_claude_once_within_the_window() {
    let cache = UsageCache::default();
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

    let open = node_read(&cache, &node, did, UsageTrigger::Open, None, &endpoints).await;
    assert_eq!(outcome(&open, None), &UsageRead::Read);
    assert_eq!(
        first_line(&handle.await.unwrap()),
        "GET /api/oauth/usage HTTP/1.1"
    );
    let again = node_read(&cache, &node, did, UsageTrigger::Open, None, &endpoints).await;
    assert_eq!(outcome(&again, None), &UsageRead::SkippedRecent);
}

#[tokio::test]
async fn node_read_reads_each_account_once() {
    let cache = UsageCache::default();
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

    let reads = node_read(
        &cache,
        &node,
        did,
        UsageTrigger::Open,
        None,
        &grok_endpoints(&url),
    )
    .await;

    assert_eq!(reads.len(), 1, "{reads:?}");
    assert_eq!(outcome(&reads, None), &UsageRead::Read);
    handle.await.unwrap();
}

#[tokio::test]
async fn node_read_disabled_backend_does_not_hide_the_account() {
    let cache = UsageCache::default();
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

    let reads = node_read(
        &cache,
        &node,
        did,
        UsageTrigger::Open,
        None,
        &grok_endpoints(&url),
    )
    .await;

    assert_eq!(reads.len(), 2, "{reads:?}");
    assert_eq!(outcome(&reads, None), &UsageRead::Read);
    assert_eq!(outcome(&reads, Some("acct-g2")), &UsageRead::Read);
}

#[tokio::test]
async fn node_read_reads_an_enabled_account_no_backend_names() {
    let cache = UsageCache::default();
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

    let reads = node_read(
        &cache,
        &node,
        did,
        UsageTrigger::Open,
        None,
        &grok_endpoints(&url),
    )
    .await;

    assert_eq!(reads.len(), 2, "{reads:?}");
    assert_eq!(outcome(&reads, None), &UsageRead::Read);
    assert_eq!(outcome(&reads, Some("acct-g2")), &UsageRead::Read);
}

#[test]
fn usage_account_for_backend_follows_node_oauth() {
    let api_key = backend(
        A,
        "XaiGrokOAuth",
        "backend-usage-a",
        json!({ "kind": "api_key", "key": "key-SECRET" }),
    );
    assert_eq!(
        UsageAccount::for_backend(A, &api_key),
        UsageAccount::Backend {
            node_did: A.to_string(),
            provider: "XaiGrokOAuth".to_string(),
            backend_id: "backend-usage-a".to_string(),
            connection: crate::admission::backend_connection_fingerprint(&api_key.backend_fields()),
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
            doc_id: None,
            node_did: A.to_string(),
            provider: GROK.to_string(),
            account_ref: Some("acct-key-a".to_string()),
        }
    );
}

#[tokio::test]
async fn node_read_skips_accounts_not_on_this_node_and_filters_by_provider() {
    let cache = UsageCache::default();
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

    let reads = node_read(
        &cache,
        &node,
        did,
        UsageTrigger::Open,
        None,
        &grok_endpoints(&url),
    )
    .await;
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

    let claude_only = node_read(
        &cache,
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
async fn node_read_outcomes_hold_no_secrets() {
    let cache = UsageCache::default();
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

    let reads = node_read(
        &cache,
        &node,
        did,
        UsageTrigger::Open,
        None,
        &grok_endpoints(&url),
    )
    .await;

    assert_eq!(reads.len(), 1, "{reads:?}");
    handle.await.unwrap();
    let json = serde_json::to_string(&reads).unwrap();
    assert!(!json.contains("SECRET") && !json.contains("TEST"), "{json}");
    assert!(!json.contains("did:"), "{json}");
}

#[test]
fn cached_open_and_refresh_follow_the_executable_model() {
    let snapshot: Value =
        serde_json::from_str(&gents_lean_contract::load_contract_json().unwrap()).unwrap();
    for case in snapshot["provider_limit_read_cases"].as_array().unwrap() {
        let trigger: UsageTrigger = serde_json::from_value(case["trigger"].clone()).unwrap();
        let now = DateTime::from_timestamp(case["now"].as_i64().unwrap(), 0).unwrap();
        let previous = case["previous"]
            .as_i64()
            .map(|time| DateTime::from_timestamp(time, 0).unwrap());
        assert_eq!(
            should_read(trigger, now, previous),
            case["should_read"].as_bool().unwrap(),
            "{case}"
        );
    }
}

#[tokio::test]
async fn cache_is_runtime_local_and_replacement_sign_ins_reject_late_responses() {
    let node = node().await;
    let cache = UsageCache::default();
    let row = credential(A, None, Some("same-provider-account"));
    seed(&node, &row).await;
    let access = local(&node);
    let original = resolve_oauth_credential(&access, A, CODEX, AccountPick::Reference(None))
        .await
        .unwrap()
        .unwrap();
    let account = UsageAccount::for_credential(&original);
    let now = Utc::now();
    record_usage(
        &cache,
        &node,
        &account,
        report(vec![window("primary", 12.0, now)]),
    )
    .await
    .unwrap();
    assert_eq!(
        used(
            &load_usage(&cache, &access, &account)
                .await
                .unwrap()
                .unwrap(),
            "primary"
        ),
        Some(12.0)
    );
    assert!(load_usage(&UsageCache::default(), &access, &account)
        .await
        .unwrap()
        .is_none());
    access
        .transact_write("test.remove", |txn| {
            let credential_id = row.credential_id.clone();
            Box::pin(async move {
                crate::oauth_credential::remove_account_in_txn(txn, A, &credential_id).await
            })
        })
        .await
        .unwrap();
    let replacement = crate::oauth_credential::store_sign_in(
        &access,
        credential(A, None, Some("same-provider-account")),
        Some("replacement"),
    )
    .await
    .unwrap()
    .credential;
    record_usage(
        &cache,
        &node,
        &account,
        report(vec![window("primary", 90.0, now)]),
    )
    .await
    .unwrap();
    assert!(
        load_usage(&cache, &access, &UsageAccount::for_credential(&replacement))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn newer_observations_win_and_backend_edits_get_an_empty_cache() {
    let node = node().await;
    let cache = UsageCache::default();
    let mut backend = backend(
        A,
        "OpenRouter",
        "account",
        json!({"kind":"api_key", "key":"first"}),
    );
    let old = UsageAccount::for_backend(A, &backend);
    let now = Utc::now();
    record_usage(
        &cache,
        &node,
        &old,
        report(vec![window("requests", 50.0, now)]),
    )
    .await
    .unwrap();
    record_usage(
        &cache,
        &node,
        &old,
        report(vec![window("requests", 5.0, now - Duration::seconds(1))]),
    )
    .await
    .unwrap();
    assert_eq!(
        used(
            &load_usage(&cache, &local(&node), &old)
                .await
                .unwrap()
                .unwrap(),
            "requests"
        ),
        Some(50.0)
    );
    let other_node = UsageAccount::for_backend(B, &backend);
    assert!(load_usage(&cache, &local(&node), &other_node)
        .await
        .unwrap()
        .is_none());
    backend.endpoint.push_str("/changed");
    assert!(usage_for_backend(&cache, &local(&node), A, &backend)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn default_limits_tool_reads_live_then_uses_cache_without_exposing_credentials() {
    use crate::llm::tool::Tool;
    let node = node().await;
    let cache = Arc::new(UsageCache::default());
    let row = credential(A, None, Some("provider-key-SECRET"));
    seed(&node, &row).await;
    record_usage(
        &cache,
        &node,
        &UsageAccount::for_credential(&row),
        report(vec![window("primary", 12.0, Utc::now())]),
    )
    .await
    .unwrap();
    let (url, received) = test_support::one_shot_token_server(200, CODEX_USAGE_BODY).await;
    let mut backend = backend(A, "ChatGptCodex", "runtime", json!({"kind":"node_oauth"}));
    backend.endpoint = origin(&url);
    crate::config_client::write_inference_backend_document(&local(&node), &backend)
        .await
        .unwrap();
    let tool = ProviderLimitsTool {
        node,
        node_did: A.into(),
        cache,
    };
    let output = tool.call(ProviderLimitsArgs {}).await.unwrap();
    received.await.unwrap();
    let cached = tool.call(ProviderLimitsArgs {}).await.unwrap();
    let cached: Value = serde_json::from_str(&cached).unwrap();
    assert_eq!(cached["accounts"][0]["read"]["outcome"], "skipped_recent");
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        value["accounts"][0]["limits"]["windows"][0]["used_pct"],
        12.0
    );
    for secret in [
        "SECRET",
        "account_ref",
        "credential_id",
        "access_token",
        "provider_account_key",
    ] {
        assert!(!output.contains(secret), "{output}");
    }
}
