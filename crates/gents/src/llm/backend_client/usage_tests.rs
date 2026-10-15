use super::tests::test_agent;
use super::*;

async fn one_openai_completion(client: BackendClient) {
    use rig::client::CompletionClient;
    use rig::completion::CompletionModel;
    match client {
        BackendClient::OpenAiChatCompletions(client) => {
            let model = client.completion_model("model-a");
            if let Ok(mut stream) = model.stream(model.completion_request("hi").build()).await {
                let _ = futures::StreamExt::next(&mut stream).await;
            }
        }
        BackendClient::OpenAiResponses(client) => {
            let model = client.completion_model("model-a");
            if let Ok(mut stream) = model.stream(model.completion_request("hi").build()).await {
                let _ = futures::StreamExt::next(&mut stream).await;
            }
        }
        _ => panic!("expected an OpenAI-compatible client"),
    }
}

const OPENAI_USAGE_HEADERS: &[(&str, &str)] = &[
    ("x-ratelimit-limit-requests", "100"),
    ("x-ratelimit-remaining-requests", "75"),
];

#[tokio::test]
async fn usage_wiring_openai_compatible_records_under_the_backend_id() {
    let node = Arc::new(crate::oauth_credential::test_support::test_node().await);
    for (wire, backend_id) in [
        (crate::OpenAiWireApi::ChatCompletions, "backend-usage-chat"),
        (crate::OpenAiWireApi::Responses, "backend-usage-responses"),
    ] {
        let url =
            crate::provider_http::tests::one_shot_server("200 OK", OPENAI_USAGE_HEADERS, "").await;
        let mut behavior = test_agent(BackendProviderKind::OpenAiCompatible, wire);
        behavior.backend_id = Some(backend_id.to_string());
        behavior.backend_endpoint = format!("{url}/v1");
        let client =
            build_backend_client(node.clone(), &behavior, "sk-TEST", Duration::from_secs(1))
                .await
                .expect("client");

        one_openai_completion(client).await;

        let account = crate::usage_observation::UsageAccount::Backend {
            node_did: behavior.node_did().to_string(),
            provider: "OpenAiCompatible".to_string(),
            backend_id: backend_id.to_string(),
        };
        let stored = crate::provider_http::tests::stored_usage_eventually(&node, &account)
            .await
            .unwrap_or_else(|| panic!("usage recorded for {backend_id}"));
        let window = &stored.report.windows[0];
        assert_eq!((window.label.as_str(), window.used_pct), ("requests", 25.0));
    }
}

#[tokio::test]
async fn usage_wiring_backend_without_id_has_no_reporter() {
    let node = Arc::new(crate::oauth_credential::test_support::test_node().await);
    let url =
        crate::provider_http::tests::one_shot_server("200 OK", OPENAI_USAGE_HEADERS, "").await;
    let mut behavior = test_agent(
        BackendProviderKind::OpenAiCompatible,
        crate::OpenAiWireApi::ChatCompletions,
    );
    behavior.backend_id = None;
    behavior.backend_endpoint = format!("{url}/v1");
    let client = build_backend_client(node.clone(), &behavior, "sk-TEST", Duration::from_secs(1))
        .await
        .expect("client");

    one_openai_completion(client).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let rows = crate::config_client::ConfigAccess::Local(node)
        .execute("{ ProviderAccountUsage { usage_key } }")
        .await
        .unwrap();
    assert_eq!(rows["data"]["ProviderAccountUsage"], serde_json::json!([]));
}
