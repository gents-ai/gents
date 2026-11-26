use anyhow::Result;
use serde_json::{json, Value};

use super::test_support::unsigned_jwt;
use super::*;
use crate::auth::claude::CLAUDE_OAUTH_PROVIDER;
use crate::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER;
use crate::clients::xai_grok::XAI_OAUTH_PROVIDER;
use gents_store::config_client::ConfigAccess;

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
            Self::Claude => crate::auth::claude::credential_from_login_tokens(
                did,
                CLAUDE_OAUTH_PROVIDER,
                &crate::auth::claude::ClaudeLoginTokens {
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
            Self::Grok => crate::auth::xai_login::credential_from_login_tokens(
                did,
                XAI_OAUTH_PROVIDER,
                &crate::auth::xai_login::XaiLoginTokens {
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

async fn access() -> (
    gents_store::test_support::TestStore,
    gents_store::identity::KeyIdentity,
    ConfigAccess,
) {
    use gents_store::config_client::NodePermission;
    use gents_store::identity::NodeIdentity;

    let (db, user, access) = gents_documents::test_support::SignedHome::start(
        "credential-interactive-sign-in",
        gents_store::node_extension::Extensions::none(),
    )
    .await
    .into_parts();
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
                gents_documents::publish_secret_configuration_in_txn(txn, "OAuthCredential", &doc, &did).await?;
                Ok::<(), anyhow::Error>(())
            })
        })
        .await
        .unwrap();
}

async fn backends(access: &ConfigAccess, did: &str) -> Vec<gents_documents::InferenceBackend> {
    access
        .transact_readonly("test.interactive.backends", |txn| {
            Box::pin(async move { gents_documents::list_inference_backends_in_txn(txn, did).await })
        })
        .await
        .unwrap()
}

fn preset(product: Product) -> crate::backends::setup::InferenceConnectionSpec {
    use crate::backends::setup::{InferenceAuthMethod as Auth, InferenceProviderId as Id};
    let (provider, auth) = match product {
        Product::ChatGpt => (Id::OpenAi, Auth::ChatGptOauth),
        Product::Claude => (Id::Anthropic, Auth::ClaudeOauth),
        Product::Grok => (Id::Grok, Auth::GrokOauth),
    };
    crate::backends::setup::connection_spec(provider, auth, "").unwrap()
}

async fn remove(access: &ConfigAccess, did: &str, credential_id: &str) -> Result<()> {
    access
        .transact_write("test.remove_account", |txn| {
            Box::pin(async move { remove_account_in_txn(txn, did, credential_id).await })
        })
        .await
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
mod scenarios;

#[tokio::test]
async fn signed_account_delete_requires_user_document_delete_permission() {
    use gents_store::config_client::NodePermission;
    use gents_store::identity::NodeIdentity;

    let (db, user, access) = gents_documents::test_support::SignedHome::start(
        "credential-delete-permission",
        gents_store::node_extension::Extensions::none(),
    )
    .await
    .into_parts();
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
    let refused = gents_protocol::error_chain::find_source::<
        gents_protocol::database_error::GraphqlFailure,
    >(error.as_ref());
    assert!(
        refused.is_some(),
        "a delete without delete-document must be refused by the database: {error:#}"
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
