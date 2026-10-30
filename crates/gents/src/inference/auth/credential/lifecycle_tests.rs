use anyhow::Result;
use defra_node::EmbeddedNode;
use serde_json::{json, Value};
use std::sync::Arc;

use super::test_support::{test_node, unsigned_jwt};
use super::*;
use crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER;
use crate::claude_oauth::CLAUDE_OAUTH_PROVIDER;
use crate::config_client::ConfigAccess;
use crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;

#[derive(Debug, Clone, Copy)]
enum Product {
    ChatGpt,
    Claude,
    Grok,
}
const PRODUCTS: [Product; 3] = [Product::ChatGpt, Product::Claude, Product::Grok];

impl Product {
    fn provider(self) -> &'static str {
        match self {
            Self::ChatGpt => CHATGPT_CODEX_PROVIDER,
            Self::Claude => CLAUDE_OAUTH_PROVIDER,
            Self::Grok => XAI_OAUTH_PROVIDER,
        }
    }

    fn sign_in(self, did: &str, who: &str, refresh_token: &str) -> OAuthCredential {
        match self {
            Self::ChatGpt => {
                let auth = json!({
                    "chatgpt_account_id": "acct-ws",
                    "chatgpt_account_user_id": format!("member-{who}"),
                });
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
            Self::Claude => crate::claude_oauth::credential_from_login_tokens(
                did,
                CLAUDE_OAUTH_PROVIDER,
                &crate::claude_oauth::ClaudeLoginTokens {
                    access_token: "access-TEST".into(),
                    refresh_token: refresh_token.into(),
                    expires_in: Some(3600),
                    scope: None,
                    account_id: Some(format!("label-{who}")),
                    organization_uuid: Some("org-1".into()),
                    account_uuid: Some(format!("account-{who}")),
                },
                Utc::now(),
            ),
            Self::Grok => crate::xai_oauth_login::credential_from_login_tokens(
                did,
                XAI_OAUTH_PROVIDER,
                &crate::xai_oauth_login::XaiLoginTokens {
                    access_token: unsigned_jwt(json!({
                        "principal_type": "user",
                        "principal_id": format!("principal-{who}"),
                    })),
                    refresh_token: refresh_token.into(),
                    id_token: None,
                    expires_in: Some(900),
                },
                Utc::now(),
            ),
        }
    }
}

async fn access() -> ConfigAccess {
    ConfigAccess::local(test_node().await)
}

async fn store(access: &ConfigAccess, credential: OAuthCredential) -> SignIn {
    store_sign_in(access, credential, None).await.unwrap()
}

async fn rows(access: &ConfigAccess, did: &str, provider: &str) -> Vec<OAuthCredential> {
    list_oauth_credentials_on(access, did)
        .await
        .unwrap()
        .into_iter()
        .filter(|row| row.provider == provider)
        .collect()
}

async fn set_enabled(access: &ConfigAccess, credential_id: &str, enabled: bool) {
    let mutation = format!(
        r#"mutation {{ update_OAuthCredential(filter: {{ credential_id: {{ _eq: "{credential_id}" }} }}, input: {{ enabled: {enabled} }}) {{ _docID }} }}"#
    );
    access.write("test.set_enabled", &mutation).await.unwrap();
}

fn did(test: &str, product: Product) -> String {
    format!("did:key:z6MkTest{test}{product:?}")
}

fn chatgpt_with(
    did: &str,
    access_auth: Value,
    id_claims: Value,
    refresh_token: &str,
) -> OAuthCredential {
    OAuthCredential::from_login_tokens(
        did,
        CHATGPT_CODEX_PROVIDER,
        &unsigned_jwt(id_claims),
        unsigned_jwt(json!({ "https://api.openai.com/auth": access_auth })),
        refresh_token.to_string(),
        Utc::now(),
    )
}

fn claude(did: &str, label: &str, account: Option<&str>, refresh_token: &str) -> OAuthCredential {
    crate::claude_oauth::credential_from_login_tokens(
        did,
        CLAUDE_OAUTH_PROVIDER,
        &crate::claude_oauth::ClaudeLoginTokens {
            access_token: "access-TEST".into(),
            refresh_token: refresh_token.into(),
            expires_in: Some(3600),
            scope: None,
            account_id: Some(label.into()),
            organization_uuid: account.map(|_| "org-1".into()),
            account_uuid: account.map(str::to_owned),
        },
        Utc::now(),
    )
}

fn grok(did: &str, principal_id: &str, refresh_token: &str) -> OAuthCredential {
    crate::xai_oauth_login::credential_from_login_tokens(
        did,
        XAI_OAUTH_PROVIDER,
        &crate::xai_oauth_login::XaiLoginTokens {
            access_token: unsigned_jwt(
                json!({ "principal_type": "user", "principal_id": principal_id }),
            ),
            refresh_token: refresh_token.into(),
            id_token: None,
            expires_in: Some(900),
        },
        Utc::now(),
    )
}

async fn seed_raw(access: &ConfigAccess, input: Value) {
    access
        .transact_write("test.seed_raw", |txn| {
            let input = input.clone();
            Box::pin(async move {
                txn.execute_with_variables(
                    "mutation($input: OAuthCredentialMutationInputArg!) { create_OAuthCredential(input: $input) { _docID } }",
                    &json!({ "input": input }),
                )
                .await
            })
        })
        .await
        .unwrap();
}

fn raw_row(did: &str, provider: &str) -> Value {
    json!({
        "credential_id": oauth_credential_id(did, provider),
        "node_did": did,
        "provider": provider,
        "access_token": "placeholder-access",
        "refresh_token": "placeholder-refresh",
        "is_fedramp": false,
        "access_token_expires_at": "2020-01-01T00:00:00Z",
        "enabled": true,
    })
}

async fn labeled(access: &ConfigAccess, credential: OAuthCredential, label: &str) -> SignIn {
    store_sign_in(access, credential, Some(label))
        .await
        .unwrap()
}

fn node(access: &ConfigAccess) -> &Arc<EmbeddedNode> {
    access.local_node().expect("tests run on a local node")
}

async fn backends(access: &ConfigAccess, did: &str) -> Vec<crate::InferenceBackend> {
    crate::backend_registry::list_all_backends(node(access))
        .await
        .unwrap()
        .into_iter()
        .filter(|backend| backend.node_did == did)
        .collect()
}

fn preset(product: Product) -> crate::inference_setup::InferenceConnectionSpec {
    use crate::inference_setup::{InferenceAuthMethod as Auth, InferenceProviderId as Id};
    let (provider, auth) = match product {
        Product::ChatGpt => (Id::OpenAi, Auth::ChatGptOauth),
        Product::Claude => (Id::Anthropic, Auth::ClaudeOauth),
        Product::Grok => (Id::Grok, Auth::GrokOauth),
    };
    crate::inference_setup::connection_spec(provider, auth, "").unwrap()
}

async fn remove(access: &ConfigAccess, did: &str, credential_id: &str) -> Result<()> {
    access
        .transact_write("test.remove_account", |txn| {
            Box::pin(async move { remove_account_in_txn(txn, did, credential_id).await })
        })
        .await
}

#[tokio::test]
async fn a_first_sign_in_is_the_original_account() {
    let access = access().await;
    for product in PRODUCTS {
        let did = did("First", product);
        let signed = store(&access, product.sign_in(&did, "a", "refresh-1")).await;
        assert_eq!(signed.result, SignInResult::Added, "{product:?}");
        let stored = rows(&access, &did, product.provider()).await;
        assert_eq!(stored.len(), 1, "{product:?}");
        assert_eq!(
            stored[0].credential_id,
            oauth_credential_id(&did, product.provider())
        );
        assert_eq!(stored[0].account_ref, None, "{product:?}");
        assert!(stored[0].connected_at.is_some(), "{product:?}");
    }
}

#[tokio::test]
async fn another_account_is_added_beside_the_first() {
    let access = access().await;
    for product in PRODUCTS {
        let did = did("Another", product);
        let provider = product.provider();
        store(&access, product.sign_in(&did, "a", "refresh-a")).await;
        let first = rows(&access, &did, provider).await.remove(0);

        let signed = store(&access, product.sign_in(&did, "b", "refresh-b")).await;
        assert_eq!(signed.result, SignInResult::Added, "{product:?}");
        let stored = rows(&access, &did, provider).await;
        assert_eq!(stored.len(), 2, "{product:?}");
        let kept = stored
            .iter()
            .find(|row| row.doc_id == first.doc_id)
            .expect("the first row is kept");
        assert_eq!(kept, &first, "{product:?}: the first account is untouched");
        let added = stored
            .iter()
            .find(|row| row.doc_id != first.doc_id)
            .unwrap();
        assert_eq!(added.refresh_token, "refresh-b");
        let reference = added.account_ref.clone().expect("a minted reference");
        assert!(!reference.is_empty() && !reference.contains("did:"));
        assert_ne!(added.account_ref, first.account_ref);
        assert_eq!(added.credential_id, format!("{provider}:{did}:{reference}"));
        assert!(added.connected_at.unwrap() >= first.connected_at.unwrap());

        let resolve = |pick| resolve_oauth_credential(&access, &did, provider, pick);
        let by_ref = resolve(AccountPick::Reference(Some(&reference)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(by_ref.credential_id, added.credential_id);
        let original = resolve(AccountPick::Reference(None))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(original.credential_id, first.credential_id);
    }
}

#[tokio::test]
async fn the_same_account_refreshes_in_place() {
    let access = access().await;
    for product in PRODUCTS {
        let did = did("Same", product);
        let provider = product.provider();
        let first = store(&access, product.sign_in(&did, "a", "refresh-1")).await;
        assert_eq!(first.result, SignInResult::Added, "{product:?}");
        let before = rows(&access, &did, provider).await.remove(0);
        assert!(before.connected_at.is_some(), "{product:?}");
        set_enabled(&access, &before.credential_id, false).await;

        let again = store(&access, product.sign_in(&did, "a", "refresh-2")).await;
        assert_eq!(again.result, SignInResult::Refreshed, "{product:?}");
        let stored = rows(&access, &did, provider).await;
        assert_eq!(stored.len(), 1, "{product:?}");
        let after = &stored[0];
        assert_eq!(after.doc_id, before.doc_id);
        assert_eq!(after.credential_id, before.credential_id);
        assert_eq!(after.account_ref, before.account_ref);
        assert_eq!(after.connected_at, before.connected_at);
        assert_eq!(after.refresh_token, "refresh-2");
        assert!(after.enabled, "{product:?}");
    }
}

#[tokio::test]
async fn two_concurrent_first_sign_ins_both_store() {
    let access = access().await;
    for product in PRODUCTS {
        let did = did("Concurrent", product);
        let (a, b) = tokio::join!(
            store_sign_in(&access, product.sign_in(&did, "a", "refresh-a"), None),
            store_sign_in(&access, product.sign_in(&did, "b", "refresh-b"), None),
        );
        a.unwrap();
        b.unwrap();
        let stored = rows(&access, &did, product.provider()).await;
        assert_eq!(stored.len(), 2, "{product:?}");
        assert_eq!(
            stored
                .iter()
                .filter(|row| row.account_ref.is_none())
                .count(),
            1,
            "{product:?}"
        );
    }
}

mod accounts;
mod backends;
mod identity;
mod labels;
