use rig::completion::ToolDefinition;

use super::*;
use crate::llm::message::Message;
use crate::oauth_credential::test_support::{seed_credential, test_node};

fn request_from_native(
    preamble: Option<&str>,
    history: Vec<Message>,
    tools: Vec<ToolDefinition>,
) -> CompletionRequest {
    let rig_history = crate::llm::rig_compat::to_rig_messages(&history);
    CompletionRequest {
        model: None,
        preamble: preamble.map(str::to_string),
        chat_history: OneOrMany::many(rig_history).expect("at least one row"),
        documents: Vec::new(),
        tools,
        temperature: None,
        max_tokens: Some(128),
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    }
}

fn ping_request() -> CompletionRequest {
    request_from_native(None, vec![Message::user("ping")], Vec::new())
}

fn echo_tool_request() -> CompletionRequest {
    request_from_native(
        None,
        vec![Message::user("use echo")],
        vec![ToolDefinition {
            name: "echo".into(),
            description: "echo".into(),
            parameters: serde_json::json!({"type":"object","properties":{}}),
        }],
    )
}

fn test_client() -> ClaudeSubscriptionClient<StaticBearer> {
    ClaudeSubscriptionClient::with_bearer(Arc::new(StaticBearer::failing("no credential")))
}

#[tokio::test]
async fn fixture_stream_maps_tool_use_without_touching_the_bearer() {
    let _guard = crate::claude_messages::lock_fixtures_for_test().await;
    crate::claude_messages::install_messages_sse_fixtures(vec![
        crate::claude_messages::sse_fixture_tool_use("toolu_1", "echo", "{\"text\":\"hi\"}"),
    ]);
    let client = test_client();
    let model = client.completion_model("claude-sonnet-5");
    let mut stream = model.stream(echo_tool_request()).await.expect("stream");
    use rig::streaming::StreamedAssistantContent;
    let mut calls = Vec::new();
    let mut usage = None;
    while let Some(item) = stream.next().await {
        match item.expect("chunk") {
            StreamedAssistantContent::ToolCall { tool_call, .. } => {
                calls.push((
                    tool_call.id,
                    tool_call.function.name,
                    tool_call.function.arguments,
                ));
            }
            StreamedAssistantContent::Final(final_response) => {
                usage = final_response.token_usage();
                break;
            }
            other => panic!("unexpected chunk: {other:?}"),
        }
    }
    assert_eq!(
        calls,
        vec![(
            "toolu_1".to_string(),
            "echo".to_string(),
            serde_json::json!({"text": "hi"})
        )]
    );
    let usage = usage.expect("usage from message_delta");
    assert_eq!((usage.input_tokens, usage.output_tokens), (10, 5));
    assert_eq!(
        client.accounts[0].bearer.bearer_calls(),
        0,
        "fixtures never consult the bearer"
    );
}

#[tokio::test]
async fn messages_http_fixture_streams_text_turn_on_empty_surface() {
    let _guard = crate::claude_messages::lock_fixtures_for_test().await;
    crate::claude_messages::install_messages_sse_fixtures(vec![
        crate::claude_messages::sse_fixture_final_text("pong"),
    ]);
    let model = test_client().completion_model("claude-sonnet-5");
    let response = model.completion(ping_request()).await.expect("text turn");
    let text = response
        .choice
        .iter()
        .filter_map(|content| match content {
            rig::completion::AssistantContent::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(text, "pong");
}

#[tokio::test]
async fn live_path_without_fixture_asks_the_bearer_and_fails_closed_on_bearer_error() {
    let _guard = crate::claude_messages::lock_fixtures_for_test().await;
    let client =
        ClaudeSubscriptionClient::with_bearer(Arc::new(StaticBearer::failing("no credential")));
    let model = client.completion_model("claude-sonnet-5");
    let err = model
        .completion(ping_request())
        .await
        .expect_err("bearer error");
    assert!(
        err.to_string()
            .contains("Claude Messages bearer: no credential"),
        "{err}"
    );
    assert_eq!(client.accounts[0].bearer.bearer_calls(), 1);
}

#[tokio::test]
async fn fixture_queue_serves_one_body_per_call_then_asks_the_bearer() {
    let _guard = crate::claude_messages::lock_fixtures_for_test().await;
    crate::claude_messages::install_messages_sse_fixtures(vec![
        crate::claude_messages::sse_fixture_final_text("one"),
    ]);
    let client =
        ClaudeSubscriptionClient::with_bearer(Arc::new(StaticBearer::failing("no credential")));
    let model = client.completion_model("claude-sonnet-5");
    model
        .completion(ping_request())
        .await
        .expect("first call served");
    assert_eq!(client.accounts[0].bearer.bearer_calls(), 0);
    let err = model
        .completion(ping_request())
        .await
        .expect_err("queue drained → live bearer read");
    assert!(
        err.to_string()
            .contains("Claude Messages bearer: no credential"),
        "{err}"
    );
    assert_eq!(client.accounts[0].bearer.bearer_calls(), 1);
}

#[tokio::test]
async fn build_without_a_credential_fails_closed_with_the_login_hint() {
    let node = test_node().await;
    let err = ClaudeSubscriptionClient::build(
        std::sync::Arc::default(),
        node,
        "did:key:z6MkNobody",
        None,
    )
    .await
    .expect_err("missing");
    assert!(
        err.to_string()
            .contains("gents claude-login --node-did did:key:z6MkNobody"),
        "{err:#}"
    );
}

#[tokio::test]
async fn build_with_a_seeded_credential_yields_a_shared_bearer() {
    let node = test_node().await;
    seed_credential(
        &node,
        "did:key:z6MkSeeded",
        crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
        chrono::Utc::now() + chrono::Duration::hours(8),
    )
    .await;
    let client = ClaudeSubscriptionClient::build(
        std::sync::Arc::default(),
        node.clone(),
        "did:key:z6MkSeeded",
        None,
    )
    .await
    .expect("client");
    let again = ClaudeSubscriptionClient::build(
        std::sync::Arc::default(),
        node,
        "did:key:z6MkSeeded",
        None,
    )
    .await
    .expect("client");
    assert!(
        Arc::ptr_eq(&client.accounts[0].bearer, &again.accounts[0].bearer),
        "one bearer per credential id"
    );
    assert_eq!(
        client.accounts[0]
            .bearer
            .current_bearer()
            .await
            .expect("token"),
        "access-TEST"
    );
}

#[tokio::test]
async fn stale_credential_refreshes_once_through_the_claude_token_endpoint() {
    let node = test_node().await;
    seed_credential(
        &node,
        "did:key:z6MkStaleRefresh",
        crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
        chrono::Utc::now() - chrono::Duration::minutes(1),
    )
    .await;
    let (url, handle) = crate::oauth_credential::test_support::one_shot_token_server(
        200,
        r#"{"access_token":"access-NEW","refresh_token":"refresh-NEW","expires_in":28800}"#,
    )
    .await;
    let _env = crate::oauth_credential::test_support::TOKEN_URL_ENV
        .lock()
        .await;
    std::env::set_var(
        crate::claude_oauth::CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV,
        &url,
    );
    let client = ClaudeSubscriptionClient::build(
        std::sync::Arc::default(),
        node,
        "did:key:z6MkStaleRefresh",
        None,
    )
    .await
    .expect("client");
    let bearer = client.accounts[0].bearer.current_bearer().await;
    std::env::remove_var(crate::claude_oauth::CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV);
    assert_eq!(bearer.expect("token"), "access-NEW");

    let request = handle.await.expect("server");
    let body: serde_json::Value =
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["grant_type"], "refresh_token");
    assert_eq!(body["refresh_token"], "refresh-TEST");
    assert_eq!(body["client_id"], gents_claude::oauth::CLIENT_ID);

    assert_eq!(
        client.accounts[0]
            .bearer
            .current_bearer()
            .await
            .expect("cached"),
        "access-NEW",
        "a second read must not refresh again"
    );
}

#[test]
fn kind_round_trips_and_is_agent_scoped() {
    assert_eq!(
        crate::BackendProviderKind::parse_optional(Some("ClaudeCliSubscription")).unwrap(),
        crate::BackendProviderKind::ClaudeCliSubscription
    );
    assert!(crate::BackendProviderKind::parse_optional(Some("claude-cli-subscription")).is_err());
    assert_eq!(
        crate::BackendProviderKind::ClaudeCliSubscription.as_str(),
        "ClaudeCliSubscription"
    );
    assert!(crate::BackendProviderKind::ClaudeCliSubscription.is_node_scoped_oauth());
}

#[test]
fn anthropic_api_key_is_a_shared_key_kind_on_the_messages_wire() {
    use crate::BackendProviderKind as Kind;
    let kind = Kind::parse_optional(Some("AnthropicApiKey")).unwrap();
    assert_eq!(kind, Kind::AnthropicApiKey);
    assert_eq!(kind.as_str(), "AnthropicApiKey");
    assert!(Kind::ALL.contains(&kind));
    assert!(!kind.is_node_scoped_oauth());
    let messages_kinds: Vec<_> = Kind::ALL
        .into_iter()
        .filter(|kind| kind.uses_messages_wire())
        .collect();
    assert_eq!(
        messages_kinds,
        vec![Kind::ClaudeCliSubscription, Kind::AnthropicApiKey]
    );
    assert_eq!(
        gents_loop::provider_input::ProviderInputProfile::resolve(
            kind,
            crate::OpenAiWireApi::ChatCompletions
        ),
        gents_loop::provider_input::ProviderInputProfile::ClaudeMessages
    );
}

#[tokio::test]
async fn usage_wiring_claude_built_client_holds_its_account_reporter() {
    let did = "did:key:z6MkUsageWireClaude";
    let node = crate::oauth_credential::test_support::test_node().await;
    let credential = crate::oauth_credential::OAuthCredential {
        doc_id: None,
        credential_id: format!(
            "{}:acct-ref-2",
            crate::oauth_credential::oauth_credential_id(did, CLAUDE_OAUTH_PROVIDER)
        ),
        node_did: did.to_string(),
        provider: CLAUDE_OAUTH_PROVIDER.to_string(),
        access_token: "access-TEST".into(),
        refresh_token: "refresh-TEST".into(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        last_refresh: None,
        enabled: true,
        account_ref: Some("acct-ref-2".into()),
        connected_at: None,
        provider_account_key: None,
        label: None,
    };
    let doc_id = crate::oauth_credential::upsert_oauth_credential(&node, &credential)
        .await
        .unwrap();

    let client =
        ClaudeSubscriptionClient::build(std::sync::Arc::default(), node, did, Some("acct-ref-2"))
            .await
            .expect("client");

    assert_eq!(
        client.accounts[0]
            .usage
            .as_ref()
            .map(|reporter| reporter.account.clone()),
        Some(crate::usage_observation::UsageAccount::Credential {
            doc_id: Some(doc_id),
            node_did: did.to_string(),
            provider: CLAUDE_OAUTH_PROVIDER.to_string(),
            account_ref: Some("acct-ref-2".into()),
        })
    );
}

mod pool_client {
    use super::*;
    use crate::usage_observation::now_unix_secs;
    use gents_claude::rotation::{Family, Policy};
    use gents_loop::claude_messages_body::SESSION_KEY_PARAM;
    use rapidhash::HashSetExt;

    const REJECTED: &[(&str, &str)] = &[
        ("anthropic-ratelimit-unified-status", "rejected"),
        ("anthropic-ratelimit-unified-reset", "4102444800"),
        (
            "anthropic-ratelimit-unified-representative-claim",
            "five_hour",
        ),
    ];

    async fn pool_model(
        ids: &[&str],
    ) -> (
        Arc<UsageCache>,
        ClaudeSubscriptionClient<StaticBearer>,
        ClaudeSubscriptionModel<StaticBearer>,
    ) {
        let cache = Arc::new(UsageCache::default());
        let node = test_node().await;
        let accounts: Vec<_> = ids
            .iter()
            .map(|id| ClaudeAccount {
                credential_id: (*id).to_string(),
                bearer: Arc::new(StaticBearer::new(id)),
                identity: AccountIdentity::of_api_key("did"),
                usage: Some(UsageReporter::new(
                    cache.clone(),
                    node.clone(),
                    UsageAccount::Backend {
                        node_did: "did".into(),
                        provider: "claude".into(),
                        backend_id: (*id).to_string(),
                        connection: "c".into(),
                    },
                )),
            })
            .collect();
        let credential_ids: Arc<[String]> = ids.iter().map(|id| id.to_string()).collect();
        let client = ClaudeSubscriptionClient {
            accounts: accounts.into(),
            rotation: Some(Rotation {
                policy: Policy::default(),
                cache: cache.clone(),
                credential_ids,
            }),
        };
        let model = client.completion_model("claude-sonnet-5");
        (cache, client, model)
    }

    fn session_request(session: &str) -> CompletionRequest {
        let mut request = ping_request();
        request.additional_params = Some(serde_json::json!({ SESSION_KEY_PARAM: session }));
        request
    }

    fn choose(cache: &UsageCache, ids: &[String], session: &str) -> Option<String> {
        cache
            .claude_accounts()
            .choose(
                ids,
                session,
                Family::Sonnet,
                now_unix_secs(),
                &Policy::default(),
            )
            .map(|choice| choice.key)
    }

    #[tokio::test]
    async fn a_rejected_account_hands_the_retry_to_the_next_one() {
        let _guard = crate::claude_messages::lock_fixtures_for_test().await;
        let url = crate::provider_http::tests::one_shot_server(
            "429 Too Many Requests",
            REJECTED,
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"limit"}}"#,
        )
        .await;
        let (cache, _client, model) = pool_model(&["id-a", "id-b"]).await;
        let rotation = model.rotation.as_ref().expect("rotation");
        let err = open_rotated(
            &url,
            "claude-sonnet-5",
            &session_request("s"),
            RapidHashSet::new(),
            &model.accounts,
            rotation,
        )
        .await
        .err()
        .expect("429");
        let text = err.to_string();
        assert!(text.contains("account-rotated"), "{text}");
        assert!(text.contains("usage-exhausted"), "{text}");
        let failure = crate::llm::rig_compat::classify_completion_error(
            &rig::agent::StreamingError::Completion(err),
        );
        assert!(failure.report.account_limited);
        assert_eq!(
            gents_loop::completion_retry::failure_class(&failure),
            gents_loop::completion_retry::FailureClass::AccountLimited
        );
        let ids: Vec<String> = vec!["id-a".into(), "id-b".into()];
        assert_eq!(choose(&cache, &ids, "s").as_deref(), Some("id-b"));
    }

    #[tokio::test]
    async fn every_account_exhausted_is_a_usage_limit_with_the_earliest_reset() {
        let (cache, _client, model) = pool_model(&["id-a", "id-b"]).await;
        let now = now_unix_secs();
        cache
            .claude_accounts()
            .reject(&"id-a".to_string(), now, 4_102_444_800);
        cache
            .claude_accounts()
            .reject(&"id-b".to_string(), now, 4_102_448_400);
        let rotation = model.rotation.as_ref().expect("rotation");
        let err = open_rotated(
            "http://127.0.0.1:1",
            "claude-sonnet-5",
            &ping_request(),
            RapidHashSet::new(),
            &model.accounts,
            rotation,
        )
        .await
        .err()
        .expect("exhausted");
        let failure = crate::llm::rig_compat::classify_completion_error(
            &rig::agent::StreamingError::Completion(err),
        );
        assert!(!failure.report.account_limited);
        match failure.error {
            crate::error::InferenceError::UsageLimited(limit) => assert_eq!(
                limit.resets_at.map(|at| at.timestamp()),
                Some(4_102_444_800)
            ),
            other => panic!("expected a usage limit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sessions_stick_to_the_account_the_pool_chose() {
        let _guard = crate::claude_messages::lock_fixtures_for_test().await;
        crate::claude_messages::install_messages_sse_fixtures(vec![
            crate::claude_messages::sse_fixture_final_text("one"),
            crate::claude_messages::sse_fixture_final_text("two"),
        ]);
        let (cache, _client, model) = pool_model(&["id-a", "id-b"]).await;
        model.completion(session_request("s1")).await.expect("s1");
        model.completion(session_request("s2")).await.expect("s2");
        let ids: Vec<String> = vec!["id-a".into(), "id-b".into()];
        assert_eq!(choose(&cache, &ids, "s1").as_deref(), Some("id-a"));
        assert_eq!(choose(&cache, &ids, "s2").as_deref(), Some("id-b"));
    }

    #[tokio::test]
    async fn a_single_account_client_shares_its_accounts_and_has_no_rotation() {
        let (_cache, mut client, _model) = pool_model(&["id-a"]).await;
        client.rotation = None;
        let model = client.completion_model("claude-sonnet-5");
        assert!(Arc::ptr_eq(&model.accounts, &client.accounts));
        assert!(model.rotation.is_none());
    }
}
