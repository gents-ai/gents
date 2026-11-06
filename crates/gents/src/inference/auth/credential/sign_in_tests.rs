use defra_node::Node as EmbeddedNode;
use serde_json::{json, Value};
use std::sync::Arc;

use super::test_support::unsigned_jwt;
use super::*;
use crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER;

async fn sign_in(node: &Arc<EmbeddedNode>, credential: &OAuthCredential) -> OAuthCredential {
    let doc_id = upsert_oauth_credential(node, credential).await.unwrap();
    let stored = lookup_oauth_credential_by_id(node, &credential.credential_id)
        .await
        .unwrap()
        .expect("stored row");
    assert_eq!(stored.doc_id.as_deref(), Some(doc_id.as_str()));
    stored
}

fn chatgpt(did: &str, member: Option<&str>, refresh_token: &str) -> OAuthCredential {
    let mut auth = json!({ "chatgpt_account_id": "acct-ws" });
    if let Some(member) = member {
        auth["chatgpt_account_user_id"] = json!(member);
    }
    OAuthCredential::from_login_tokens(
        did,
        CHATGPT_CODEX_PROVIDER,
        &unsigned_jwt(
            json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct-ws" } }),
        ),
        unsigned_jwt(json!({ "https://api.openai.com/auth": auth })),
        refresh_token.to_string(),
        Utc::now(),
    )
}

fn claude(did: &str, label: &str, account_uuid: &str, refresh_token: &str) -> OAuthCredential {
    crate::inference::auth::claude::credential_from_login_tokens(
        did,
        crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
        &crate::inference::auth::claude::ClaudeLoginTokens {
            access_token: "access-TEST".into(),
            refresh_token: refresh_token.into(),
            expires_in: Some(3600),
            scope: None,
            account_id: Some(label.into()),
            organization_uuid: Some("org-1".into()),
            account_uuid: Some(account_uuid.into()),
        },
        Utc::now(),
    )
}

fn grok(did: &str, access_claims: Value, refresh_token: &str) -> OAuthCredential {
    crate::inference::auth::xai_login::credential_from_login_tokens(
        did,
        crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER,
        &crate::inference::auth::xai_login::XaiLoginTokens {
            access_token: unsigned_jwt(access_claims),
            refresh_token: refresh_token.into(),
            id_token: None,
            expires_in: Some(900),
        },
        Utc::now(),
    )
}

#[tokio::test]
async fn a_matching_sign_in_fills_an_empty_key() {
    let (db, _user, _user_access, _binding) =
        crate::test_support::signed_test_node("credential-upsert").await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let before = sign_in(&node, &chatgpt(did, None, "refresh-1")).await;
    assert_eq!(before.provider_account_key, None);

    let after = sign_in(&node, &chatgpt(did, Some("member-a"), "refresh-2")).await;
    assert_eq!(after.provider_account_key.as_deref(), Some("member-a"));
    assert_eq!(after.credential_id, before.credential_id);
    assert_eq!(after.doc_id, before.doc_id);
}

#[tokio::test]
async fn the_raw_upsert_of_another_account_replaces_tokens_and_key() {
    let (db, _user, _user_access, _binding) =
        crate::test_support::signed_test_node("credential-upsert").await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    sign_in(&node, &chatgpt(did, Some("member-a"), "refresh-1")).await;
    let stored = sign_in(&node, &chatgpt(did, Some("member-b"), "refresh-2")).await;
    assert_eq!(stored.provider_account_key.as_deref(), Some("member-b"));
    assert_eq!(stored.refresh_token, "refresh-2");

    let first = sign_in(&node, &claude(did, "account-1", "account-1", "refresh-1")).await;
    assert_eq!(
        first.provider_account_key.as_deref(),
        Some("org-1:account-1")
    );
    let stored = sign_in(&node, &claude(did, "account-2", "account-2", "refresh-2")).await;
    assert_eq!(
        stored.provider_account_key.as_deref(),
        Some("org-1:account-2")
    );
    assert_eq!(stored.account_id.as_deref(), Some("account-2"));
    assert_eq!(stored.refresh_token, "refresh-2");
}

#[tokio::test]
async fn a_keyless_sign_in_clears_the_stored_key() {
    let (db, _user, _user_access, _binding) =
        crate::test_support::signed_test_node("credential-upsert").await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let claims = json!({ "principal_type": "user", "principal_id": "principal-1" });
    let first = sign_in(&node, &grok(did, claims, "refresh-1")).await;
    assert_eq!(
        first.provider_account_key.as_deref(),
        Some("user:principal-1")
    );
    let stored = sign_in(&node, &grok(did, json!({ "sub": "user-a" }), "refresh-2")).await;
    assert_eq!(stored.provider_account_key, None);
    assert_eq!(stored.refresh_token, "refresh-2");

    sign_in(&node, &chatgpt(did, Some("member-a"), "refresh-1")).await;
    let stored = sign_in(&node, &chatgpt(did, None, "refresh-2")).await;
    assert_eq!(stored.provider_account_key, None);
    assert_eq!(stored.account_id.as_deref(), Some("acct-ws"));
    assert_eq!(stored.refresh_token, "refresh-2");
}
