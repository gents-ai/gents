use super::*;

#[tokio::test]
async fn list_accounts_is_ordered_token_free_and_marks_the_default() {
    let access = access().await;
    let did = "did:key:z6MkTestListAccounts";
    let a = store(
        &access,
        Product::Claude.sign_in(did, "a", "refresh-SECRET-a"),
    )
    .await;
    store(
        &access,
        Product::Claude.sign_in(did, "b", "refresh-SECRET-b"),
    )
    .await;
    store(
        &access,
        Product::Claude.sign_in(did, "c", "refresh-SECRET-c"),
    )
    .await;
    store(&access, Product::Grok.sign_in(did, "a", "refresh-SECRET-g")).await;
    set_enabled(&access, &a.credential.credential_id, false).await;

    let listed = list_accounts(&access, did).await.unwrap();
    let rows = list_oauth_credentials_on(&access, did).await.unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|account| &account.credential_id)
            .collect::<Vec<_>>(),
        rows.iter()
            .map(|row| &row.credential_id)
            .collect::<Vec<_>>()
    );
    for provider in [CLAUDE_OAUTH_PROVIDER, XAI_OAUTH_PROVIDER] {
        let default =
            resolve_oauth_credential(&access, did, provider, AccountPick::ProviderDefault)
                .await
                .unwrap()
                .unwrap();
        let marked: Vec<_> = listed
            .iter()
            .filter(|account| account.provider == provider && account.default)
            .map(|account| account.credential_id.clone())
            .collect();
        assert_eq!(marked, [default.credential_id], "{provider}");
    }
    let claude: Vec<_> = listed
        .iter()
        .filter(|account| account.provider == CLAUDE_OAUTH_PROVIDER)
        .collect();
    let mut labels: Vec<_> = claude
        .iter()
        .map(|account| account.label.as_str())
        .collect();
    labels.sort();
    assert_eq!(labels, ["Claude", "Claude 2", "Claude 3"]);
    let original = claude
        .iter()
        .find(|account| account.account_ref.is_none())
        .unwrap();
    assert!(!original.enabled);
    assert_eq!(original.identity.as_deref(), Some("label-a"));
    let text = serde_json::to_string(&listed).unwrap();
    assert!(
        !text.contains("SECRET") && !text.contains("access-TEST"),
        "{text}"
    );
}

#[tokio::test]
async fn a_cloud_workspace_token_is_not_an_account() {
    let access = access().await;
    let did = "did:key:z6MkTestCloudToken";
    let mut cloud = Product::Grok.sign_in(did, "a", "refresh-1");
    cloud.provider = "gents-cloud".into();
    cloud.credential_id = oauth_credential_id(did, "gents-cloud");
    upsert_oauth_credential_on(&access, &cloud).await.unwrap();
    let before = list_oauth_credentials_on(&access, did).await.unwrap();

    assert!(list_accounts(&access, did).await.unwrap().is_empty());
    let id = cloud.credential_id.as_str();
    for error in [
        set_account_label(&access, did, id, "Work")
            .await
            .unwrap_err(),
        set_account_enabled(&access, did, id, false)
            .await
            .unwrap_err(),
        remove(&access, did, id).await.unwrap_err(),
    ] {
        assert!(error.to_string().contains("not found"), "{error}");
    }
    assert_eq!(
        list_oauth_credentials_on(&access, did).await.unwrap(),
        before
    );
}

#[tokio::test]
async fn a_label_change_is_validated_unique_and_renames_the_backend() {
    let access = access().await;
    let did = "did:key:z6MkTestSetLabel";
    store(&access, Product::Claude.sign_in(did, "a", "refresh-a")).await;
    let b = store(&access, Product::Claude.sign_in(did, "b", "refresh-b")).await;
    let id = b.credential.credential_id.as_str();

    set_account_label(&access, did, id, " Work ").await.unwrap();
    let row = rows(&access, did, CLAUDE_OAUTH_PROVIDER)
        .await
        .into_iter()
        .find(|row| row.credential_id == id)
        .unwrap();
    assert_eq!(row.label.as_deref(), Some("Work"));
    let backend = backends(&access, did).await.remove(0);
    assert_eq!(backend.name, "Work");

    for bad in ["Claude", "", "a\u{7}b"] {
        assert!(
            set_account_label(&access, did, id, bad).await.is_err(),
            "{bad:?}"
        );
    }

    let mut custom = backend.clone();
    custom.name = "Custom".into();
    crate::config_client::write_inference_backend_document(&access, &custom)
        .await
        .unwrap();
    set_account_label(&access, did, id, "Home").await.unwrap();
    assert_eq!(backends(&access, did).await.remove(0).name, "Custom");
}

#[tokio::test]
async fn disable_keeps_the_tokens() {
    let access = access().await;
    let did = "did:key:z6MkTestDisable";
    let a = store(&access, Product::Grok.sign_in(did, "a", "refresh-a")).await;
    let before = rows(&access, did, XAI_OAUTH_PROVIDER).await.remove(0);
    set_account_enabled(&access, did, &a.credential.credential_id, false)
        .await
        .unwrap();
    let after = rows(&access, did, XAI_OAUTH_PROVIDER).await.remove(0);
    assert!(!after.enabled);
    assert_eq!(after.access_token, before.access_token);
    assert_eq!(after.refresh_token, before.refresh_token);
    set_account_enabled(&access, did, &a.credential.credential_id, true)
        .await
        .unwrap();
    assert!(rows(&access, did, XAI_OAUTH_PROVIDER).await[0].enabled);
}

#[tokio::test]
async fn remove_deletes_the_row() {
    let access = access().await;
    let did = "did:key:z6MkTestRemove";
    let a = store(&access, Product::ChatGpt.sign_in(did, "a", "refresh-a")).await;
    let b = store(&access, Product::ChatGpt.sign_in(did, "b", "refresh-b")).await;
    remove(&access, did, &b.credential.credential_id)
        .await
        .unwrap();
    let left = rows(&access, did, CHATGPT_CODEX_PROVIDER).await;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].credential_id, a.credential.credential_id);
    let error = remove(&access, did, &b.credential.credential_id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error}");
}

#[tokio::test]
async fn a_sign_in_after_removing_the_only_account_takes_the_original_slot() {
    let access = access().await;
    let did = "did:key:z6MkTestSlotReuse";
    for product in PRODUCTS {
        let spec = preset(product);
        let original = crate::InferenceBackend {
            node_did: did.to_owned(),
            backend_id: format!("{}-original", product.provider()),
            name: format!("{product:?}"),
            provider_kind: spec.provider_kind,
            openai_wire_api: spec.openai_wire_api,
            endpoint: spec.endpoint,
            auth: crate::document_config::BackendAuth::NodeOAuth { account_ref: None },
            connect_timeout_secs: None,
            discovery_timeout_secs: None,
            max_concurrent: None,
            max_queue_depth: None,
            enabled: true,
            tags: Vec::new(),
        };
        crate::config_client::write_inference_backend_document(&access, &original)
            .await
            .unwrap();
        let profile = serde_json::from_value(json!({
            "node_did": did,
            "profile_id": format!("{product:?}-profile"),
            "backend_id": original.backend_id,
            "model_name": "model-x",
        }))
        .unwrap();
        crate::config_client::write_inference_profile_document(&access, &profile)
            .await
            .unwrap();
    }
    let a = store(&access, Product::Claude.sign_in(did, "a", "refresh-a")).await;
    remove(&access, did, &a.credential.credential_id)
        .await
        .unwrap();
    let b = store(&access, Product::Claude.sign_in(did, "b", "refresh-b")).await;
    assert_eq!(b.result, SignInResult::Added);
    assert_eq!(b.credential.credential_id, a.credential.credential_id);
    assert_eq!(b.credential.account_ref, None);
    assert_ne!(b.doc_id, a.doc_id);
    let stored = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].refresh_token, "refresh-b");
    assert_eq!(b.profiles, ["Claude-profile"]);
    let note = b.profiles_note().expect("the sign-in names its profiles");
    assert!(note.contains("Claude-profile"), "{note}");

    let added = store(&access, Product::Claude.sign_in(did, "c", "refresh-c")).await;
    assert!(added.profiles.is_empty());
    assert_eq!(added.profiles_note(), None);
}

#[tokio::test]
async fn another_nodes_account_is_not_touched() {
    let access = access().await;
    let mine = "did:key:z6MkTestMine";
    let theirs = "did:key:z6MkTestTheirs";
    let signed = store(&access, Product::Claude.sign_in(theirs, "a", "refresh-a")).await;
    let id = signed.credential.credential_id.as_str();
    let before = rows(&access, theirs, CLAUDE_OAUTH_PROVIDER).await;
    for error in [
        set_account_label(&access, mine, id, "Work")
            .await
            .unwrap_err(),
        set_account_enabled(&access, mine, id, false)
            .await
            .unwrap_err(),
        remove(&access, mine, id).await.unwrap_err(),
    ] {
        assert!(error.to_string().contains("not found"), "{error}");
    }
    assert_eq!(rows(&access, theirs, CLAUDE_OAUTH_PROVIDER).await, before);
}

#[tokio::test]
async fn a_provider_no_backend_reads_is_refused() {
    let access = access().await;
    let did = "did:key:z6MkTestOtherProvider";
    let mut credential = Product::Grok.sign_in(did, "a", "refresh-1");
    credential.provider = "other-provider".into();
    credential.credential_id = oauth_credential_id(did, "other-provider");
    assert!(store_sign_in(&access, credential, None).await.is_err());
    assert!(list_oauth_credentials_on(&access, did)
        .await
        .unwrap()
        .is_empty());
}
