use anyhow::Result;
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use serde_json::json;
use std::sync::Arc;

fn claude_sign_in(did: &str, who: &str) -> gents_inference::auth::credential::OAuthCredential {
    gents_inference::auth::claude::credential_from_login_tokens(
        did,
        gents_inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
        &gents_inference::auth::claude::ClaudeLoginTokens {
            access_token: "access-TEST".into(),
            refresh_token: format!("refresh-{who}"),
            expires_in: Some(3600),
            scope: None,
            account_id: Some(format!("label-{who}")),
            organization_uuid: Some("org-1".into()),
            account_uuid: Some(format!("account-{who}")),
        },
        chrono::Utc::now(),
    )
}

#[tokio::test]
async fn account_events_never_repoint_a_profile() -> Result<()> {
    use gents_inference::auth::credential::{
        remove_account_in_txn, resolve_oauth_credential, set_account_enabled, store_sign_in,
        AccountPick,
    };
    let did = "did:key:z6MkTestProfileEvents";
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    gents_store::schema::ensure_runtime_schemas(&node).await?;
    let access = ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(
            node.node_identity_did()
                .expect("fixture node has an identity"),
        )
        .unwrap(),
    );
    gents_documents::test_support::bootstrap_signed_test_node(&access, did).await;
    let spec = gents_inference::backends::setup::connection_spec(
        gents_inference::backends::setup::InferenceProviderId::Anthropic,
        gents_inference::backends::setup::InferenceAuthMethod::ClaudeOauth,
        "",
    )?;
    let original = gents_documents::InferenceBackend {
        physical_doc_id: None,
        node_did: did.to_owned(),
        backend_id: "claude".into(),
        name: "Claude".into(),
        provider_kind: spec.provider_kind,
        openai_wire_api: spec.openai_wire_api,
        endpoint: spec.endpoint,
        claude_platform: None,
        auth: gents_documents::BackendAuth::NodeOAuth { account_ref: None },
        connect_timeout_secs: None,
        discovery_timeout_secs: None,
        max_concurrent: None,
        max_queue_depth: None,
        enabled: true,
        tags: Vec::new(),
    };
    gents_documents::write_inference_backend_document(
        &access,
        &original,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await?;
    let a = store_sign_in(&access, claude_sign_in(did, "a"), None).await?;
    let b = store_sign_in(&access, claude_sign_in(did, "b"), None).await?;
    let b_backend = format!(
        "{}-{}",
        gents_inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
        b.credential.account_ref.as_deref().unwrap()
    );
    for (profile_id, backend_id) in [
        ("on-a", "claude"),
        ("on-b", b_backend.as_str()),
        ("compaction", b_backend.as_str()),
    ] {
        let profile: gents_documents::InferenceProfile = serde_json::from_value(json!({
            "node_did": did, "profile_id": profile_id, "backend_id": backend_id,
            "model_name": "model-x",
        }))?;
        gents_documents::write_inference_profile_document(
            &access,
            &profile,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await?;
    }
    let profiles = || async {
        access
            .transact_write("test.profiles", |txn| {
                Box::pin(async move {
                    let mut records = Vec::new();
                    for id in ["on-a", "on-b", "compaction"] {
                        records.push(
                            gents_documents::read_desired_state_record_in_txn(
                                txn,
                                Collection::InferenceProfile,
                                did,
                                id,
                            )
                            .await?
                            .expect("profile"),
                        );
                    }
                    Ok::<_, anyhow::Error>(records)
                })
            })
            .await
            .unwrap()
    };
    let before = profiles().await;
    let remove = |credential_id: String| {
        let access = &access;
        async move {
            access
                .transact_write("test.remove", |txn| {
                    let credential_id = credential_id.clone();
                    Box::pin(
                        async move { remove_account_in_txn(txn, did, &credential_id).await },
                    )
                })
                .await
                .unwrap()
        }
    };

    store_sign_in(&access, claude_sign_in(did, "c"), None).await?;
    assert_eq!(profiles().await, before, "add C");
    store_sign_in(&access, claude_sign_in(did, "a"), None).await?;
    assert_eq!(profiles().await, before, "refresh A");
    store_sign_in(&access, claude_sign_in(did, "b"), Some("label-b2")).await?;
    assert_eq!(profiles().await, before, "relabel B");
    set_account_enabled(&access, did, &b.credential.credential_id, false).await?;
    assert_eq!(profiles().await, before, "disable B");
    remove(a.credential.credential_id.clone()).await;
    assert_eq!(profiles().await, before, "remove A");
    let d = store_sign_in(&access, claude_sign_in(did, "d"), None).await?;
    assert!(
        d.credential.account_ref.is_some(),
        "B remains, so D is added"
    );
    assert_eq!(profiles().await, before, "sign in D");

    for account in gents_inference::auth::credential::list_accounts(&access, did).await? {
        remove(account.credential_id).await;
    }
    let reused = store_sign_in(&access, claude_sign_in(did, "e"), None).await?;
    assert_eq!(reused.credential.account_ref, None, "the original slot");
    assert_eq!(reused.profiles, ["on-a"]);
    assert_eq!(profiles().await, before, "remove all, then sign in");
    let resolved = resolve_oauth_credential(
        &access,
        did,
        gents_inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
        AccountPick::Reference(None),
    )
    .await?
    .expect("the original slot resolves");
    assert_eq!(resolved.doc_id.as_deref(), Some(reused.doc_id.as_str()));

    let backends = access
        .transact_write("test.backends", |txn| {
            Box::pin(
                async move { gents_documents::list_inference_backends_in_txn(txn, did).await },
            )
        })
        .await?;
    for backend in &backends {
        gents_inference::backends::registry::record_discovered_catalog_on(&access, backend, Vec::new())
            .await?;
    }
    assert_eq!(profiles().await, before, "catalog refresh");
    let ConfigAccess::Local(local) = &access else {
        unreachable!()
    };
    let node = local.node();
    super::load_document_runtime_view(node, did).await?;
    assert_eq!(profiles().await, before, "view load");
    Ok(())
}
