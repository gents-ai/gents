use defra_node::EmbeddedNode;
use std::sync::Arc;

use super::test_support::{
    one_shot_token_server, seed_credential, seed_credential_with_refresh_token, test_node,
    TOKEN_URL_ENV,
};
use super::*;

#[tokio::test]
async fn failed_refresh_is_not_retried_during_the_cooldown() {
    let node = test_node().await;
    let did = "did:key:z6MkRevoked";
    let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
    seed_credential(&node, did, provider, Utc::now() - Duration::minutes(1)).await;
    let (url, handle) = one_shot_token_server(401, r#"{"error":"invalid_grant"}"#).await;
    // Process-global; hold TOKEN_URL_ENV while it is set.
    let _env = TOKEN_URL_ENV.lock().await;
    std::env::set_var(
        crate::xai_oauth_refresh::XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV,
        &url,
    );
    let bearer = DbCredentialBearer::new(
        node,
        did,
        provider,
        oauth_credential_id(did, provider),
        true,
        OAuthRefreshKind::Xai,
        XAI_OAUTH_PRODUCT,
    );
    let first = bearer.current_bearer().await.expect_err("revoked");
    let request = handle.await.expect("server");
    let second = bearer.current_bearer().await.expect_err("cooldown");
    std::env::remove_var(crate::xai_oauth_refresh::XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV);

    assert!(request.contains("grant_type=refresh_token"), "{request}");
    assert!(
        first.to_string().contains("is expired or revoked"),
        "{first}"
    );
    assert_eq!(first.to_string(), second.to_string());
}

#[tokio::test]
async fn failed_refresh_keeps_the_bearer_forced_and_never_reserves_the_bad_token() {
    let node = test_node().await;
    let did = "did:key:z6MkRejected";
    let provider = crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER;
    seed_credential(&node, did, provider, Utc::now() + Duration::hours(1)).await;
    let (url, handle) = one_shot_token_server(401, r#"{"error":"invalid_grant"}"#).await;
    // Process-global; hold TOKEN_URL_ENV while it is set.
    let _env = TOKEN_URL_ENV.lock().await;
    std::env::set_var(
        gents_protocol::chatgpt_oauth::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
        &url,
    );
    let bearer = DbCredentialBearer::new(
        node,
        did,
        provider,
        oauth_credential_id(did, provider),
        true,
        OAuthRefreshKind::ChatGpt,
        CHATGPT_OAUTH_PRODUCT,
    );
    bearer.invalidate().await;
    let first = bearer.current_bearer().await;
    handle.await.expect("server");
    let second = bearer.current_bearer().await;
    std::env::remove_var(gents_protocol::chatgpt_oauth::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR);

    let first = first.expect_err("rejected refresh");
    let second = second.expect_err("cooldown must not re-serve the rejected token");
    assert!(
        first.to_string().contains("is expired or revoked"),
        "{first}"
    );
    assert_eq!(first.to_string(), second.to_string());
}

fn grok_bearer(node: Arc<EmbeddedNode>, did: &str) -> DbCredentialBearer {
    let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
    DbCredentialBearer::new(
        node,
        did,
        provider,
        oauth_credential_id(did, provider),
        true,
        OAuthRefreshKind::Xai,
        XAI_OAUTH_PRODUCT,
    )
}

async fn set_label(node: &std::sync::Arc<EmbeddedNode>, did: &str, label: Option<&str>) {
    let credential_id = oauth_credential_id(did, crate::xai_grok_oauth::XAI_OAUTH_PROVIDER);
    let mutation = format!(
        r#"mutation {{ update_OAuthCredential(filter: {{ credential_id: {{ _eq: "{credential_id}" }} }}, input: {{ {} }}) {{ _docID }} }}"#,
        gents_protocol::graphql::nullable_string_field("label", label),
    );
    crate::config_client::ConfigAccess::write_local(node, "test.label", &mutation)
        .await
        .unwrap();
}

async fn revoked_twice(bearer: &DbCredentialBearer) -> (String, String, String) {
    let (url, handle) = one_shot_token_server(401, r#"{"error":"invalid_grant"}"#).await;
    let _env = TOKEN_URL_ENV.lock().await;
    std::env::set_var(
        crate::xai_oauth_refresh::XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV,
        &url,
    );
    let first = bearer.current_bearer().await.expect_err("revoked");
    let request = handle.await.expect("server");
    let second = bearer.current_bearer().await.expect_err("cooldown");
    std::env::remove_var(crate::xai_oauth_refresh::XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV);
    (first.to_string(), second.to_string(), request)
}

#[tokio::test]
async fn a_revoked_refresh_names_the_account() {
    for (label, named) in [(Some("label-a"), "label-a"), (None, "Grok")] {
        let node = test_node().await;
        let did = "did:key:z6MkTestRevokedNamed";
        let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
        seed_credential(&node, did, provider, Utc::now() - Duration::minutes(1)).await;
        set_label(&node, did, label).await;
        let (first, second, _) = revoked_twice(&grok_bearer(node, did)).await;
        for error in [&first, &second] {
            assert!(
                error.starts_with(&format!(
                    "Grok account \"{named}\" is signed out (expired or revoked)"
                )),
                "{error}"
            );
            assert!(error.contains("is expired or revoked"), "{error}");
            assert!(
                error.contains("gents config profile set-account"),
                "{error}"
            );
            assert!(
                !error.contains("SECRET") && !error.contains("IDENTITY"),
                "{error}"
            );
        }
    }
}

#[tokio::test]
async fn a_revoked_refresh_never_uses_another_account() {
    let node = test_node().await;
    let did = "did:key:z6MkTestRevokedOther";
    let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
    seed_credential(&node, did, provider, Utc::now() - Duration::minutes(1)).await;
    let mut other = lookup_oauth_credential_by_id(&node, &oauth_credential_id(did, provider))
        .await
        .unwrap()
        .unwrap();
    other.doc_id = None;
    other.credential_id = format!("{}:acct-b", other.credential_id);
    other.account_ref = Some("acct-b".into());
    other.refresh_token = "refresh-B".into();
    other.access_token_expires_at = Utc::now() + Duration::hours(1);
    upsert_oauth_credential(&node, &other).await.unwrap();
    let (_, _, request) = revoked_twice(&grok_bearer(node, did)).await;
    assert!(request.contains("refresh-TEST"), "{request}");
    assert!(!request.contains("refresh-B"), "{request}");
}

#[tokio::test]
async fn a_reused_slot_names_the_new_account() {
    let node = test_node().await;
    let did = "did:key:z6MkTestReusedSlot";
    let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
    let credential_id = oauth_credential_id(did, provider);
    seed_credential(&node, did, provider, Utc::now() - Duration::minutes(1)).await;
    set_label(&node, did, Some("label-a")).await;
    let cached = lookup_oauth_credential_by_id(&node, &credential_id)
        .await
        .unwrap()
        .unwrap();
    let bearer = DbCredentialBearer::with_cache(
        node.clone(),
        did,
        provider,
        credential_id.clone(),
        true,
        Some(cached),
        OAuthRefreshKind::Xai,
        XAI_OAUTH_PRODUCT,
    );
    let access = crate::config_client::ConfigAccess::local(node.clone());
    access
        .transact_write("test.remove", |txn| {
            let credential_id = credential_id.clone();
            Box::pin(async move { remove_account_in_txn(txn, did, &credential_id).await })
        })
        .await
        .unwrap();
    seed_credential_with_refresh_token(
        &node,
        did,
        provider,
        Utc::now() - Duration::minutes(1),
        "refresh-NEW",
    )
    .await;
    set_label(&node, did, Some("label-new")).await;
    let (first, _, request) = revoked_twice(&bearer).await;
    assert!(request.contains("refresh-NEW"), "{request}");
    assert!(first.contains("\"label-new\""), "{first}");
    assert!(
        !first.contains("label-a") && !first.contains("removed"),
        "{first}"
    );
}

#[tokio::test]
async fn a_disabled_or_removed_row_names_the_account() {
    let node = test_node().await;
    let did = "did:key:z6MkTestDisabledNamed";
    let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
    let credential_id = oauth_credential_id(did, provider);
    seed_credential(&node, did, provider, Utc::now() - Duration::minutes(1)).await;
    set_label(&node, did, Some("label-a")).await;
    let cached = lookup_oauth_credential_by_id(&node, &credential_id)
        .await
        .unwrap()
        .unwrap();
    let bearer = DbCredentialBearer::with_cache(
        node.clone(),
        did,
        provider,
        credential_id.clone(),
        true,
        Some(cached),
        OAuthRefreshKind::Xai,
        XAI_OAUTH_PRODUCT,
    );
    let access = crate::config_client::ConfigAccess::local(node.clone());
    set_account_enabled(&access, did, &credential_id, false)
        .await
        .unwrap();
    let disabled = bearer
        .current_bearer()
        .await
        .expect_err("disabled")
        .to_string();
    assert!(
        disabled.starts_with("Grok account \"label-a\" is disabled"),
        "{disabled}"
    );
    assert!(disabled.contains("set-account"), "{disabled}");

    access
        .transact_write("test.remove", |txn| {
            let credential_id = credential_id.clone();
            Box::pin(async move { remove_account_in_txn(txn, did, &credential_id).await })
        })
        .await
        .unwrap();
    let removed = bearer
        .current_bearer()
        .await
        .expect_err("removed")
        .to_string();
    assert!(
        removed.starts_with("Grok account \"label-a\" is removed from this node"),
        "{removed}"
    );
    assert!(removed.contains("set-account"), "{removed}");
    assert!(removed.contains("adds a new account"), "{removed}");
    assert!(
        !removed.contains(XAI_OAUTH_PRODUCT.login_command),
        "{removed}"
    );
}

#[tokio::test]
async fn a_limit_like_label_is_not_a_limit() {
    for label in ["usage limit", "maximum context length"] {
        let node = test_node().await;
        let did = "did:key:z6MkTestLimitLabel";
        let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
        seed_credential(&node, did, provider, Utc::now() - Duration::minutes(1)).await;
        set_label(&node, did, Some(label)).await;
        let (first, _, _) = revoked_twice(&grok_bearer(node, did)).await;
        assert!(first.starts_with("Grok account \"Grok\""), "{first}");
        assert!(!first.contains(label), "{first}");
        assert!(
            gents_loop::provider_limit::classify_provider_limit(&first, Utc::now()).is_none(),
            "{first}"
        );
    }
}
