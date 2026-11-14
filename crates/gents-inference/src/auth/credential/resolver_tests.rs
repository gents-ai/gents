use super::*;

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

fn credential(
    account: &gents_lean_vocab::families::account_resolver_order::Account,
) -> OAuthCredential {
    row(
        &account.owner,
        &account.provider,
        &account.credential_id,
        account.account_ref.as_deref(),
        account.connected_at.map(|seconds| seconds as i64),
        account.enabled,
    )
}

#[test]
fn account_resolution_matches_the_lean_model() {
    use gents_lean_vocab::families::account_resolver_order as family;

    family::replay(|input: &family::Input| {
        let rows: Vec<OAuthCredential> = input.rows.iter().map(credential).collect();
        let pick = if input.pick_default {
            AccountPick::ProviderDefault
        } else {
            AccountPick::Reference(input.account_ref.as_deref())
        };
        family::Resolution {
            pool: pick_oauth_pool(&rows, &input.owner, &input.provider)
                .into_iter()
                .map(|row| row.credential_id.clone())
                .collect(),
            selected: pick_oauth_credential(&rows, &input.owner, &input.provider, pick)
                .map(|row| row.credential_id.clone()),
        }
    });
}

#[tokio::test]
async fn resolver_reads_through_config_access() {
    let (db, _user, _user_access, _binding) = crate::test_support::signed_test_node(
        "credential-resolver",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let access = super::rows::runtime_credential_access(&node, did).unwrap();
    for credential in [
        row(did, PROVIDER, "xai-oauth:b", Some("b"), Some(1_000), true),
        row(did, PROVIDER, "xai-oauth:original", None, None, true),
    ] {
        upsert_oauth_credential(&node, &credential).await.unwrap();
    }
    for (pick, expected) in [
        (AccountPick::Reference(Some("b")), "xai-oauth:b"),
        (AccountPick::Reference(None), "xai-oauth:original"),
        (AccountPick::ProviderDefault, "xai-oauth:original"),
    ] {
        let resolved = resolve_oauth_credential(&access, did, PROVIDER, pick)
            .await
            .unwrap()
            .map(|row| row.credential_id);
        assert_eq!(resolved.as_deref(), Some(expected), "{pick:?}");
    }
}

#[tokio::test]
async fn resolver_missing_account_keeps_the_missing_guidance() {
    let (db, _user, _user_access, _binding) = crate::test_support::signed_test_node(
        "credential-resolver",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let missing = classify_oauth_auth_error(
        &XAI_OAUTH_PRODUCT,
        did,
        PROVIDER,
        &OAuthAuthProblem::Missing,
    );
    let bootstrap = |pick| {
        crate::auth::http::bootstrap_oauth_client(
            node.clone(),
            did,
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
        &row(did, PROVIDER, "xai-oauth:original", None, None, true),
    )
    .await
    .unwrap();
    let Err(error) = bootstrap(AccountPick::Reference(Some("b"))).await else {
        panic!("a reference never falls back to the original account");
    };
    assert_eq!(error.to_string(), missing);
}
