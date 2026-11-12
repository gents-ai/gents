use gents_documents::BackendAuth;

#[tokio::test]
async fn oauth_account_scope_matches_lean() {
    use crate::auth::credential::{
        resolve_oauth_credential, resolve_oauth_pool, upsert_oauth_credential_on, AccountPick,
    };
    use crate::backends::provider::BackendProviderOauthExt;
    use gents_loop::backend_provider::BackendProviderKind;
    use gents_lean_vocab::families::configuration_scope_oauth_account as family;
    let kind_of = |value: &str| -> BackendProviderKind {
        serde_json::from_value(serde_json::Value::String(value.to_owned())).unwrap()
    };
    let backend_auth = |auth: &family::OauthAuth| -> BackendAuth {
        let mut value = serde_json::to_value(auth).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .retain(|_, field| !field.is_null());
        let backend: BackendAuth = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&backend).unwrap(), value);
        backend
    };
    let cases = family::cases();
    let rows_of = |rows: &[family::OauthRow]| -> Vec<(String, String, Option<String>, bool)> {
        rows.iter()
            .map(|row| {
                (
                    row.owner.clone(),
                    row.provider_kind.clone(),
                    row.account.clone(),
                    row.enabled,
                )
            })
            .collect()
    };
    let rows = rows_of(&cases[0].input.rows);

    let (local_db, _local_user, access, _local_agent) =
        crate::test_support::signed_test_node("oauth-model-local", gents_store::node_extension::Extensions::none()).await;
    let (foreign_db, _foreign_user, foreign_access, _foreign_agent) =
        crate::test_support::signed_test_node("oauth-model-foreign", gents_store::node_extension::Extensions::none()).await;
    let local_node = local_db.node_identity.did();
    let foreign_node = foreign_db.node_identity.did();
    assert_ne!(local_node, foreign_node);
    let actual_owner = |model: &str| match model {
        "alice" => local_node,
        "bob" => foreign_node,
        _ => panic!("unknown model Node {model}"),
    };
    let model_owner = |actual: &str| {
        if actual == local_node {
            "alice"
        } else if actual == foreign_node {
            "bob"
        } else {
            panic!("unknown actual Node {actual}")
        }
    };
    for (owner, provider_kind, account, enabled) in &rows {
        let provider = kind_of(provider_kind).oauth_provider().unwrap();
        let owner = owner.as_str();
        let account = account.as_deref();
        let credential = crate::auth::credential::OAuthCredential {
            doc_id: None,
            credential_id: format!("{provider}:{owner}:{}", account.unwrap_or("original")),
            node_did: actual_owner(owner).to_string(),
            provider: provider.to_string(),
            access_token: "access-TEST".to_string(),
            refresh_token: "refresh-TEST".to_string(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled: *enabled,
            account_ref: account.map(str::to_string),
            connected_at: None,
            provider_account_key: None,
            label: None,
        };
        let writer = if owner == "alice" {
            &access
        } else {
            &foreign_access
        };
        upsert_oauth_credential_on(writer, &credential)
            .await
            .unwrap();
    }

    let resolved_row = |owner: &str, account: Option<&str>| family::OauthResolved {
        owner: owner.to_owned(),
        account: account.map(str::to_owned),
    };
    for case in cases {
        let input = &case.input;
        assert_eq!(rows_of(&input.rows), rows);
        let auth = backend_auth(&input.auth);
        let text = serde_json::to_string(&auth).unwrap();
        assert!(
            !text.contains(&input.scope) && !text.contains(&input.stored_owner),
            "{text}"
        );
        let actual_scope = actual_owner(&input.scope);
        let resolved = match &auth {
            BackendAuth::NodeOAuthPool { .. } => {
                let provider = kind_of(&input.provider_kind).oauth_provider().unwrap();
                let pool_rows = resolve_oauth_pool(&access, actual_scope, provider)
                    .await
                    .unwrap();
                let mut pool: Vec<_> = pool_rows
                    .iter()
                    .map(|row| {
                        resolved_row(model_owner(&row.node_did), row.account_ref.as_deref())
                    })
                    .collect();
                pool.sort_by_key(|row| (row.owner.clone(), row.account.clone()));
                pool
            }
            BackendAuth::NodeOAuth { .. } => {
                let provider = kind_of(&input.provider_kind).oauth_provider().unwrap();
                resolve_oauth_credential(
                    &access,
                    actual_scope,
                    provider,
                    AccountPick::Reference(auth.oauth_account_ref()),
                )
                .await
                .unwrap()
                .map(|row| resolved_row(model_owner(&row.node_did), row.account_ref.as_deref()))
                .into_iter()
                .collect()
            }
            _ => Vec::new(),
        };
        case.check(family::OauthOutput {
            uses_node_oauth: auth.uses_node_oauth(),
            resolved,
        });
    }
}
