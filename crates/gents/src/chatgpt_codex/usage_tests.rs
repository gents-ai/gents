use super::*;

const CODEX_USAGE_HEADERS: &[(&str, &str)] = &[
    ("x-codex-primary-used-percent", "12"),
    ("x-codex-primary-window-minutes", "300"),
];

/// A signed-in Codex account for `agent_did`, keyed `key` when given.
async fn seed_codex_account(
    node: &Arc<EmbeddedNode>,
    agent_did: &str,
    key: Option<&str>,
) -> crate::oauth_credential::OAuthCredential {
    crate::oauth_credential::test_support::seed_credential(
        node,
        agent_did,
        CHATGPT_CODEX_PROVIDER,
        Utc::now() + chrono::Duration::hours(1),
    )
    .await;
    let mut row = crate::oauth_credential::resolve_oauth_credential(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        agent_did,
        CHATGPT_CODEX_PROVIDER,
        crate::oauth_credential::AccountPick::Reference(None),
    )
    .await
    .unwrap()
    .expect("seeded row");
    if let Some(key) = key {
        row.provider_account_key = Some(key.to_string());
        crate::oauth_credential::upsert_oauth_credential(node, &row)
            .await
            .unwrap();
    }
    row
}

/// Usage keys stored for `agent_did`, polled until `key` shows or 5 s pass.
async fn usage_keys_until(node: &Arc<EmbeddedNode>, agent_did: &str, key: &str) -> Vec<String> {
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let response = access
            .execute(&format!(
                r#"{{ ProviderAccountUsage(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{ usage_key }} }}"#
            ))
            .await
            .unwrap();
        let mut keys: Vec<String> = response["data"]["ProviderAccountUsage"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["usage_key"].as_str().map(str::to_string))
            .collect();
        keys.sort();
        if keys.iter().any(|stored| stored == key) || tokio::time::Instant::now() >= deadline {
            return keys;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn usage_wiring_codex_client_records_under_its_account_key() {
    use rig::client::CompletionClient;
    use rig::completion::CompletionModel;
    let did = "did:key:z6MkUsageWireCodexA";
    let node = Arc::new(crate::oauth_credential::test_support::test_node().await);
    seed_codex_account(&node, did, Some("acct-key-a")).await;
    let url = crate::provider_http::tests::one_shot_server("200 OK", CODEX_USAGE_HEADERS, "").await;
    let client = build_responses_client(node.clone(), did, None, &url)
        .await
        .expect("client");
    let model = client.completion_model("gpt-5-codex");

    if let Ok(mut stream) = model.stream(model.completion_request("hi").build()).await {
        let _ = futures::StreamExt::next(&mut stream).await;
    }

    assert_eq!(
        usage_keys_until(&node, did, "acct-key-a").await,
        vec!["acct-key-a"]
    );
}

#[tokio::test]
async fn usage_wiring_codex_key_backfill_lands_under_the_key() {
    use rig::client::CompletionClient;
    use rig::completion::CompletionModel;
    let did = "did:key:z6MkUsageWireCodexKey";
    let node = Arc::new(crate::oauth_credential::test_support::test_node().await);
    let mut row = seed_codex_account(&node, did, None).await;
    let url = crate::provider_http::tests::server_for(2, "200 OK", CODEX_USAGE_HEADERS, "").await;
    let client = build_responses_client(node.clone(), did, None, &url)
        .await
        .expect("client");
    let model = client.completion_model("gpt-5-codex");

    if let Ok(mut stream) = model.stream(model.completion_request("hi").build()).await {
        let _ = futures::StreamExt::next(&mut stream).await;
    }
    let reference = format!("ref:original:{}", row.doc_id.as_deref().unwrap());
    assert_eq!(
        usage_keys_until(&node, did, &reference).await,
        vec![reference.clone()]
    );

    row.provider_account_key = Some("acct-key-a".into());
    crate::oauth_credential::upsert_oauth_credential(&node, &row)
        .await
        .unwrap();
    if let Ok(mut stream) = model.stream(model.completion_request("hi").build()).await {
        let _ = futures::StreamExt::next(&mut stream).await;
    }
    assert_eq!(
        usage_keys_until(&node, did, "acct-key-a").await,
        vec!["acct-key-a".to_string(), reference]
    );

    let backend: crate::document_config::InferenceBackend =
        serde_json::from_value(serde_json::json!({
            "agent_did": did,
            "backend_id": "backend-usage-a",
            "name": "backend-usage-a",
            "provider_kind": "ChatGptCodex",
            "endpoint": url,
            "auth": { "kind": "principal_oauth" },
        }))
        .unwrap();
    let stored = crate::usage_observation::usage_for_backend(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        did,
        &backend,
    )
    .await
    .unwrap()
    .expect("usage for the backend");
    assert_eq!(stored.report.windows[0].used_pct, 12.0);
}
