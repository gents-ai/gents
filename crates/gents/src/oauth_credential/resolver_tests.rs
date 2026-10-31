use super::test_support::test_node;
use super::*;

const DID: &str = "did:key:zResolver";
const PROVIDER: &str = "xai-oauth";

fn row(
    node_did: &str,
    provider: &str,
    credential_id: &str,
    account_ref: Option<&str>,
    connected_at: Option<i64>,
    enabled: bool,
) -> OAuthCredential {
    OAuthCredential {
        doc_id: None,
        credential_id: credential_id.to_string(),
        node_did: node_did.to_string(),
        provider: provider.to_string(),
        access_token: "access-TEST".to_string(),
        refresh_token: "refresh-TEST".to_string(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: DateTime::<Utc>::from_timestamp(1_900_000_000, 0).unwrap(),
        last_refresh: None,
        enabled,
        account_ref: account_ref.map(str::to_string),
        connected_at: connected_at.map(|secs| DateTime::<Utc>::from_timestamp(secs, 0).unwrap()),
        provider_account_key: None,
        label: None,
    }
}

fn picked<'c>(rows: &'c [OAuthCredential], pick: AccountPick<'_>) -> Option<&'c str> {
    pick_oauth_credential(rows, DID, PROVIDER, pick).map(|row| row.credential_id.as_str())
}

#[test]
fn resolver_picks_the_earliest_connected_enabled_account() {
    let rows = [
        row(DID, PROVIDER, "late", Some("late"), Some(2_000), true),
        row(DID, PROVIDER, "early", Some("early"), Some(1_000), true),
    ];
    assert_eq!(picked(&rows, AccountPick::ProviderDefault), Some("early"));
}

#[test]
fn resolver_skips_a_disabled_account() {
    let rows = [
        row(DID, PROVIDER, "off", Some("off"), Some(500), false),
        row(DID, PROVIDER, "on", Some("on"), Some(1_000), true),
    ];
    assert_eq!(picked(&rows, AccountPick::ProviderDefault), Some("on"));
}

#[test]
fn resolver_sorts_a_missing_connection_time_first() {
    let rows = [
        row(DID, PROVIDER, "timed", Some("timed"), Some(1_000), true),
        row(DID, PROVIDER, "untimed", None, None, true),
    ];
    assert_eq!(picked(&rows, AccountPick::ProviderDefault), Some("untimed"));
}

#[test]
fn resolver_breaks_equal_times_by_credential_id() {
    let rows = [
        row(DID, PROVIDER, "b", Some("b"), Some(1_000), true),
        row(DID, PROVIDER, "a", Some("a"), Some(1_000), true),
    ];
    assert_eq!(picked(&rows, AccountPick::ProviderDefault), Some("a"));
}

#[test]
fn resolver_never_returns_another_node_or_provider() {
    let rows = [
        row("did:key:zOther", PROVIDER, "other-did", None, None, true),
        row(DID, "chatgpt-codex", "other-provider", None, None, true),
        row(DID, PROVIDER, "mine", None, Some(1_000), true),
    ];
    assert_eq!(picked(&rows, AccountPick::ProviderDefault), Some("mine"));
    let foreign = [row("did:key:zOther", PROVIDER, "b", Some("b"), None, true)];
    assert_eq!(picked(&foreign, AccountPick::Reference(Some("b"))), None);
}

#[test]
fn resolver_returns_nothing_when_no_account_is_enabled() {
    let rows = [row(DID, PROVIDER, "off", None, None, false)];
    assert_eq!(picked(&rows, AccountPick::ProviderDefault), None);
    assert_eq!(picked(&rows, AccountPick::Reference(None)), None);
}

#[test]
fn resolver_reference_names_exactly_one_account() {
    let rows = [
        row(DID, PROVIDER, "earlier", Some("earlier"), Some(1_000), true),
        row(DID, PROVIDER, "original", None, Some(2_000), true),
        row(DID, PROVIDER, "b", Some("b"), Some(3_000), true),
    ];
    assert_eq!(
        picked(&rows, AccountPick::Reference(None)),
        Some("original")
    );
    assert_eq!(picked(&rows, AccountPick::Reference(Some("b"))), Some("b"));
    assert_eq!(picked(&rows, AccountPick::ProviderDefault), Some("earlier"));

    let disabled = [
        row(DID, PROVIDER, "original", None, None, true),
        row(DID, PROVIDER, "b", Some("b"), Some(1_000), false),
    ];
    assert_eq!(picked(&disabled, AccountPick::Reference(Some("b"))), None);
}

#[tokio::test]
async fn resolver_reads_through_config_access() {
    let node = test_node().await;
    for credential in [
        row(DID, PROVIDER, "xai-oauth:b", Some("b"), Some(1_000), true),
        row(DID, PROVIDER, "xai-oauth:original", None, None, true),
    ] {
        upsert_oauth_credential(&node, &credential).await.unwrap();
    }
    let access = crate::config_client::ConfigAccess::local(node);
    for (pick, expected) in [
        (AccountPick::Reference(Some("b")), "xai-oauth:b"),
        (AccountPick::Reference(None), "xai-oauth:original"),
        (AccountPick::ProviderDefault, "xai-oauth:original"),
    ] {
        let resolved = resolve_oauth_credential(&access, DID, PROVIDER, pick)
            .await
            .unwrap()
            .map(|row| row.credential_id);
        assert_eq!(resolved.as_deref(), Some(expected), "{pick:?}");
    }
}

#[tokio::test]
async fn resolver_missing_account_keeps_the_missing_guidance() {
    let node = test_node().await;
    let missing = classify_oauth_auth_error(
        &XAI_OAUTH_PRODUCT,
        DID,
        PROVIDER,
        &OAuthAuthProblem::Missing,
    );
    let bootstrap = |pick| {
        crate::oauth_http::bootstrap_oauth_client(
            node.clone(),
            DID,
            PROVIDER,
            OAuthRefreshKind::Xai,
            XAI_OAUTH_PRODUCT,
            pick,
        )
    };
    let Err(error) = bootstrap(AccountPick::ProviderDefault).await else {
        panic!("no account is stored");
    };
    assert_eq!(error.to_string(), missing);

    upsert_oauth_credential(
        &node,
        &row(DID, PROVIDER, "xai-oauth:original", None, None, true),
    )
    .await
    .unwrap();
    let Err(error) = bootstrap(AccountPick::Reference(Some("b"))).await else {
        panic!("a reference never falls back to the original account");
    };
    assert_eq!(error.to_string(), missing);
}
