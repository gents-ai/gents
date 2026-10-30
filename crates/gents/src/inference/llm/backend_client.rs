use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

use crate::inference::backends::provider::BackendProviderKind;
use crate::inference::resolved::ResolvedAgent;

pub(crate) enum BackendClient {
    OpenAiChatCompletions(
        rig::providers::openai::CompletionsClient<
            crate::inference::clients::inference_http::SessionTaggingHttpClient<
                crate::inference::rendered_request::RenderedRequestCapturingHttpClient<
                    crate::inference::clients::provider_http::ProviderHttpClient,
                >,
            >,
        >,
    ),
    OpenAiResponses(
        rig::providers::openai::Client<
            crate::inference::clients::inference_http::SessionTaggingHttpClient<
                crate::inference::clients::inference_http::ResponsesNormalizingHttpClient<
                    crate::inference::rendered_request::RenderedRequestCapturingHttpClient<
                        crate::inference::clients::provider_http::ProviderHttpClient,
                    >,
                >,
            >,
        >,
    ),
    OpenRouter(
        rig::providers::openrouter::Client<
            crate::inference::rendered_request::RenderedRequestCapturingHttpClient<
                crate::inference::clients::provider_http::ProviderHttpClient,
            >,
        >,
    ),
    ChatGptCodex(
        rig::providers::openai::Client<
            crate::inference::clients::chatgpt_codex::ChatGptCodexHttpClient<
                crate::inference::auth::credential::DbCredentialBearer,
                crate::inference::rendered_request::RenderedRequestCapturingHttpClient<
                    crate::inference::clients::provider_http::ProviderHttpClient,
                >,
            >,
        >,
    ),
    XaiGrokChatCompletions(
        rig::providers::openai::CompletionsClient<
            crate::inference::clients::xai_grok::CapturingXaiGrokOAuthHttpClient,
        >,
    ),
    XaiGrokResponses(
        rig::providers::openai::Client<
            crate::inference::clients::xai_grok::CapturingXaiGrokOAuthHttpClient,
        >,
    ),
    ClaudeSubscription(
        crate::inference::clients::claude_subscription::ClaudeSubscriptionClient<
            crate::inference::auth::credential::DbCredentialBearer,
        >,
    ),
    AnthropicApiKey(
        crate::inference::clients::claude_subscription::ClaudeSubscriptionClient<
            crate::inference::clients::claude_subscription::ApiKeyBearer,
        >,
    ),
}

impl BackendClient {
    pub(crate) fn replay_issuer(
        &self,
    ) -> Result<Option<gents_loop::claude_messages_body::ReplayIssuer>> {
        let request = match self {
            Self::OpenAiChatCompletions(client) => client.post("/chat/completions")?,
            Self::OpenAiResponses(client) => client.post("/responses")?,
            Self::OpenRouter(client) => client.post("/chat/completions")?,
            Self::ChatGptCodex(client) => client.post("/responses")?,
            Self::XaiGrokChatCompletions(client) => client.post("/chat/completions")?,
            Self::XaiGrokResponses(client) => client.post("/responses")?,
            Self::ClaudeSubscription(_) => return claude_subscription_replay_issuer(),
            Self::AnthropicApiKey(_) => {
                return messages_replay_issuer(BackendProviderKind::AnthropicApiKey)
            }
        }
        .body(())?;
        Ok(
            gents_loop::rendered_request::transport::replay_issuer_for_destination(
                self.provider_family(),
                request.uri(),
            ),
        )
    }

    pub(crate) fn provider_family(&self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions(_) | Self::OpenAiResponses(_) => {
                BackendProviderKind::OpenAiCompatible.as_str()
            }
            Self::OpenRouter(_) => BackendProviderKind::OpenRouter.as_str(),
            Self::ChatGptCodex(_) => BackendProviderKind::ChatGptCodex.as_str(),
            Self::XaiGrokChatCompletions(_) | Self::XaiGrokResponses(_) => {
                BackendProviderKind::XaiGrokOAuth.as_str()
            }
            Self::ClaudeSubscription(_) => BackendProviderKind::ClaudeCliSubscription.as_str(),
            Self::AnthropicApiKey(_) => BackendProviderKind::AnthropicApiKey.as_str(),
        }
    }
}

pub(crate) fn claude_subscription_replay_issuer(
) -> Result<Option<gents_loop::claude_messages_body::ReplayIssuer>> {
    messages_replay_issuer(BackendProviderKind::ClaudeCliSubscription)
}

fn messages_replay_issuer(
    kind: BackendProviderKind,
) -> Result<Option<gents_loop::claude_messages_body::ReplayIssuer>> {
    let request =
        rig::http_client::Request::post(crate::inference::clients::claude_messages::MESSAGES_URI)
            .body(())?;
    Ok(
        gents_loop::rendered_request::transport::replay_issuer_for_destination(
            kind.as_str(),
            request.uri(),
        ),
    )
}

fn api_key_http(
    cache: &Arc<crate::inference::usage_observation::UsageCache>,
    node: &Arc<EmbeddedNode>,
    agent: &ResolvedAgent,
) -> crate::inference::clients::provider_http::ProviderHttpClient {
    match crate::inference::usage_observation::UsageAccount::for_agent(agent) {
        Some(account) => crate::inference::clients::provider_http::ProviderHttpClient::with_usage(
            Default::default(),
            crate::inference::usage_observation::UsageReporter::new(
                cache.clone(),
                node.clone(),
                account,
            ),
        ),
        None => crate::inference::clients::provider_http::ProviderHttpClient::default(),
    }
}

/// OAuth-branch construction hits DefraDB for the agent's `OAuthCredential`
/// and is bounded by `build_timeout` so a wedged lookup cannot hang startup.
pub(crate) async fn build_backend_client(
    cache: Arc<crate::inference::usage_observation::UsageCache>,
    node: Arc<EmbeddedNode>,
    agent: &ResolvedAgent,
    api_key: &str,
    build_timeout: Duration,
) -> Result<BackendClient> {
    match agent.backend_provider_kind {
        BackendProviderKind::OpenAiCompatible => {
            let build_context = format!(
                "building OpenAI-compatible completion client for agent {} against {}",
                agent.agent_id, agent.backend_endpoint
            );
            if agent.openai_wire_api == crate::OpenAiWireApi::ChatCompletions {
                let client = crate::inference::clients::inference_http::build_openai_chat_completions_client(
                    api_key,
                    &agent.backend_endpoint,
                    crate::inference::clients::inference_http::SessionTaggingHttpClient::new(
                        crate::inference::rendered_request::RenderedRequestCapturingHttpClient::new(
                            api_key_http(&cache, &node, agent),
                        ),
                    ),
                )
                .with_context(|| build_context.clone())?;
                Ok(BackendClient::OpenAiChatCompletions(client))
            } else {
                let client = crate::inference::clients::inference_http::build_openai_responses_client(
                    api_key,
                    &agent.backend_endpoint,
                    crate::inference::clients::inference_http::SessionTaggingHttpClient::new(
                        crate::inference::clients::inference_http::ResponsesNormalizingHttpClient::new(
                            crate::inference::rendered_request::RenderedRequestCapturingHttpClient::new(
                                api_key_http(&cache, &node, agent),
                            ),
                        )
                        .with_service_tier(agent.service_tier),
                    ),
                    Default::default(),
                )
                .with_context(|| build_context.clone())?;
                Ok(BackendClient::OpenAiResponses(client))
            }
        }
        BackendProviderKind::OpenRouter => {
            let build_context = format!(
                "building OpenRouter completion client for agent {} against {}",
                agent.agent_id, agent.backend_endpoint
            );
            let client: rig::providers::openrouter::Client<
                crate::inference::rendered_request::RenderedRequestCapturingHttpClient<
                    crate::inference::clients::provider_http::ProviderHttpClient,
                >,
            > = rig::providers::openrouter::Client::builder()
                .api_key(api_key)
                .base_url(&agent.backend_endpoint)
                .http_client(
                    crate::inference::rendered_request::RenderedRequestCapturingHttpClient::<
                        crate::inference::clients::provider_http::ProviderHttpClient,
                    >::new(api_key_http(&cache, &node, agent)),
                )
                .build()
                .with_context(|| build_context.clone())?;
            Ok(BackendClient::OpenRouter(client))
        }
        BackendProviderKind::ChatGptCodex => {
            let client = tokio::time::timeout(
                build_timeout,
                crate::inference::clients::chatgpt_codex::build_responses_client(
                    cache.clone(),
                    node,
                    agent.node_did(),
                    agent.backend_auth.oauth_account_ref(),
                    &agent.backend_endpoint,
                    agent.service_tier,
                ),
            )
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "timed out after {build_timeout:?} building the ChatGPT Codex completion client"
                )
            })
            .and_then(|result| result)
            .with_context(|| {
                format!(
                    "building ChatGPT Codex completion client for agent {} against {}",
                    agent.agent_id, agent.backend_endpoint
                )
            })?;
            Ok(BackendClient::ChatGptCodex(client))
        }
        BackendProviderKind::XaiGrokOAuth => {
            let build_context = format!(
                "building Grok OAuth completion client for agent {} against {}",
                agent.agent_id, agent.backend_endpoint
            );
            let timeout_error = || {
                anyhow::anyhow!(
                    "timed out after {build_timeout:?} building the Grok OAuth completion client"
                )
            };
            if agent.openai_wire_api == crate::OpenAiWireApi::ChatCompletions {
                let client = tokio::time::timeout(
                    build_timeout,
                    crate::inference::clients::xai_grok::build_chat_completions_client(
                        node,
                        agent.node_did(),
                        agent.backend_auth.oauth_account_ref(),
                        &agent.backend_endpoint,
                    ),
                )
                .await
                .map_err(|_| timeout_error())
                .and_then(|result| result)
                .with_context(|| build_context.clone())?;
                Ok(BackendClient::XaiGrokChatCompletions(client))
            } else {
                let client = tokio::time::timeout(
                    build_timeout,
                    crate::inference::clients::xai_grok::build_responses_client(
                        node,
                        agent.node_did(),
                        agent.backend_auth.oauth_account_ref(),
                        &agent.backend_endpoint,
                    ),
                )
                .await
                .map_err(|_| timeout_error())
                .and_then(|result| result)
                .with_context(|| build_context.clone())?;
                Ok(BackendClient::XaiGrokResponses(client))
            }
        }
        BackendProviderKind::ClaudeCliSubscription => {
            let client = tokio::time::timeout(
                build_timeout,
                crate::inference::clients::claude_subscription::ClaudeSubscriptionClient::build(
                    cache.clone(),
                    node,
                    agent.node_did(),
                    agent.backend_auth.oauth_account_ref(),
                ),
            )
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "timed out after {build_timeout:?} building the Claude subscription completion client"
                )
            })
            .and_then(|result| result)
            .with_context(|| {
                format!(
                    "building Claude subscription completion client for agent {}",
                    agent.agent_id
                )
            })?;
            Ok(BackendClient::ClaudeSubscription(client))
        }
        BackendProviderKind::AnthropicApiKey => {
            let mut client = crate::inference::clients::claude_subscription::ClaudeSubscriptionClient::with_api_key(
                api_key.to_owned(),
            );
            client.usage = crate::inference::usage_observation::UsageAccount::for_agent(agent).map(
                |account| {
                    crate::inference::usage_observation::UsageReporter::new(cache, node, account)
                },
            );
            Ok(BackendClient::AnthropicApiKey(client))
        }
    }
}

macro_rules! with_backend_client {
    ($client:expr, |$c:ident| $body:expr) => {
        match $client {
            $crate::inference::llm::backend_client::BackendClient::OpenAiChatCompletions($c) => {
                $body
            }
            $crate::inference::llm::backend_client::BackendClient::OpenAiResponses($c) => $body,
            $crate::inference::llm::backend_client::BackendClient::OpenRouter($c) => $body,
            $crate::inference::llm::backend_client::BackendClient::ChatGptCodex($c) => $body,
            $crate::inference::llm::backend_client::BackendClient::XaiGrokChatCompletions($c) => {
                $body
            }
            $crate::inference::llm::backend_client::BackendClient::XaiGrokResponses($c) => $body,
            $crate::inference::llm::backend_client::BackendClient::ClaudeSubscription($c) => $body,
            $crate::inference::llm::backend_client::BackendClient::AnthropicApiKey($c) => $body,
        }
    };
}
pub(crate) use with_backend_client;

#[cfg(test)]
mod usage_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::PendingAgent;
    use crate::inference::auth::credential::BearerSource;
    use crate::store::identity::KeyIdentity;

    async fn test_node() -> Arc<EmbeddedNode> {
        Arc::new(crate::test_support::node_builder().build().await.unwrap())
    }

    pub(super) fn test_agent(
        kind: BackendProviderKind,
        wire_api: crate::OpenAiWireApi,
    ) -> ResolvedAgent {
        let identity = KeyIdentity::load_or_create(
            std::env::temp_dir().join(format!("backend-client-table-{}.key", uuid::Uuid::new_v4())),
            None,
        )
        .unwrap();
        let mut agent =
            PendingAgent::new("backend-client-table").build_with_identity_for_test(identity);
        agent.backend_provider_kind = kind;
        agent.openai_wire_api = wire_api;
        agent.backend_endpoint = "http://127.0.0.1:1/v1".to_string();
        agent
    }

    #[tokio::test]
    async fn each_openai_shaped_provider_kind_yields_the_expected_client_variant() {
        let node = test_node().await;

        let chat = test_agent(
            BackendProviderKind::OpenAiCompatible,
            crate::OpenAiWireApi::ChatCompletions,
        );
        let client = build_backend_client(
            std::sync::Arc::default(),
            node.clone(),
            &chat,
            "key",
            Duration::from_secs(1),
        )
        .await
        .expect("chat completions client builds without I/O");
        assert!(matches!(&client, BackendClient::OpenAiChatCompletions(_)));
        assert_eq!(
            client.provider_family(),
            BackendProviderKind::OpenAiCompatible.as_str()
        );
        let chat_issuer = client
            .replay_issuer()
            .expect("built chat URI")
            .expect("route");

        let responses = test_agent(
            BackendProviderKind::OpenAiCompatible,
            crate::OpenAiWireApi::Responses,
        );
        let client = build_backend_client(
            std::sync::Arc::default(),
            node.clone(),
            &responses,
            "key",
            Duration::from_secs(1),
        )
        .await
        .expect("responses client builds without I/O");
        assert!(matches!(&client, BackendClient::OpenAiResponses(_)));
        assert_eq!(
            client.provider_family(),
            BackendProviderKind::OpenAiCompatible.as_str()
        );
        let responses_issuer = client
            .replay_issuer()
            .expect("built Responses URI")
            .expect("route");
        assert_eq!(chat_issuer.family, responses_issuer.family);
        assert_ne!(chat_issuer.endpoint, responses_issuer.endpoint);

        let mut openrouter = test_agent(
            BackendProviderKind::OpenRouter,
            crate::OpenAiWireApi::ChatCompletions,
        );
        openrouter.backend_endpoint = "https://openrouter.ai/api/v1".to_string();
        let client = build_backend_client(
            std::sync::Arc::default(),
            node.clone(),
            &openrouter,
            "key",
            Duration::from_secs(1),
        )
        .await
        .expect("openrouter client builds without I/O");
        assert!(matches!(&client, BackendClient::OpenRouter(_)));
        assert_eq!(
            client.provider_family(),
            BackendProviderKind::OpenRouter.as_str()
        );
    }

    #[tokio::test]
    async fn anthropic_api_key_builds_a_key_messages_client_without_io() {
        let node = test_node().await;
        let agent = test_agent(
            BackendProviderKind::AnthropicApiKey,
            crate::OpenAiWireApi::ChatCompletions,
        );
        let client = build_backend_client(
            std::sync::Arc::default(),
            node,
            &agent,
            "placeholder-key",
            Duration::from_secs(1),
        )
        .await
        .expect("key client builds without I/O");
        assert!(matches!(&client, BackendClient::AnthropicApiKey(_)));
        assert_eq!(client.provider_family(), "AnthropicApiKey");
        let issuer = client.replay_issuer().expect("built URI").expect("route");
        let sign_in = claude_subscription_replay_issuer()
            .expect("sign-in URI")
            .expect("route");
        assert_eq!(issuer.family, "AnthropicApiKey");
        assert_eq!(issuer.endpoint, sign_in.endpoint);
    }

    async fn seed_oauth_credential(
        node: &std::sync::Arc<EmbeddedNode>,
        agent: &ResolvedAgent,
        provider: &str,
    ) {
        seed_oauth_account(node, agent, provider, None).await;
    }

    async fn seed_oauth_account(
        node: &std::sync::Arc<EmbeddedNode>,
        agent: &ResolvedAgent,
        provider: &str,
        account_ref: Option<&str>,
    ) {
        let node_did = agent.node_did();
        let original = crate::inference::auth::credential::oauth_credential_id(node_did, provider);
        let credential = crate::inference::auth::credential::OAuthCredential {
            doc_id: None,
            credential_id: match account_ref {
                Some(account_ref) => format!("{original}:{account_ref}"),
                None => original,
            },
            node_did: node_did.to_string(),
            provider: provider.to_string(),
            access_token: format!("access-{}", account_ref.unwrap_or("original")),
            refresh_token: "refresh-token".to_string(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled: true,
            account_ref: account_ref.map(str::to_string),
            connected_at: None,
            provider_account_key: None,
            label: None,
        };
        crate::inference::auth::credential::upsert_oauth_credential(node, &credential)
            .await
            .expect("test OAuthCredential must persist");
    }

    #[tokio::test]
    async fn backend_account_reference_builds_with_its_own_account() {
        let node = test_node().await;
        crate::store::schema::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .unwrap();
        for (kind, wire, provider) in [
            (
                BackendProviderKind::ChatGptCodex,
                crate::OpenAiWireApi::Responses,
                crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
            ),
            (
                BackendProviderKind::XaiGrokOAuth,
                crate::OpenAiWireApi::ChatCompletions,
                crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER,
            ),
            (
                BackendProviderKind::XaiGrokOAuth,
                crate::OpenAiWireApi::Responses,
                crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER,
            ),
            (
                BackendProviderKind::ClaudeCliSubscription,
                crate::OpenAiWireApi::Responses,
                crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            ),
        ] {
            let mut agent = test_agent(kind, wire);
            seed_oauth_account(&node, &agent, provider, None).await;
            seed_oauth_account(&node, &agent, provider, Some("acct-b")).await;
            agent.backend_auth = crate::documents::BackendAuth::NodeOAuth {
                account_ref: Some("acct-b".into()),
            };
            let client = build_backend_client(
                std::sync::Arc::default(),
                node.clone(),
                &agent,
                "key",
                Duration::from_secs(5),
            )
            .await
            .unwrap_or_else(|error| panic!("{kind:?} {wire:?} with acct-b: {error:#}"));
            let built = match (&client, wire) {
                (BackendClient::ChatGptCodex(_), _) => BackendProviderKind::ChatGptCodex,
                (
                    BackendClient::XaiGrokChatCompletions(_),
                    crate::OpenAiWireApi::ChatCompletions,
                )
                | (BackendClient::XaiGrokResponses(_), crate::OpenAiWireApi::Responses) => {
                    BackendProviderKind::XaiGrokOAuth
                }
                (BackendClient::ClaudeSubscription(_), _) => {
                    BackendProviderKind::ClaudeCliSubscription
                }
                _ => panic!("{kind:?} {wire:?} built {}", client.provider_family()),
            };
            assert_eq!(built, kind);
            let acct_b = format!(
                "{}:acct-b",
                crate::inference::auth::credential::oauth_credential_id(agent.node_did(), provider)
            );
            let bearer = crate::inference::auth::credential::test_support::bound_bearer(&acct_b)
                .unwrap_or_else(|| panic!("{kind:?} {wire:?} did not bind acct-b"));
            assert_eq!(bearer.current_bearer().await.unwrap(), "access-acct-b");
        }
    }

    #[tokio::test]
    async fn two_profiles_on_two_accounts_are_served_by_their_own() {
        let node = test_node().await;
        crate::store::schema::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .unwrap();
        for (kind, wire, provider) in [
            (
                BackendProviderKind::ChatGptCodex,
                crate::OpenAiWireApi::Responses,
                crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
            ),
            (
                BackendProviderKind::XaiGrokOAuth,
                crate::OpenAiWireApi::ChatCompletions,
                crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER,
            ),
            (
                BackendProviderKind::ClaudeCliSubscription,
                crate::OpenAiWireApi::Responses,
                crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            ),
        ] {
            let on_a = test_agent(kind, wire);
            seed_oauth_account(&node, &on_a, provider, None).await;
            seed_oauth_account(&node, &on_a, provider, Some("acct-b")).await;
            let mut on_b = on_a.clone();
            on_b.backend_auth = crate::documents::BackendAuth::NodeOAuth {
                account_ref: Some("acct-b".into()),
            };
            let original =
                crate::inference::auth::credential::oauth_credential_id(on_a.node_did(), provider);
            for (agent, credential_id, token) in [
                (&on_a, original.clone(), "access-original"),
                (&on_b, format!("{original}:acct-b"), "access-acct-b"),
            ] {
                build_backend_client(
                    std::sync::Arc::default(),
                    node.clone(),
                    agent,
                    "key",
                    Duration::from_secs(5),
                )
                .await
                .unwrap_or_else(|error| panic!("{kind:?} {credential_id}: {error:#}"));
                let bearer =
                    crate::inference::auth::credential::test_support::bound_bearer(&credential_id)
                        .unwrap_or_else(|| panic!("{kind:?} did not bind {credential_id}"));
                assert_eq!(bearer.current_bearer().await.unwrap(), token, "{kind:?}");
            }
        }
    }

    #[tokio::test]
    async fn backend_account_reference_never_falls_back_to_the_original_account() {
        let node = test_node().await;
        crate::store::schema::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .unwrap();
        let mut codex = test_agent(
            BackendProviderKind::ChatGptCodex,
            crate::OpenAiWireApi::Responses,
        );
        seed_oauth_credential(
            &node,
            &codex,
            crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
        )
        .await;
        codex.backend_auth = crate::documents::BackendAuth::NodeOAuth {
            account_ref: Some("acct-b".into()),
        };
        let error = build_backend_client(
            std::sync::Arc::default(),
            node,
            &codex,
            "key",
            Duration::from_secs(5),
        )
        .await
        .err()
        .expect("an account reference must not build with the original credential");
        let missing = crate::inference::auth::credential::classify_chatgpt_auth_error(
            codex.node_did(),
            crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
            &crate::inference::auth::credential::OAuthAuthProblem::Missing,
        );
        assert!(format!("{error:#}").contains(&missing), "{error:#}");
    }

    #[tokio::test]
    async fn each_oauth_provider_kind_yields_the_expected_client_variant() {
        let node = test_node().await;
        crate::store::schema::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .unwrap();

        let codex = test_agent(
            BackendProviderKind::ChatGptCodex,
            crate::OpenAiWireApi::Responses,
        );
        seed_oauth_credential(
            &node,
            &codex,
            crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
        )
        .await;
        let client = build_backend_client(
            std::sync::Arc::default(),
            node.clone(),
            &codex,
            "key",
            Duration::from_secs(5),
        )
        .await
        .expect("Codex client builds from the seeded credential without network I/O");
        assert!(matches!(&client, BackendClient::ChatGptCodex(_)));
        assert_eq!(
            client.provider_family(),
            BackendProviderKind::ChatGptCodex.as_str()
        );

        let xai_chat = test_agent(
            BackendProviderKind::XaiGrokOAuth,
            crate::OpenAiWireApi::ChatCompletions,
        );
        seed_oauth_credential(
            &node,
            &xai_chat,
            crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER,
        )
        .await;
        let client = build_backend_client(
            std::sync::Arc::default(),
            node.clone(),
            &xai_chat,
            "key",
            Duration::from_secs(5),
        )
        .await
        .expect("Grok Chat Completions client builds without network I/O");
        assert!(matches!(&client, BackendClient::XaiGrokChatCompletions(_)));
        assert_eq!(
            client.provider_family(),
            BackendProviderKind::XaiGrokOAuth.as_str()
        );

        let xai_responses = test_agent(
            BackendProviderKind::XaiGrokOAuth,
            crate::OpenAiWireApi::Responses,
        );
        seed_oauth_credential(
            &node,
            &xai_responses,
            crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER,
        )
        .await;
        let client = build_backend_client(
            std::sync::Arc::default(),
            node.clone(),
            &xai_responses,
            "key",
            Duration::from_secs(5),
        )
        .await
        .expect("Grok Responses client builds without network I/O");
        assert!(matches!(&client, BackendClient::XaiGrokResponses(_)));
        assert_eq!(
            client.provider_family(),
            BackendProviderKind::XaiGrokOAuth.as_str()
        );

        let claude = test_agent(
            BackendProviderKind::ClaudeCliSubscription,
            crate::OpenAiWireApi::ChatCompletions,
        );
        seed_oauth_credential(
            &node,
            &claude,
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
        )
        .await;
        let client = build_backend_client(
            std::sync::Arc::default(),
            node.clone(),
            &claude,
            "key",
            Duration::from_secs(5),
        )
        .await
        .expect("Claude subscription client builds without network I/O");
        assert!(matches!(&client, BackendClient::ClaudeSubscription(_)));
        assert_eq!(
            client.provider_family(),
            BackendProviderKind::ClaudeCliSubscription.as_str()
        );
        assert_eq!(
            client
                .replay_issuer()
                .expect("built Claude URI")
                .unwrap()
                .family,
            BackendProviderKind::ClaudeCliSubscription.as_str()
        );
    }
}
