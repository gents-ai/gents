use std::sync::Arc;
use std::time::Duration;

use defra_node::EmbeddedNode;

use crate::backend_provider::BackendProviderKind;
use crate::config::ResolvedAgent;

#[derive(Debug, thiserror::Error)]
pub(crate) enum BackendClientError {
    #[error(transparent)]
    Http(#[from] rig::http_client::Error),
    #[error("building OpenAI-compatible completion client for agent {agent_id} against {endpoint}")]
    OpenAiCompatible {
        agent_id: String,
        endpoint: String,
        #[source]
        source: crate::inference_http::InferenceHttpError,
    },
    #[error("building OpenRouter completion client for agent {agent_id} against {endpoint}")]
    OpenRouter {
        agent_id: String,
        endpoint: String,
        #[source]
        source: rig::http_client::Error,
    },
    #[error("building ChatGPT Codex completion client for agent {agent_id} against {endpoint}")]
    ChatGptCodex {
        agent_id: String,
        endpoint: String,
        #[source]
        source: ClientBuildFailure<crate::chatgpt_codex::ChatGptCodexClientError>,
    },
    #[error("building Grok OAuth completion client for agent {agent_id} against {endpoint}")]
    XaiGrok {
        agent_id: String,
        endpoint: String,
        #[source]
        source: ClientBuildFailure<crate::xai_grok_oauth::XaiGrokOAuthError>,
    },
    #[error("building Claude subscription completion client for agent {agent_id}")]
    ClaudeSubscription {
        agent_id: String,
        #[source]
        source: ClientBuildFailure<crate::claude_subscription::ClaudeSubscriptionError>,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ClientBuildFailure<E>
where
    E: std::error::Error + 'static,
{
    #[error("timed out after {timeout:?} building the {client} completion client")]
    TimedOut {
        timeout: Duration,
        client: &'static str,
    },
    #[error(transparent)]
    Build(E),
}

type Result<T, E = BackendClientError> = std::result::Result<T, E>;

fn bounded<T, E>(
    outcome: std::result::Result<std::result::Result<T, E>, tokio::time::error::Elapsed>,
    timeout: Duration,
    client: &'static str,
) -> std::result::Result<T, ClientBuildFailure<E>>
where
    E: std::error::Error + 'static,
{
    outcome
        .map_err(|_| ClientBuildFailure::TimedOut { timeout, client })?
        .map_err(ClientBuildFailure::Build)
}

pub(crate) enum BackendClient {
    OpenAiChatCompletions(
        rig::providers::openai::CompletionsClient<
            crate::inference_http::SessionTaggingHttpClient<
                crate::rendered_request::RenderedRequestCapturingHttpClient<
                    crate::provider_http::ProviderHttpClient,
                >,
            >,
        >,
    ),
    OpenAiResponses(
        rig::providers::openai::Client<
            crate::inference_http::SessionTaggingHttpClient<
                crate::inference_http::ResponsesNormalizingHttpClient<
                    crate::rendered_request::RenderedRequestCapturingHttpClient<
                        crate::provider_http::ProviderHttpClient,
                    >,
                >,
            >,
        >,
    ),
    OpenRouter(
        rig::providers::openrouter::Client<
            crate::rendered_request::RenderedRequestCapturingHttpClient<
                crate::provider_http::ProviderHttpClient,
            >,
        >,
    ),
    ChatGptCodex(
        rig::providers::openai::Client<
            crate::chatgpt_codex::ChatGptCodexHttpClient<
                crate::oauth_credential::DbCredentialBearer,
                crate::rendered_request::RenderedRequestCapturingHttpClient<
                    crate::provider_http::ProviderHttpClient,
                >,
            >,
        >,
    ),
    XaiGrokChatCompletions(
        rig::providers::openai::CompletionsClient<
            crate::xai_grok_oauth::CapturingXaiGrokOAuthHttpClient,
        >,
    ),
    XaiGrokResponses(
        rig::providers::openai::Client<crate::xai_grok_oauth::CapturingXaiGrokOAuthHttpClient>,
    ),
    ClaudeSubscription(
        crate::claude_subscription::ClaudeSubscriptionClient<
            crate::oauth_credential::DbCredentialBearer,
        >,
    ),
    AnthropicApiKey(
        crate::claude_subscription::ClaudeSubscriptionClient<
            crate::claude_subscription::ApiKeyBearer,
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
        .body(())
        .map_err(rig::http_client::Error::Protocol)?;
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
    let request = rig::http_client::Request::post(crate::claude_messages::MESSAGES_URI)
        .body(())
        .map_err(rig::http_client::Error::Protocol)?;
    Ok(
        gents_loop::rendered_request::transport::replay_issuer_for_destination(
            kind.as_str(),
            request.uri(),
        ),
    )
}

fn api_key_http(
    node: &Arc<EmbeddedNode>,
    agent: &ResolvedAgent,
) -> crate::provider_http::ProviderHttpClient {
    match crate::usage_observation::UsageAccount::for_agent(agent) {
        Some(account) => crate::provider_http::ProviderHttpClient::with_usage(
            Default::default(),
            crate::usage_observation::UsageReporter::new(node.clone(), account),
        ),
        None => crate::provider_http::ProviderHttpClient::default(),
    }
}

/// OAuth-branch construction hits DefraDB for the agent's `OAuthCredential`
/// and is bounded by `build_timeout` so a wedged lookup cannot hang startup.
pub(crate) async fn build_backend_client(
    node: Arc<EmbeddedNode>,
    agent: &ResolvedAgent,
    api_key: &str,
    build_timeout: Duration,
) -> Result<BackendClient> {
    match agent.backend_provider_kind {
        BackendProviderKind::OpenAiCompatible => {
            let build_context = |source| BackendClientError::OpenAiCompatible {
                agent_id: agent.agent_id.to_string(),
                endpoint: agent.backend_endpoint.clone(),
                source,
            };
            if agent.openai_wire_api == crate::OpenAiWireApi::ChatCompletions {
                let client = crate::inference_http::build_openai_chat_completions_client(
                    api_key,
                    &agent.backend_endpoint,
                    crate::inference_http::SessionTaggingHttpClient::new(
                        crate::rendered_request::RenderedRequestCapturingHttpClient::new(
                            api_key_http(&node, agent),
                        ),
                    ),
                )
                .map_err(build_context)?;
                Ok(BackendClient::OpenAiChatCompletions(client))
            } else {
                let client = crate::inference_http::build_openai_responses_client(
                    api_key,
                    &agent.backend_endpoint,
                    crate::inference_http::SessionTaggingHttpClient::new(
                        crate::inference_http::ResponsesNormalizingHttpClient::new(
                            crate::rendered_request::RenderedRequestCapturingHttpClient::new(
                                api_key_http(&node, agent),
                            ),
                        )
                        .with_service_tier(agent.service_tier),
                    ),
                    Default::default(),
                )
                .map_err(build_context)?;
                Ok(BackendClient::OpenAiResponses(client))
            }
        }
        BackendProviderKind::OpenRouter => {
            let client: rig::providers::openrouter::Client<
                crate::rendered_request::RenderedRequestCapturingHttpClient<
                    crate::provider_http::ProviderHttpClient,
                >,
            > = rig::providers::openrouter::Client::builder()
                .api_key(api_key)
                .base_url(&agent.backend_endpoint)
                .http_client(
                    crate::rendered_request::RenderedRequestCapturingHttpClient::<
                        crate::provider_http::ProviderHttpClient,
                    >::default(),
                )
                .build()
                .map_err(|source| BackendClientError::OpenRouter {
                    agent_id: agent.agent_id.to_string(),
                    endpoint: agent.backend_endpoint.clone(),
                    source,
                })?;
            Ok(BackendClient::OpenRouter(client))
        }
        BackendProviderKind::ChatGptCodex => {
            let client = bounded(
                tokio::time::timeout(
                    build_timeout,
                    crate::chatgpt_codex::build_responses_client(
                        node,
                        agent.node_did(),
                        agent.backend_auth.oauth_account_ref(),
                        &agent.backend_endpoint,
                        agent.service_tier,
                    ),
                )
                .await,
                build_timeout,
                "ChatGPT Codex",
            )
            .map_err(|source| BackendClientError::ChatGptCodex {
                agent_id: agent.agent_id.to_string(),
                endpoint: agent.backend_endpoint.clone(),
                source,
            })?;
            Ok(BackendClient::ChatGptCodex(client))
        }
        BackendProviderKind::XaiGrokOAuth => {
            let build_context = |source| BackendClientError::XaiGrok {
                agent_id: agent.agent_id.to_string(),
                endpoint: agent.backend_endpoint.clone(),
                source,
            };
            if agent.openai_wire_api == crate::OpenAiWireApi::ChatCompletions {
                let client = bounded(
                    tokio::time::timeout(
                        build_timeout,
                        crate::xai_grok_oauth::build_chat_completions_client(
                            node,
                            agent.node_did(),
                            agent.backend_auth.oauth_account_ref(),
                            &agent.backend_endpoint,
                        ),
                    )
                    .await,
                    build_timeout,
                    "Grok OAuth",
                )
                .map_err(build_context)?;
                Ok(BackendClient::XaiGrokChatCompletions(client))
            } else {
                let client = bounded(
                    tokio::time::timeout(
                        build_timeout,
                        crate::xai_grok_oauth::build_responses_client(
                            node,
                            agent.node_did(),
                            agent.backend_auth.oauth_account_ref(),
                            &agent.backend_endpoint,
                        ),
                    )
                    .await,
                    build_timeout,
                    "Grok OAuth",
                )
                .map_err(build_context)?;
                Ok(BackendClient::XaiGrokResponses(client))
            }
        }
        BackendProviderKind::ClaudeCliSubscription => {
            let client = bounded(
                tokio::time::timeout(
                    build_timeout,
                    crate::claude_subscription::ClaudeSubscriptionClient::build(
                        node,
                        agent.node_did(),
                        agent.backend_auth.oauth_account_ref(),
                    ),
                )
                .await,
                build_timeout,
                "Claude subscription",
            )
            .map_err(|source| BackendClientError::ClaudeSubscription {
                agent_id: agent.agent_id.to_string(),
                source,
            })?;
            Ok(BackendClient::ClaudeSubscription(client))
        }
        BackendProviderKind::AnthropicApiKey => Ok(BackendClient::AnthropicApiKey(
            crate::claude_subscription::ClaudeSubscriptionClient::with_api_key(api_key.to_owned()),
        )),
    }
}

macro_rules! with_backend_client {
    ($client:expr, |$c:ident| $body:expr) => {
        match $client {
            $crate::llm::backend_client::BackendClient::OpenAiChatCompletions($c) => $body,
            $crate::llm::backend_client::BackendClient::OpenAiResponses($c) => $body,
            $crate::llm::backend_client::BackendClient::OpenRouter($c) => $body,
            $crate::llm::backend_client::BackendClient::ChatGptCodex($c) => $body,
            $crate::llm::backend_client::BackendClient::XaiGrokChatCompletions($c) => $body,
            $crate::llm::backend_client::BackendClient::XaiGrokResponses($c) => $body,
            $crate::llm::backend_client::BackendClient::ClaudeSubscription($c) => $body,
            $crate::llm::backend_client::BackendClient::AnthropicApiKey($c) => $body,
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
    use crate::identity::KeyIdentity;
    use crate::oauth_credential::BearerSource;
    use gents_protocol::error_chain::ErrorChain;

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
        let client = build_backend_client(node.clone(), &chat, "key", Duration::from_secs(1))
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
        let client = build_backend_client(node.clone(), &responses, "key", Duration::from_secs(1))
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
        let client = build_backend_client(node.clone(), &openrouter, "key", Duration::from_secs(1))
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
        let client = build_backend_client(node, &agent, "placeholder-key", Duration::from_secs(1))
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
        let original = crate::oauth_credential::oauth_credential_id(node_did, provider);
        let credential = crate::oauth_credential::OAuthCredential {
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
        crate::oauth_credential::upsert_oauth_credential(node, &credential)
            .await
            .expect("test OAuthCredential must persist");
    }

    #[tokio::test]
    async fn backend_account_reference_builds_with_its_own_account() {
        let node = test_node().await;
        crate::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .unwrap();
        for (kind, wire, provider) in [
            (
                BackendProviderKind::ChatGptCodex,
                crate::OpenAiWireApi::Responses,
                crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
            ),
            (
                BackendProviderKind::XaiGrokOAuth,
                crate::OpenAiWireApi::ChatCompletions,
                crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
            ),
            (
                BackendProviderKind::XaiGrokOAuth,
                crate::OpenAiWireApi::Responses,
                crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
            ),
            (
                BackendProviderKind::ClaudeCliSubscription,
                crate::OpenAiWireApi::Responses,
                crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
            ),
        ] {
            let mut agent = test_agent(kind, wire);
            seed_oauth_account(&node, &agent, provider, None).await;
            seed_oauth_account(&node, &agent, provider, Some("acct-b")).await;
            agent.backend_auth = crate::document_config::BackendAuth::NodeOAuth {
                account_ref: Some("acct-b".into()),
            };
            let client = build_backend_client(node.clone(), &agent, "key", Duration::from_secs(5))
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "{kind:?} {wire:?} with acct-b: {}",
                        ErrorChain(&error)
                    )
                });
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
                crate::oauth_credential::oauth_credential_id(agent.node_did(), provider)
            );
            let bearer = crate::oauth_credential::test_support::bound_bearer(&acct_b)
                .unwrap_or_else(|| panic!("{kind:?} {wire:?} did not bind acct-b"));
            assert_eq!(bearer.current_bearer().await.unwrap(), "access-acct-b");
        }
    }

    #[tokio::test]
    async fn two_profiles_on_two_accounts_are_served_by_their_own() {
        let node = test_node().await;
        crate::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .unwrap();
        for (kind, wire, provider) in [
            (
                BackendProviderKind::ChatGptCodex,
                crate::OpenAiWireApi::Responses,
                crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
            ),
            (
                BackendProviderKind::XaiGrokOAuth,
                crate::OpenAiWireApi::ChatCompletions,
                crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
            ),
            (
                BackendProviderKind::ClaudeCliSubscription,
                crate::OpenAiWireApi::Responses,
                crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
            ),
        ] {
            let on_a = test_agent(kind, wire);
            seed_oauth_account(&node, &on_a, provider, None).await;
            seed_oauth_account(&node, &on_a, provider, Some("acct-b")).await;
            let mut on_b = on_a.clone();
            on_b.backend_auth = crate::document_config::BackendAuth::NodeOAuth {
                account_ref: Some("acct-b".into()),
            };
            let original = crate::oauth_credential::oauth_credential_id(on_a.node_did(), provider);
            for (agent, credential_id, token) in [
                (&on_a, original.clone(), "access-original"),
                (&on_b, format!("{original}:acct-b"), "access-acct-b"),
            ] {
                build_backend_client(node.clone(), agent, "key", Duration::from_secs(5))
                    .await
                    .unwrap_or_else(|error| {
                        panic!("{kind:?} {credential_id}: {}", ErrorChain(&error))
                    });
                let bearer = crate::oauth_credential::test_support::bound_bearer(&credential_id)
                    .unwrap_or_else(|| panic!("{kind:?} did not bind {credential_id}"));
                assert_eq!(bearer.current_bearer().await.unwrap(), token, "{kind:?}");
            }
        }
    }

    #[tokio::test]
    async fn backend_account_reference_never_falls_back_to_the_original_account() {
        let node = test_node().await;
        crate::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .unwrap();
        let mut codex = test_agent(
            BackendProviderKind::ChatGptCodex,
            crate::OpenAiWireApi::Responses,
        );
        seed_oauth_credential(&node, &codex, crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER).await;
        codex.backend_auth = crate::document_config::BackendAuth::NodeOAuth {
            account_ref: Some("acct-b".into()),
        };
        let error = build_backend_client(node, &codex, "key", Duration::from_secs(5))
            .await
            .err()
            .expect("an account reference must not build with the original credential");
        let missing = crate::oauth_credential::classify_chatgpt_auth_error(
            codex.node_did(),
            crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
            &crate::oauth_credential::OAuthAuthProblem::Missing,
        );
        assert!(
            format!("{}", ErrorChain(&error)).contains(&missing),
            "{}",
            ErrorChain(&error)
        );
    }

    #[tokio::test]
    async fn each_oauth_provider_kind_yields_the_expected_client_variant() {
        let node = test_node().await;
        crate::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .unwrap();

        let codex = test_agent(
            BackendProviderKind::ChatGptCodex,
            crate::OpenAiWireApi::Responses,
        );
        seed_oauth_credential(&node, &codex, crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER).await;
        let client = build_backend_client(node.clone(), &codex, "key", Duration::from_secs(5))
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
        seed_oauth_credential(&node, &xai_chat, crate::xai_grok_oauth::XAI_OAUTH_PROVIDER).await;
        let client = build_backend_client(node.clone(), &xai_chat, "key", Duration::from_secs(5))
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
            crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
        )
        .await;
        let client =
            build_backend_client(node.clone(), &xai_responses, "key", Duration::from_secs(5))
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
        seed_oauth_credential(&node, &claude, crate::claude_oauth::CLAUDE_OAUTH_PROVIDER).await;
        let client = build_backend_client(node.clone(), &claude, "key", Duration::from_secs(5))
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
