use anyhow::Result;
use serde_json::{json, Value};

use super::test_support::unsigned_jwt;
use super::*;
use crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER;
use crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER;
use crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER;
use crate::store::config_client::ConfigAccess;

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
            Self::Claude => crate::inference::auth::claude::credential_from_login_tokens(
                did,
                CLAUDE_OAUTH_PROVIDER,
                &crate::inference::auth::claude::ClaudeLoginTokens {
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
            Self::Grok => crate::inference::auth::xai_login::credential_from_login_tokens(
                did,
                XAI_OAUTH_PROVIDER,
                &crate::inference::auth::xai_login::XaiLoginTokens {
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

async fn access() -> (crate::support::TestDb, crate::KeyIdentity, ConfigAccess) {
    use crate::store::config_client::NodePermission;
    use crate::NodeIdentity;

    let (db, user, access) =
        crate::test_support::signed_test_node_without_agent("credential-interactive-sign-in").await;
    let runtime = ConfigAccess::local_as(
        db.node.clone(),
        ::identity::Did::new(db.node_identity.did()).unwrap(),
    );
    let actor = ::identity::Did::new(user.did()).unwrap();
    for permission in [
        NodePermission::DocumentDelete,
        NodePermission::DacRelationDelete,
    ] {
        runtime
            .grant_node_permission(&actor, permission)
            .await
            .unwrap();
    }
    (db, user, access)
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

async fn set_enabled(access: &ConfigAccess, did: &str, credential_id: &str, enabled: bool) {
    set_account_enabled(access, did, credential_id, enabled)
        .await
        .unwrap();
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
    crate::inference::auth::claude::credential_from_login_tokens(
        did,
        CLAUDE_OAUTH_PROVIDER,
        &crate::inference::auth::claude::ClaudeLoginTokens {
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
    crate::inference::auth::xai_login::credential_from_login_tokens(
        did,
        XAI_OAUTH_PROVIDER,
        &crate::inference::auth::xai_login::XaiLoginTokens {
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
    let did = input["node_did"].as_str().unwrap().to_owned();
    access
        .transact_write("test.seed_raw", |txn| {
            let input = input.clone();
            let did = did.clone();
            Box::pin(async move {
                let response = txn.execute_client_signed_create(
                    "mutation($input: OAuthCredentialMutationInputArg!) { create_OAuthCredential(input: $input) { _docID } }",
                    &json!({ "input": input }),
                    gents_protocol::schemas::OAUTH_CREDENTIAL,
                ).await?;
                let doc = gents_protocol::graphql::extract_mutation_doc_id(&response, "OAuthCredential")?;
                crate::store::config_client::publish_secret_configuration_in_txn(txn, "OAuthCredential", &doc, &did).await?;
                Ok::<(), anyhow::Error>(())
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

async fn backends(access: &ConfigAccess, did: &str) -> Vec<crate::InferenceBackend> {
    access
        .transact_readonly("test.interactive.backends", |txn| {
            Box::pin(async move {
                crate::store::config_client::list_inference_backends_in_txn(txn, did).await
            })
        })
        .await
        .unwrap()
}

fn preset(product: Product) -> crate::inference::backends::setup::InferenceConnectionSpec {
    use crate::inference::backends::setup::{
        InferenceAuthMethod as Auth, InferenceProviderId as Id,
    };
    let (provider, auth) = match product {
        Product::ChatGpt => (Id::OpenAi, Auth::ChatGptOauth),
        Product::Claude => (Id::Anthropic, Auth::ClaudeOauth),
        Product::Grok => (Id::Grok, Auth::GrokOauth),
    };
    crate::inference::backends::setup::connection_spec(provider, auth, "").unwrap()
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
    let (db, _user, access) = access().await;
    for product in PRODUCTS {
        let did = db.node_identity.did().to_owned();
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
        let runtime_row = lookup_oauth_credential_by_id(&db.node, &stored[0].credential_id)
            .await
            .unwrap()
            .expect("runtime writer can read the interactive sign-in");
        assert_eq!(runtime_row, stored[0], "{product:?}");
    }
}

#[tokio::test]
async fn another_account_is_added_beside_the_first() {
    let (db, _user, access) = access().await;
    for product in PRODUCTS {
        let did = db.node_identity.did().to_owned();
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
    let (db, _user, access) = access().await;
    for product in PRODUCTS {
        let did = db.node_identity.did().to_owned();
        let provider = product.provider();
        let first = store(&access, product.sign_in(&did, "a", "refresh-1")).await;
        assert_eq!(first.result, SignInResult::Added, "{product:?}");
        let before = rows(&access, &did, provider).await.remove(0);
        assert!(before.connected_at.is_some(), "{product:?}");
        set_enabled(&access, &did, &before.credential_id, false).await;

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
    let (db, _user, access) = access().await;
    for product in PRODUCTS {
        let did = db.node_identity.did().to_owned();
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

#[tokio::test]
async fn signed_account_delete_requires_user_document_delete_permission() {
    use crate::store::config_client::{ConfigClientError, NodePermission};
    use crate::store::identity::NodeIdentity;

    let (db, user, access) =
        crate::test_support::signed_test_node_without_agent("credential-delete-permission").await;
    let node_did = db.node_identity.did();
    store(
        &access,
        Product::Claude.sign_in(node_did, "delete", "refresh-1"),
    )
    .await;
    let existing = rows(&access, node_did, CLAUDE_OAUTH_PROVIDER).await;
    let [credential] = existing.as_slice() else {
        panic!("one signed User-owned credential must exist");
    };
    let credential_id = credential.credential_id.clone();
    let admin = ConfigAccess::local_as(db.node.clone(), node_did.parse().unwrap());
    // The fixture's OperatorUser grant includes DocumentDelete; withdraw it so
    // the denial below is the user's own missing permission.
    admin
        .revoke_node_permission(&user.did().parse().unwrap(), NodePermission::DocumentDelete)
        .await
        .unwrap();

    let error = remove(&access, node_did, &credential_id).await.unwrap_err();
    let typed =
        gents_protocol::error_chain::find_map_source(error.as_ref(), |cause| match cause
            .downcast_ref::<ConfigClientError>(
        ) {
            Some(ConfigClientError::ClientAgentSigning(
                defra_http::client_signed_create::ClientSignedCreateError::Incomplete,
            )) => Some(()),
            _ => None,
        });
    assert!(
        typed.is_some(),
        "a denied no-wave delete must remain an error: {error:#}"
    );
    assert_eq!(
        rows(&access, node_did, CLAUDE_OAUTH_PROVIDER).await.len(),
        1
    );

    admin
        .grant_node_permission(&user.did().parse().unwrap(), NodePermission::DocumentDelete)
        .await
        .unwrap();
    remove(&access, node_did, &credential_id).await.unwrap();
    assert!(rows(&access, node_did, CLAUDE_OAUTH_PROVIDER)
        .await
        .is_empty());
}
