use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use defra_node::Node as EmbeddedNode;
use gents_loop::account_usage::{UsageReport, UsageSource, UsageWindow};
use serde_json::{json, Value};

use crate::backends::registry::backend_fields;

use super::*;
use crate::auth::credential::{
    oauth_credential_id, resolve_oauth_credential, test_support, upsert_oauth_credential,
    AccountPick,
};

const A: &str = "did:key:z6MkUsageA";
const CODEX: &str = "chatgpt-codex";

fn local(node: &Arc<EmbeddedNode>) -> ConfigAccess {
    crate::auth::credential::runtime_credential_access(
        node,
        node.node_identity_did().expect("fixture runtime identity"),
    )
    .unwrap()
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
    let access = crate::auth::credential::runtime_credential_access(node, node_did).unwrap();
    test_support::seed_credential(&access, node_did, provider, expires_at).await;
    if let Some(account_id) = account_id {
        let mut row =
            resolve_oauth_credential(&access, node_did, provider, AccountPick::Reference(None))
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

fn api_key_backend(
    node_did: &str,
    endpoint: String,
    backend_id: &str,
    auth: Value,
) -> InferenceBackend {
    let mut backend = backend(node_did, "OpenRouter", backend_id, auth);
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
async fn runtime_usage_rejects_a_different_node_identity() {
    let (db, _user, _user_access) = crate::test_support::signed_test_node_without_agent(
        "usage-owner",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let other = crate::test_support::test_db(
        "usage-foreign-owner",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let foreign_did = other.node_identity.did();
    let cache = UsageCache::default();
    let foreign = UsageAccount::Backend {
        node_did: foreign_did.to_owned(),
        provider: "OpenRouter".into(),
        backend_id: "other".into(),
        connection: "other".into(),
    };
    let error = record_usage(&cache, &db.node, &foreign, UsageReport::default())
        .await
        .expect_err("a runtime cannot record another Node's usage");
    assert!(error.to_string().contains("configured runtime identity"));
    let error = read_node_usage(
        &cache,
        db.node.clone(),
        foreign_did,
        UsageTrigger::Open,
        None,
        &UsageEndpoints::default(),
        Utc::now(),
    )
    .await
    .expect_err("a runtime cannot read another Node's usage");
    assert!(error.to_string().contains("configured runtime identity"));
    assert!(cache.entries.lock().await.is_empty());
}

#[tokio::test]
async fn read_codex_sends_account_header_to_the_backend_host_and_stores_endpoint_windows() {
    let cache = UsageCache::default();
    let (db, _user, _user_access) = crate::test_support::signed_test_node_without_agent(
        "usage-observation",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did();
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

    let stored = usage_for_backend(
        &cache,
        &crate::auth::credential::runtime_credential_access(&node, did).unwrap(),
        did,
        &codex,
    )
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
    let (db, _user, _user_access) = crate::test_support::signed_test_node_without_agent(
        "usage-observation",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let provider = crate::clients::xai_grok::XAI_OAUTH_PROVIDER;
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
    let stored = usage_for_backend(
        &cache,
        &crate::auth::credential::runtime_credential_access(&node, did).unwrap(),
        did,
        &grok,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored.report.windows[0].used_pct, 30.0);
}

#[tokio::test]
async fn read_openrouter_uses_the_backend_key_and_endpoint() {
    let cache = UsageCache::default();
    let (db, _user, _user_access) = crate::test_support::signed_test_node_without_agent(
        "usage-observation",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let (url, handle) = test_support::one_shot_token_server(200, OPENROUTER_KEY_BODY).await;
    let openrouter = api_key_backend(
        did,
        format!("{}/api/v1", origin(&url)),
        "backend-usage-or",
        json!({ "kind": "api_key", "key": "sk-or-TEST" }),
    );

    assert_eq!(
        read(
            &cache,
            &node,
            did,
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
    let stored = usage_for_backend(&cache, &local(&node), did, &openrouter)
        .await
        .unwrap()
        .unwrap();
    let daily = &stored.report.windows[0];
    assert_eq!((daily.label.as_str(), daily.used_pct), ("daily", 25.0));
}

#[tokio::test]
async fn read_claude_on_refresh_sends_the_oauth_beta() {
    let cache = UsageCache::default();
    let (db, _user, _user_access) = crate::test_support::signed_test_node_without_agent(
        "usage-observation",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let provider = crate::auth::claude::CLAUDE_OAUTH_PROVIDER;
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
            crate::clients::claude_messages::OAUTH_BETA
        )),
        "{request}"
    );
    let stored = usage_for_backend(
        &cache,
        &crate::auth::credential::runtime_credential_access(&node, did).unwrap(),
        did,
        &claude,
    )
    .await
    .unwrap()
    .unwrap();
    let five = &stored.report.windows[0];
    assert_eq!((five.label.as_str(), five.used_pct), ("5h", 25.0));
}

#[tokio::test]
async fn read_expired_sign_in_refreshes_through_the_owner_then_reads() {
    let cache = UsageCache::default();
    let (db, _user, _user_access) = crate::test_support::signed_test_node_without_agent(
        "usage-observation",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did();
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
    let stored = usage_for_backend(
        &cache,
        &crate::auth::credential::runtime_credential_access(&node, did).unwrap(),
        did,
        &codex,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored.report.windows[0].used_pct, 12.0);
}

const REFRESH_ENV: &str = gents_protocol::chatgpt_oauth::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR;

#[tokio::test]
async fn read_guard_recent_open_is_skipped_but_refresh_reads_again() {
    let cache = UsageCache::default();
    let (db, _user, _user_access) = crate::test_support::signed_test_node_without_agent(
        "usage-observation",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let now = Utc::now();
    let (url, handle) = test_support::one_shot_token_server(200, OPENROUTER_KEY_BODY).await;
    let openrouter = api_key_backend(
        did,
        format!("{}/api/v1", origin(&url)),
        "backend-usage-recent",
        json!({"kind":"api_key", "key":"sk-or-TEST"}),
    );
    write(
        &cache,
        &node,
        &UsageAccount::for_backend(did, &openrouter),
        UsageReport::default(),
        Some((now, None)),
    )
    .await
    .unwrap();
    assert_eq!(
        read(
            &cache,
            &node,
            did,
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
            did,
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
async fn read_guard_refresh_failure_is_sign_in_expired() {
    let cache = UsageCache::default();
    let (db, _user, _user_access) = crate::test_support::signed_test_node_without_agent(
        "usage-observation",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did();
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
    let (db, _user, _user_access) = crate::test_support::signed_test_node_without_agent(
        "usage-observation",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let mut openrouter = api_key_backend(
        did,
        "http://127.0.0.1:9/api/v1".to_string(),
        "backend-usage-disabled",
        json!({ "kind": "api_key", "key": "sk-or-TEST" }),
    );
    openrouter.enabled = false;
    assert_eq!(
        read(
            &cache,
            &node,
            did,
            &openrouter,
            UsageTrigger::Refresh,
            &UsageEndpoints::default(),
            Utc::now()
        )
        .await,
        UsageRead::Disabled
    );

    let mut row = credential(did, None, Some("acct-key-a"));
    row.enabled = false;
    seed(&node, &row).await;
    let codex = backend(
        did,
        "ChatGptCodex",
        "backend-usage-a",
        json!({ "kind": "node_oauth" }),
    );
    assert_eq!(
        read(
            &cache,
            &node,
            did,
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

const GROK: &str = crate::clients::xai_grok::XAI_OAUTH_PROVIDER;
const CLAUDE: &str = crate::auth::claude::CLAUDE_OAUTH_PROVIDER;
const GROK_BILLING_BODY: &str = r#"{"config":{"creditUsagePercent":30,"currentPeriod":{"start":"2100-01-01T00:00:00Z","end":"2100-01-08T00:00:00Z"}}}"#;

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

async fn store_backends(access: &ConfigAccess, backends: &[InferenceBackend]) {
    for backend in backends {
        gents_documents::write_inference_backend_document(
            access,
            backend,
            &crate::resolved::ProviderProfilePolicy,
        )
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
            connection: crate::admission::backend_connection_fingerprint(&backend_fields(&api_key)),
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
async fn node_read_outcomes_hold_no_secrets() {
    let cache = UsageCache::default();
    let (db, _user, user_access) = crate::test_support::signed_test_node_without_agent(
        "usage-observation",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    sign_in(&node, did, GROK, Utc::now() + Duration::hours(1), None).await;
    store_backends(
        &user_access,
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
    gents_lean_vocab::families::provider_usage_limit_read::replay::<UsageTrigger>(|input| {
        should_read(
            input.trigger,
            DateTime::from_timestamp(i64::try_from(input.now).unwrap(), 0).unwrap(),
            input
                .previous
                .map(|time| DateTime::from_timestamp(i64::try_from(time).unwrap(), 0).unwrap()),
        )
    });
}

/// A node whose runtime identity may write calls AND whose agent signer may:
/// the `InferenceCall` collection is policy-gated on `children`, so the seed a
/// daemon would leave needs a signed agent, not the unsigned runtime actor.
async fn refusal_fixture() -> (
    Arc<EmbeddedNode>,
    String,
    ConfigAccess,
    gents_store::config_client::ConfigAccess,
    gents_store::identity::ManagedAgentSigner,
) {
    use gents_store::config_client::NodePermission;
    let (db, _user, user_access, binding) = crate::test_support::signed_test_node(
        "usage-observation",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did().to_owned();
    let agent = gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .expect("agent identity");
    let signer = agent.managed_agent_signer().expect("agent signer");
    let admin = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(&did).expect("node did"),
    );
    let actor = identity::Did::new(signer.did()).expect("agent did");
    for permission in [
        NodePermission::CollectionGet,
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DocumentDelete,
        NodePermission::DacRelationAdd,
        NodePermission::DacRelationDelete,
    ] {
        admin
            .grant_node_permission(&actor, permission)
            .await
            .expect("grant");
    }
    let agent_access = gents_store::config_client::ConfigAccess::local_with_agent_signer(
        node.clone(),
        actor,
        signer.clone(),
    )
    .expect("agent access");
    (node, did, user_access, agent_access, signer)
}

/// Leaves one failed call in the node's own durable record, the way the daemon
/// leaves it: the classifier's rendered text in `failure_reason`, and the tokens
/// it reported. The refusal has to be readable from that record alone.
struct SeedCall<'a> {
    call_id: &'a str,
    queued_at: &'a str,
    call_state: &'a str,
    /// The model the call ran on. Production always writes it, and a refusal is
    /// model-scoped, so a seed without it would be evidence about no model.
    model_name: &'a str,
    failure_reason: Option<&'a str>,
    provider_total_tokens: Option<u64>,
}

async fn seed_refused_call(
    access: &ConfigAccess,
    node_did: &str,
    backend_id: &str,
    call: SeedCall<'_>,
) -> String {
    let SeedCall {
        call_id,
        queued_at,
        call_state,
        model_name,
        failure_reason,
        provider_total_tokens,
    } = call;
    let model = gents_store::graphql::escape_graphql_string(model_name);
    let escape = gents_store::graphql::escape_graphql_string;
    let failure = failure_reason
        .map(|text| format!(r#", failure_reason: "{}""#, escape(text)))
        .unwrap_or_default();
    let tokens = provider_total_tokens
        .map(|total| format!(", provider_total_tokens: {total}"))
        .unwrap_or_default();
    let (call_id, request_id, request_doc) = (
        escape(call_id),
        escape(&format!("{call_id}-request")),
        escape(&format!("{call_id}-request-doc")),
    );
    let (backend_id, node_did, queued_at, call_state) = (
        escape(backend_id),
        escape(node_did),
        escape(queued_at),
        escape(call_state),
    );
    let response = access
        .write(
            "test.seed_refused_call",
            &format!(
                r#"mutation {{ add_InferenceCall(input: {{
                call_id: "{call_id}", runtime_instance_id: "usage-refusal-runtime",
                request_id: "{request_id}", request_doc_id: "{request_doc}",
                call_seq: 1, backend_id: "{backend_id}", agent_did: "{node_did}",
                node_did: "{node_did}", call_kind: "inference", attempt: 1,
                call_state: "{call_state}", model_name: "{model}",
                queued_at: "{queued_at}", priority: 0,
                queue_depth_at_enqueue: 0, controller_generation: 0,
                backend_config_fingerprint: "usage-refusal"{failure}{tokens}
            }}) {{ _docID }} }}"#,
            ),
        )
        .await
        .expect("seed call");
    gents_store::graphql::created_doc_id(&response, "InferenceCall").expect("call doc id")
}

/// Production writes every call's `parent` edge to its session, and the session
/// admits the agent that works it. That chain is what makes a call readable to
/// anyone other than the agent that signed it, so the seed has to build it too.
async fn seed_session_chain(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    call_doc_id: &str,
) {
    let escape = gents_store::graphql::escape_graphql_string;
    let response = access
        .write(
            "test.seed_session",
            &format!(
                r#"mutation {{ add_AgentSession(input: {{
                session_id: "{}", node_did: "{}", requester_did: "{}", agent_did: "{}",
                created_at: "2026-11-17T09:00:00Z"
            }}) {{ _docID }} }}"#,
                escape(session_id),
                escape(node_did),
                escape(node_did),
                escape(node_did),
            ),
        )
        .await
        .expect("seed session");
    let session_doc =
        gents_store::graphql::created_doc_id(&response, "AgentSession").expect("session doc id");
    access
        .add_resource_relationship(
            "InferenceCall",
            call_doc_id,
            "parent",
            "sessions",
            &session_doc,
        )
        .await
        .expect("call parent edge");
}

/// The provided refusal, in the words a Grok free-tier refusal uses: a stated
/// window and a stated counter, and no reset time at all.
/// xAI's refusal, **captured verbatim from the live node** on 2026-11-16
/// (2026-11-16T19:01:05Z, tool call `plugin-effect:bae-25641432…:7`). 339 bytes,
/// with the em dash and the `tokens (actual/limit):` shape xAI really sends.
const GROK_REFUSAL: &str = concat!(
    "agent stream failed: CompletionError: ProviderError: provider usage limit reached ",
    "(reset time not reported): You've used all the included free usage for model grok-4.7 ",
    "for now. Usage resets over a rolling 24-hour window — tokens (actual/limit): ",
    "691791/600000. Upgrade to a Grok subscription for higher limits: https://grok.com/supergrok",
);

/// The model the refusal names. xAI says "for model grok-4.7", and evidence is
/// scoped by model, so a call seeded without one is evidence about no model.
const GROK_MODEL: &str = "grok-4.7";

/// The item's whole point, at the read path: an account the provider refused is
/// reported as exhausted from the node's own durable record, with the fields
/// the provider stated and nothing invented for the fields it did not — and the
/// same record says so through every view, because there is one DTO.
#[tokio::test]
async fn a_refused_account_is_exhausted_from_evidence_and_says_so_everywhere() {
    let (node, did, user_access, agent_access, signer) = refusal_fixture().await;
    let cache = UsageCache::default().with_agent_signers(vec![signer.clone()]);
    let now = Utc::now();
    sign_in(&node, &did, GROK, now + Duration::hours(1), None).await;
    store_backends(
        &user_access,
        &[oauth_backend(
            &did,
            "XaiGrokOAuth",
            "backend-usage-a",
            None,
            true,
        )],
    )
    .await;
    let call_doc = seed_refused_call(
        &agent_access,
        &did,
        "backend-usage-a",
        SeedCall {
            call_id: "usage-refusal-call",
            queued_at: &(now - Duration::minutes(30)).to_rfc3339(),
            call_state: "failed",
            // The model xAI's refusal names: evidence is scoped by it.
            model_name: GROK_MODEL,
            failure_reason: Some(GROK_REFUSAL),
            provider_total_tokens: Some(1200),
        },
    )
    .await;
    seed_session_chain(&agent_access, &did, "usage-refusal-session", &call_doc).await;
    let (url, handle) = test_support::one_shot_token_server(200, GROK_BILLING_BODY).await;

    let reads = node_read(
        &cache,
        &node,
        &did,
        UsageTrigger::Open,
        None,
        &grok_endpoints(&url),
    )
    .await;
    handle.await.unwrap();

    let read = reads.first().expect("one account");
    let refusal = read.refusal.as_ref().expect("the refusal is evidenced");
    assert_eq!(refusal.window.as_deref(), Some("rolling 24-hour"));
    // A counter on a cap below what the provider reported is what the provider
    // said, so both numbers are kept rather than reconciled into a percentage.
    assert_eq!((refusal.used, refusal.cap), (Some(691_791), Some(600_000)));
    assert!(refusal.resets_at.is_none(), "no reset was stated");
    assert_eq!(refusal.consumed_by_node, Some(1200));
    assert_eq!(
        refusal.window_provenance(),
        crate::usage_refusal::WindowProvenance::ProviderReported
    );
    // The percentage side is untouched: a refusal does not ride a window.
    let usage = read.usage.as_ref().expect("usage view");
    assert!(
        usage.windows.iter().all(|window| window.used_pct != 100.0),
        "a refusal must not appear as a synthetic 100%: {usage:?}"
    );

    // The same DTO is what both agent-facing reads render, so both say it.
    use gents_loop::tool::Tool as _;
    let tool = ProviderLimitsTool {
        node: node.clone(),
        node_did: did.clone(),
        cache: Arc::new(UsageCache::default().with_agent_signers(vec![signer])),
    };
    let output: Value =
        serde_json::from_str(&tool.call(ProviderLimitsArgs {}).await.unwrap()).expect("tool json");
    assert_eq!(
        output["accounts"][0]["refusal"]["window"],
        "rolling 24-hour"
    );
    assert_eq!(output["accounts"][0]["refusal"]["used"], 691_791);
    assert_eq!(output["accounts"][0]["refusal"]["cap"], 600_000);
    assert_eq!(
        output["accounts"][0]["refusal"]["window_basis"],
        "provider_reported"
    );
    // The parsed fields and nothing quoted: the bounded excerpt is the
    // operator's (the shell and the desktop), never a model's.
    assert!(
        output["accounts"][0]["refusal"].get("detail").is_none(),
        "no excerpt reaches a model: {output}"
    );
    let rendered = output.to_string();
    assert!(
        !rendered.contains("free usage"),
        "no fragment of the provider's body reaches a model: {rendered}"
    );
}

/// A restart empties the in-memory cache; the refusal does not go with it,
/// because it is derived from the durable record rather than remembered.
#[tokio::test]
async fn a_restart_with_an_empty_cache_still_shows_the_refusal() {
    let (node, did, user_access, agent_access, signer) = refusal_fixture().await;
    let now = Utc::now();
    sign_in(&node, &did, GROK, now + Duration::hours(1), None).await;
    store_backends(
        &user_access,
        &[oauth_backend(
            &did,
            "XaiGrokOAuth",
            "backend-usage-a",
            None,
            true,
        )],
    )
    .await;
    seed_refused_call(
        &agent_access,
        &did,
        "backend-usage-a",
        SeedCall {
            call_id: "usage-refusal-call",
            queued_at: &(now - Duration::minutes(30)).to_rfc3339(),
            call_state: "failed",
            // The model xAI's refusal names: evidence is scoped by it.
            model_name: GROK_MODEL,
            failure_reason: Some(GROK_REFUSAL),
            provider_total_tokens: None,
        },
    )
    .await;

    let (url, handle) = test_support::one_shot_token_server(200, GROK_BILLING_BODY).await;
    let reads = node_read(
        &UsageCache::default().with_agent_signers(vec![signer.clone()]),
        &node,
        &did,
        UsageTrigger::Open,
        None,
        &grok_endpoints(&url),
    )
    .await;
    handle.await.unwrap();
    assert!(
        reads[0].refusal.is_some(),
        "the record outlives the cache: {reads:?}"
    );
}

/// A completion after the refusal means the account served again, so the
/// refusal no longer stands even though the failed call is still recorded.
#[tokio::test]
async fn a_later_completed_call_clears_the_refusal_at_the_read_path() {
    let cache = UsageCache::default();
    let (node, did, user_access, agent_access, _signer) = refusal_fixture().await;
    let now = Utc::now();
    sign_in(&node, &did, GROK, now + Duration::hours(1), None).await;
    store_backends(
        &user_access,
        &[oauth_backend(
            &did,
            "XaiGrokOAuth",
            "backend-usage-a",
            None,
            true,
        )],
    )
    .await;
    seed_refused_call(
        &agent_access,
        &did,
        "backend-usage-a",
        SeedCall {
            call_id: "usage-refusal-call",
            queued_at: &(now - Duration::minutes(40)).to_rfc3339(),
            call_state: "failed",
            // The model xAI's refusal names: evidence is scoped by it.
            model_name: GROK_MODEL,
            failure_reason: Some(GROK_REFUSAL),
            provider_total_tokens: None,
        },
    )
    .await;
    seed_refused_call(
        &agent_access,
        &did,
        "backend-usage-a",
        SeedCall {
            call_id: "usage-clearing-call",
            queued_at: &(now - Duration::minutes(10)).to_rfc3339(),
            call_state: "completed",
            // Same model as the refusal: a sibling model's success must not
            // clear it, so this fixture proves the same-model clear.
            model_name: GROK_MODEL,
            failure_reason: None,
            provider_total_tokens: None,
        },
    )
    .await;
    let (url, handle) = test_support::one_shot_token_server(200, GROK_BILLING_BODY).await;
    let reads = node_read(
        &cache,
        &node,
        &did,
        UsageTrigger::Open,
        None,
        &grok_endpoints(&url),
    )
    .await;
    handle.await.unwrap();
    assert!(reads[0].refusal.is_none(), "{reads:?}");
}

/// Evidence outside the analytical window is not the refusal the node acts on,
/// and the window it used is stated as its own choice rather than the
/// provider's, because the provider named none.
#[tokio::test]
async fn evidence_older_than_the_window_is_not_a_standing_refusal() {
    let (node, did, user_access, agent_access, signer) = refusal_fixture().await;
    let now = Utc::now();
    sign_in(&node, &did, GROK, now + Duration::hours(1), None).await;
    store_backends(
        &user_access,
        &[oauth_backend(
            &did,
            "XaiGrokOAuth",
            "backend-usage-a",
            None,
            true,
        )],
    )
    .await;
    seed_refused_call(
        &agent_access,
        &did,
        "backend-usage-a",
        SeedCall {
            call_id: "usage-stale-call",
            queued_at: &(now - Duration::hours(30)).to_rfc3339(),
            call_state: "failed",
            model_name: GROK_MODEL,
            failure_reason: Some("you have used all the included free usage"),
            provider_total_tokens: None,
        },
    )
    .await;
    let (url, handle) = test_support::one_shot_token_server(200, GROK_BILLING_BODY).await;
    let reads = node_read(
        &UsageCache::default().with_agent_signers(vec![signer.clone()]),
        &node,
        &did,
        UsageTrigger::Open,
        None,
        &grok_endpoints(&url),
    )
    .await;
    handle.await.unwrap();
    assert!(reads[0].refusal.is_none(), "outside the window: {reads:?}");
}

fn used_u64(stored: Option<UsageObservation>, label: &str) -> Option<u64> {
    stored
        .and_then(|stored| used(&stored, label))
        .map(|pct| pct as u64)
}

#[tokio::test]
async fn usage_cache_runtime_replacement_follows_the_lean_model() {
    for case in gents_lean_vocab::families::usage_cache_runtime_replacement::cases() {
        let (db, _user, _user_access) = crate::test_support::signed_test_node_without_agent(
            "usage-observation",
            gents_store::node_extension::Extensions::none(),
        )
        .await;
        let node = db.node.clone();
        let did = db.node_identity.did();
        let cache = UsageCache::default();
        let row = credential(did, None, Some("same-provider-account"));
        seed(&node, &row).await;
        let access = local(&node);
        let original = resolve_oauth_credential(&access, did, CODEX, AccountPick::Reference(None))
            .await
            .unwrap()
            .unwrap();
        let account = UsageAccount::for_credential(&original);
        let now = Utc::now();
        record_usage(
            &cache,
            &node,
            &account,
            report(vec![window("primary", case.input.used as f64, now)]),
        )
        .await
        .unwrap();
        let same_runtime = used_u64(
            load_usage(&cache, &access, &account).await.unwrap(),
            "primary",
        );
        let other_runtime = used_u64(
            load_usage(&UsageCache::default(), &access, &account)
                .await
                .unwrap(),
            "primary",
        );
        access
            .transact_write("test.remove", |txn| {
                let credential_id = row.credential_id.clone();
                Box::pin(async move {
                    crate::auth::credential::remove_account_in_txn(txn, did, &credential_id).await
                })
            })
            .await
            .unwrap();
        let replacement = crate::auth::credential::store_sign_in(
            &access,
            credential(did, None, Some("same-provider-account")),
            Some("replacement"),
        )
        .await
        .unwrap()
        .credential;
        record_usage(
            &cache,
            &node,
            &account,
            report(vec![window(
                "primary",
                case.input.late as f64,
                now + Duration::seconds(1),
            )]),
        )
        .await
        .unwrap();
        let replacement = used_u64(
            load_usage(&cache, &access, &UsageAccount::for_credential(&replacement))
                .await
                .unwrap(),
            "primary",
        );
        case.check(
            gents_lean_vocab::families::usage_cache_runtime_replacement::ReplacementOutput {
                same_runtime,
                other_runtime,
                replacement,
            },
        );
    }
}

#[test]
fn a_write_lands_only_on_the_credential_row_it_was_read_for() {
    gents_lean_vocab::families::usage_write_binding::replay(|input| {
        let mut row = credential(A, None, None);
        row.doc_id = Some(input.current.clone());
        credential_target(A, CODEX, input.observed.as_deref(), row)
            .unwrap()
            .is_some()
    });
}

#[tokio::test]
async fn usage_cache_runtime_newer_follows_the_lean_model() {
    for case in gents_lean_vocab::families::usage_cache_runtime_newer::cases() {
        let (db, _user, _user_access) = crate::test_support::signed_test_node_without_agent(
            "usage-observation",
            gents_store::node_extension::Extensions::none(),
        )
        .await;
        let node = db.node.clone();
        let did = db.node_identity.did();
        let other_db = crate::test_support::test_db(
            "usage-other-node",
            gents_store::node_extension::Extensions::none(),
        )
        .await;
        let other_did = other_db.node_identity.did();
        let cache = UsageCache::default();
        let mut backend = backend(
            did,
            "OpenRouter",
            "account",
            json!({"kind":"api_key", "key":"first"}),
        );
        let old = UsageAccount::for_backend(did, &backend);
        let now = Utc::now();
        let newest = (now, 50.0);
        let oldest = (now - Duration::seconds(1), 5.0);
        let writes = if case.input.newer_first {
            [newest, oldest]
        } else {
            [oldest, newest]
        };
        for (observed_at, used_pct) in writes {
            record_usage(
                &cache,
                &node,
                &old,
                report(vec![window("requests", used_pct, observed_at)]),
            )
            .await
            .unwrap();
        }
        let used_value = used_u64(
            load_usage(&cache, &local(&node), &old).await.unwrap(),
            "requests",
        );
        let other_node = UsageAccount::for_backend(other_did, &backend);
        let other_node_value = used_u64(
            load_usage(&cache, &local(&node), &other_node)
                .await
                .unwrap(),
            "requests",
        );
        backend.endpoint.push_str("/changed");
        let after_edit = used_u64(
            usage_for_backend(&cache, &local(&node), did, &backend)
                .await
                .unwrap(),
            "requests",
        );
        case.check(
            gents_lean_vocab::families::usage_cache_runtime_newer::NewerOutput {
                used: used_value,
                other_node: other_node_value,
                after_edit,
            },
        );
    }
}

#[tokio::test]
async fn default_limits_tool_reads_live_then_uses_cache_without_exposing_credentials() {
    use gents_loop::tool::Tool;
    let (db, _user, user_access) = crate::test_support::signed_test_node_without_agent(
        "usage-observation",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let cache = Arc::new(UsageCache::default());
    let row = credential(did, None, Some("provider-key-SECRET"));
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
    let mut backend = backend(did, "ChatGptCodex", "runtime", json!({"kind":"node_oauth"}));
    backend.endpoint = origin(&url);
    gents_documents::write_inference_backend_document(
        &user_access,
        &backend,
        &crate::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    let tool = ProviderLimitsTool {
        node,
        node_did: did.into(),
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

#[tokio::test]
async fn a_rotation_feed_folds_response_headers_into_the_account_pool() {
    use gents_claude::rotation::{Family, Policy};
    let cache = Arc::new(UsageCache::default());
    let node = crate::auth::credential::test_support::test_node().await;
    let account = UsageAccount::Backend {
        node_did: "did".into(),
        provider: "claude".into(),
        backend_id: "b".into(),
        connection: "c".into(),
    };
    let reporter = UsageReporter::new(cache.clone(), node, account).with_rotation(RotationFeed {
        credential_id: "id-a".into(),
        family: Family::Sonnet,
    });
    let mut headers = HeaderMap::new();
    headers.insert(
        "anthropic-ratelimit-unified-status",
        "rejected".parse().unwrap(),
    );
    headers.insert(
        "anthropic-ratelimit-unified-reset",
        "4102444800".parse().unwrap(),
    );
    headers.insert(
        "anthropic-ratelimit-unified-representative-claim",
        "five_hour".parse().unwrap(),
    );
    reporter.observe(&headers, UsageSource::Error);
    let ids = ["id-a".to_string()];
    let now = now_unix_secs();
    assert!(cache
        .claude_accounts()
        .choose(&ids, "s", Family::Sonnet, now, &Policy::default())
        .is_none());
}

#[path = "tests/scenarios.rs"]
mod scenarios;
