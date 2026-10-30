use defra_node::EmbeddedNode;
use serde_json::{json, Value};
use std::sync::Arc;

use super::test_support::{one_shot_token_server, test_node, unsigned_jwt, TOKEN_URL_ENV};
use super::*;
use crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER;
use crate::config_client::ConfigAccess;
use crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;

const XAI_ENV: &str = crate::xai_oauth_refresh::XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV;
const CHATGPT_ENV: &str = gents_protocol::chatgpt_oauth::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR;

fn token_response(access_claims: Value) -> &'static str {
    let body = json!({
        "access_token": unsigned_jwt(access_claims),
        "refresh_token": "refresh-rotated",
        "expires_in": 900
    });
    Box::leak(body.to_string().into_boxed_str())
}

async fn seed_expired_grok(node: &Arc<EmbeddedNode>, did: &str, key: Option<&str>) {
    let credential = OAuthCredential {
        doc_id: None,
        credential_id: oauth_credential_id(did, XAI_OAUTH_PROVIDER),
        node_did: did.to_string(),
        provider: XAI_OAUTH_PROVIDER.to_string(),
        access_token: "access-TEST".into(),
        refresh_token: "refresh-TEST".into(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: Utc::now() - Duration::minutes(1),
        last_refresh: None,
        enabled: true,
        account_ref: None,
        connected_at: None,
        provider_account_key: key.map(str::to_owned),
        label: None,
    };
    upsert_oauth_credential(node, &credential).await.unwrap();
}

async fn refresh_once(
    node: &Arc<EmbeddedNode>,
    did: &str,
    provider: &str,
    kind: OAuthRefreshKind,
    body: &'static str,
) -> OAuthCredential {
    let (env, product) = match kind {
        OAuthRefreshKind::ChatGpt => (CHATGPT_ENV, CHATGPT_OAUTH_PRODUCT),
        _ => (XAI_ENV, XAI_OAUTH_PRODUCT),
    };
    let credential_id = oauth_credential_id(did, provider);
    let _env = TOKEN_URL_ENV.lock().await;
    let (url, handle) = one_shot_token_server(200, body).await;
    std::env::set_var(env, &url);
    let bearer = DbCredentialBearer::new(
        node.clone(),
        did,
        provider,
        credential_id.clone(),
        true,
        kind,
        product,
    );
    let refreshed = bearer.current_bearer().await;
    std::env::remove_var(env);
    handle.await.expect("server");
    refreshed.expect("refresh");
    lookup_oauth_credential_by_id(node, &credential_id)
        .await
        .unwrap()
        .expect("stored row")
}

#[tokio::test]
async fn refresh_fills_an_empty_key_from_the_refreshed_claims() {
    let node = test_node().await;
    let did = "did:key:zBackfillFill";
    seed_expired_grok(&node, did, None).await;
    let body = token_response(json!({ "principal_type": "user", "principal_id": "principal-1" }));
    let stored = refresh_once(&node, did, XAI_OAUTH_PROVIDER, OAuthRefreshKind::Xai, body).await;
    assert_eq!(stored.refresh_token, "refresh-rotated");
    assert_eq!(
        stored.provider_account_key.as_deref(),
        Some("user:principal-1")
    );
}

#[tokio::test]
async fn refresh_never_changes_a_present_key() {
    let node = test_node().await;
    let did = "did:key:zBackfillPresent";
    seed_expired_grok(&node, did, Some("user:principal-old")).await;
    let body = token_response(json!({ "principal_type": "user", "principal_id": "principal-1" }));
    let stored = refresh_once(&node, did, XAI_OAUTH_PROVIDER, OAuthRefreshKind::Xai, body).await;
    assert_eq!(stored.refresh_token, "refresh-rotated");
    assert_eq!(
        stored.provider_account_key.as_deref(),
        Some("user:principal-old")
    );
}

#[tokio::test]
async fn refresh_leaves_the_key_empty_without_the_claim() {
    let node = test_node().await;
    let did = "did:key:zBackfillNoClaim";
    seed_expired_grok(&node, did, None).await;
    let body = token_response(json!({ "sub": "user-a" }));
    let stored = refresh_once(&node, did, XAI_OAUTH_PROVIDER, OAuthRefreshKind::Xai, body).await;
    assert_eq!(stored.refresh_token, "refresh-rotated");
    assert_eq!(stored.provider_account_key, None);
}

async fn refresh_racing<F>(node: &Arc<EmbeddedNode>, did: &str, change: F) -> DbCredentialBearer
where
    F: std::future::Future<Output = ()>,
{
    let body = token_response(json!({ "principal_type": "user", "principal_id": "principal-1" }));
    let _env = TOKEN_URL_ENV.lock().await;
    let (received_tx, received) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let (url, handle) =
        super::test_support::gated_token_server(200, body, Some((received_tx, release_rx))).await;
    std::env::set_var(XAI_ENV, &url);
    let bearer = DbCredentialBearer::new(
        node.clone(),
        did,
        XAI_OAUTH_PROVIDER,
        oauth_credential_id(did, XAI_OAUTH_PROVIDER),
        true,
        OAuthRefreshKind::Xai,
        XAI_OAUTH_PRODUCT,
    );
    let (refreshed, ()) = tokio::join!(bearer.current_bearer(), async {
        received.await.expect("the owner asked for a refresh");
        change.await;
        release.send(()).expect("server waits");
    });
    std::env::remove_var(XAI_ENV);
    handle.await.expect("server");
    refreshed.expect("the owner still serves its refreshed token");
    bearer
}

#[tokio::test]
async fn a_refresh_racing_a_remove_does_not_revive_the_row() {
    let node = test_node().await;
    let did = "did:key:z6MkTestRaceRemove";
    seed_expired_grok(&node, did, None).await;
    let credential_id = oauth_credential_id(did, XAI_OAUTH_PROVIDER);
    refresh_racing(&node, did, async {
        ConfigAccess::write_local(
            &node,
            "test.remove",
            &format!(
                r#"mutation {{ delete_OAuthCredential(filter: {{ credential_id: {{ _eq: "{credential_id}" }} }}) {{ _docID }} }}"#
            ),
        )
        .await
        .unwrap();
    })
    .await;
    assert_eq!(
        lookup_oauth_credential_by_id(&node, &credential_id)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_refresh_racing_a_disable_keeps_it_disabled() {
    let node = test_node().await;
    let did = "did:key:z6MkTestRaceDisable";
    seed_expired_grok(&node, did, None).await;
    let credential_id = oauth_credential_id(did, XAI_OAUTH_PROVIDER);
    refresh_racing(&node, did, async {
        ConfigAccess::write_local(
            &node,
            "test.disable",
            &format!(
                r#"mutation {{ update_OAuthCredential(filter: {{ credential_id: {{ _eq: "{credential_id}" }} }}, input: {{ enabled: false }}) {{ _docID }} }}"#
            ),
        )
        .await
        .unwrap();
    })
    .await;
    let stored = lookup_oauth_credential_by_id(&node, &credential_id)
        .await
        .unwrap()
        .expect("stored row");
    assert!(!stored.enabled);
    assert_eq!(stored.refresh_token, "refresh-rotated");
    assert_eq!(
        stored.provider_account_key.as_deref(),
        Some("user:principal-1")
    );
}

#[tokio::test]
async fn a_refresh_racing_a_sign_in_keeps_the_sign_in() {
    let node = test_node().await;
    let did = "did:key:z6MkTestRaceSignIn";
    seed_expired_grok(&node, did, None).await;
    let credential_id = oauth_credential_id(did, XAI_OAUTH_PROVIDER);
    let sign_in = crate::xai_oauth_login::credential_from_login_tokens(
        did,
        XAI_OAUTH_PROVIDER,
        &crate::xai_oauth_login::XaiLoginTokens {
            access_token: unsigned_jwt(
                json!({ "principal_type": "user", "principal_id": "principal-1" }),
            ),
            refresh_token: "refresh-signed-in".into(),
            id_token: None,
            expires_in: Some(900),
        },
        Utc::now(),
    );
    let access = ConfigAccess::local(node.clone());
    let bearer = refresh_racing(&node, did, async {
        let signed = store_sign_in(&access, sign_in.clone(), None).await.unwrap();
        assert_eq!(signed.result, SignInResult::Refreshed);
    })
    .await;
    let stored = lookup_oauth_credential_by_id(&node, &credential_id)
        .await
        .unwrap()
        .expect("stored row");
    assert_eq!(stored.refresh_token, "refresh-signed-in");
    assert_eq!(stored.access_token, sign_in.access_token);
    assert_eq!(
        stored.provider_account_key.as_deref(),
        Some("user:principal-1")
    );

    let _env = TOKEN_URL_ENV.lock().await;
    let body = token_response(json!({ "principal_type": "user", "principal_id": "principal-1" }));
    let (url, handle) = one_shot_token_server(200, body).await;
    std::env::set_var(XAI_ENV, &url);
    bearer.invalidate().await;
    let refreshed = bearer.current_bearer().await;
    std::env::remove_var(XAI_ENV);
    let request = handle.await.expect("server");
    refreshed.expect("second refresh");
    assert!(request.contains("refresh-signed-in"), "{request}");
}

#[tokio::test]
async fn a_refresh_racing_a_replaced_sign_in_keeps_the_new_row() {
    let node = test_node().await;
    let did = "did:key:z6MkTestRaceReplaced";
    seed_expired_grok(&node, did, None).await;
    let credential_id = oauth_credential_id(did, XAI_OAUTH_PROVIDER);
    let sign_in = crate::xai_oauth_login::credential_from_login_tokens(
        did,
        XAI_OAUTH_PROVIDER,
        &crate::xai_oauth_login::XaiLoginTokens {
            access_token: unsigned_jwt(
                json!({ "principal_type": "user", "principal_id": "principal-2" }),
            ),
            refresh_token: "refresh-replaced".into(),
            id_token: None,
            expires_in: Some(900),
        },
        Utc::now(),
    );
    let access = ConfigAccess::local(node.clone());
    refresh_racing(&node, did, async {
        ConfigAccess::write_local(
            &node,
            "test.remove",
            &format!(
                r#"mutation {{ delete_OAuthCredential(filter: {{ credential_id: {{ _eq: "{credential_id}" }} }}) {{ _docID }} }}"#
            ),
        )
        .await
        .unwrap();
        let signed = store_sign_in(&access, sign_in.clone(), None).await.unwrap();
        assert_eq!(signed.result, SignInResult::Added);
        assert_eq!(signed.credential.credential_id, credential_id);
    })
    .await;
    let stored = lookup_oauth_credential_by_id(&node, &credential_id)
        .await
        .unwrap()
        .expect("stored row");
    assert_eq!(stored.refresh_token, "refresh-replaced");
    assert_eq!(stored.access_token, sign_in.access_token);
    assert_eq!(
        stored.provider_account_key.as_deref(),
        Some("user:principal-2")
    );
}

#[tokio::test]
async fn a_refresh_adopts_a_replaced_document() {
    let node = test_node().await;
    let did = "did:key:z6MkTestRefreshReplaced";
    seed_expired_grok(&node, did, None).await;
    let credential_id = oauth_credential_id(did, XAI_OAUTH_PROVIDER);
    let mut removed = lookup_oauth_credential_by_id(&node, &credential_id)
        .await
        .unwrap()
        .expect("stored row");
    removed.doc_id = Some("doc-removed".into());
    removed.refresh_token = "refresh-removed".into();
    removed.access_token_expires_at = Utc::now() - Duration::seconds(30);
    let _env = TOKEN_URL_ENV.lock().await;
    let body = token_response(json!({ "principal_type": "user", "principal_id": "principal-1" }));
    let (url, handle) = one_shot_token_server(200, body).await;
    std::env::set_var(XAI_ENV, &url);
    let bearer = DbCredentialBearer::with_cache(
        node.clone(),
        did,
        XAI_OAUTH_PROVIDER,
        credential_id,
        true,
        Some(removed),
        OAuthRefreshKind::Xai,
        XAI_OAUTH_PRODUCT,
    );
    let refreshed = bearer.current_bearer().await;
    std::env::remove_var(XAI_ENV);
    let request = handle.await.expect("server");
    refreshed.expect("refresh");
    assert!(request.contains("refresh-TEST"), "{request}");
}

async fn seed_upgraded_row(node: &Arc<EmbeddedNode>, did: &str, provider: &str, account_id: &str) {
    let credential_id = oauth_credential_id(did, provider);
    let mutation = format!(
        r#"mutation {{ create_OAuthCredential(input: {{
            credential_id: "{credential_id}"
            node_did: "{did}"
            provider: "{provider}"
            access_token: "placeholder-access"
            refresh_token: "placeholder-refresh"
            {account_id}
            is_fedramp: false
            access_token_expires_at: "2020-01-01T00:00:00Z"
            last_refresh: "2019-12-31T00:00:00Z"
            enabled: true
        }}) {{ _docID }} }}"#
    );
    ConfigAccess::write_local(node, "test.seed_upgraded_row", &mutation)
        .await
        .expect("seed upgraded row");
}

#[tokio::test]
async fn upgraded_rows_keep_their_identity_and_fill_the_key_on_first_refresh() {
    let node = test_node().await;
    let did = "did:key:zUpgradedRows";
    let access = ConfigAccess::local(node.clone());
    seed_upgraded_row(
        &node,
        did,
        CHATGPT_CODEX_PROVIDER,
        r#"account_id: "acct-ws""#,
    )
    .await;
    seed_upgraded_row(&node, did, XAI_OAUTH_PROVIDER, "").await;
    let cases = [
        (
            CHATGPT_CODEX_PROVIDER,
            OAuthRefreshKind::ChatGpt,
            CHATGPT_OAUTH_PRODUCT,
            token_response(json!({ "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-ws",
                "chatgpt_account_user_id": "member-a"
            } })),
            "member-a",
        ),
        (
            XAI_OAUTH_PROVIDER,
            OAuthRefreshKind::Xai,
            XAI_OAUTH_PRODUCT,
            token_response(json!({ "principal_type": "user", "principal_id": "principal-1" })),
            "user:principal-1",
        ),
    ];
    for (provider, kind, product, body, key) in cases {
        let resolve = |pick| resolve_oauth_credential(&access, did, provider, pick);
        let before = resolve(AccountPick::Reference(None))
            .await
            .unwrap()
            .expect("an upgraded row resolves");
        assert_eq!(before.provider_account_key, None);
        let default = resolve(AccountPick::ProviderDefault).await.unwrap();
        assert_eq!(default.as_ref(), Some(&before));
        let (_, bootstrapped) = crate::oauth_http::bootstrap_oauth_client(
            node.clone(),
            did,
            provider,
            kind,
            product,
            AccountPick::ProviderDefault,
        )
        .await
        .expect("an upgraded row builds a client");
        assert_eq!(bootstrapped.credential_id, before.credential_id);
        let credential_id = oauth_credential_id(did, provider);
        let stored = lookup_oauth_credential_by_id(&node, &credential_id)
            .await
            .unwrap()
            .expect("stored row");
        assert_eq!(stored, before, "{provider}");
        assert_eq!(stored.credential_id, credential_id);
        assert_eq!(stored.access_token, "placeholder-access");
        assert_eq!(stored.refresh_token, "placeholder-refresh");
        assert!(stored.enabled);

        let after = refresh_once(&node, did, provider, kind, body).await;
        assert_eq!(
            after.provider_account_key.as_deref(),
            Some(key),
            "{provider}"
        );
        assert_eq!(after.credential_id, before.credential_id);
        assert_eq!(after.doc_id, before.doc_id);
        assert!(after.enabled);
        assert_eq!(after.account_ref, None);
        assert_eq!(after.connected_at, None);
        assert_eq!(after.account_id, before.account_id);
        let resolved = resolve(AccountPick::ProviderDefault).await.unwrap();
        assert_eq!(resolved.as_ref(), Some(&after));
    }
}
