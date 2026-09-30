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
    seed(&node, &original).await;
    seed(&node, &second).await;
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
    assert_eq!(keys, vec!["ref:acct-ref-2", "ref:original"]);
    assert!(keys.iter().all(|key| !key.contains("did:")));
}

#[tokio::test]
async fn store_key_is_read_when_the_write_happens() {
    let node = node().await;
    let mut row = credential(A, None, None);
    seed(&node, &row).await;
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
    assert_eq!(keys, vec!["acct-key-a", "ref:original"]);
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
