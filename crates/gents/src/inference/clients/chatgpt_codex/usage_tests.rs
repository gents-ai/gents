use super::*;

const CODEX_USAGE_HEADERS: &[(&str, &str)] = &[
    ("x-codex-primary-used-percent", "12"),
    ("x-codex-primary-window-minutes", "300"),
];

async fn seed_codex_account(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    key: Option<&str>,
) -> crate::inference::auth::credential::OAuthCredential {
    crate::inference::auth::credential::test_support::seed_credential(
        node,
        node_did,
        CHATGPT_CODEX_PROVIDER,
        Utc::now() + chrono::Duration::hours(1),
    )
    .await;
    let mut row = crate::inference::auth::credential::resolve_oauth_credential(
        &crate::config_client::ConfigAccess::local(node.clone()),
        node_did,
        CHATGPT_CODEX_PROVIDER,
        crate::inference::auth::credential::AccountPick::Reference(None),
    )
    .await
    .unwrap()
    .expect("seeded row");
    if let Some(key) = key {
        row.provider_account_key = Some(key.to_string());
        crate::inference::auth::credential::upsert_oauth_credential(node, &row)
            .await
            .unwrap();
    }
    row
}

#[tokio::test]
async fn usage_wiring_codex_client_records_under_its_account_key() {
    use rig::client::CompletionClient;
    use rig::completion::CompletionModel;
    let did = "did:key:z6MkUsageWireCodexA";
    let cache: std::sync::Arc<crate::usage_observation::UsageCache> = std::sync::Arc::default();
    let node = crate::inference::auth::credential::test_support::test_node().await;
    let row = seed_codex_account(&node, did, Some("acct-key-a")).await;
    let url = crate::inference::clients::provider_http::tests::one_shot_server(
        "200 OK",
        CODEX_USAGE_HEADERS,
        "",
    )
    .await;
    let client = build_responses_client(cache.clone(), node.clone(), did, None, &url, None)
        .await
        .expect("client");
    let model = client.completion_model("gpt-5-codex");

    if let Ok(mut stream) = model.stream(model.completion_request("hi").build()).await {
        let _ = futures::StreamExt::next(&mut stream).await;
    }

    let stored = crate::inference::clients::provider_http::tests::stored_usage_eventually(
        &cache,
        &node,
        &crate::usage_observation::UsageAccount::for_credential(&row),
    )
    .await
    .unwrap();
    assert_eq!(stored.report.windows[0].used_pct, 12.0);
}

#[tokio::test]
async fn usage_wiring_codex_key_backfill_lands_under_the_key() {
    use rig::client::CompletionClient;
    use rig::completion::CompletionModel;
    let did = "did:key:z6MkUsageWireCodexKey";
    let cache: std::sync::Arc<crate::usage_observation::UsageCache> = std::sync::Arc::default();
    let node = crate::inference::auth::credential::test_support::test_node().await;
    let mut row = seed_codex_account(&node, did, None).await;
    let url = crate::inference::clients::provider_http::tests::server_for(
        2,
        "200 OK",
        CODEX_USAGE_HEADERS,
        "",
    )
    .await;
    let client = build_responses_client(cache.clone(), node.clone(), did, None, &url, None)
        .await
        .expect("client");
    let model = client.completion_model("gpt-5-codex");

    if let Ok(mut stream) = model.stream(model.completion_request("hi").build()).await {
        let _ = futures::StreamExt::next(&mut stream).await;
    }
    row.provider_account_key = Some("acct-key-a".into());
    crate::inference::auth::credential::upsert_oauth_credential(&node, &row)
        .await
        .unwrap();
    if let Ok(mut stream) = model.stream(model.completion_request("hi").build()).await {
        let _ = futures::StreamExt::next(&mut stream).await;
    }
    let backend: crate::document_config::InferenceBackend =
        serde_json::from_value(serde_json::json!({
            "node_did": did,
            "backend_id": "backend-usage-a",
            "name": "backend-usage-a",
            "provider_kind": "ChatGptCodex",
            "endpoint": url,
            "auth": { "kind": "node_oauth" },
        }))
        .unwrap();
    let stored = crate::usage_observation::usage_for_backend(
        &cache,
        &crate::config_client::ConfigAccess::local(node.clone()),
        did,
        &backend,
    )
    .await
    .unwrap()
    .expect("usage for the backend");
    assert_eq!(stored.report.windows[0].used_pct, 12.0);
}
