//! Single owner of "build the provider completion client for a behavior's
//! `BackendProviderKind`" — the daemon (`agent/runtime/context.rs`) and
//! one-shot (`oneshot.rs`) used to carry independent copies of this
//! four-way match, and only the daemon wrapped OAuth-branch client
//! construction in a build timeout. Both callers now go through
//! [`build_backend_client`], which applies that timeout to every OAuth build
//! regardless of caller.
//!
//! Each `BackendProviderKind` × wire-API combination produces a distinct
//! concrete `rig` client type, so the result is a small closed enum
//! ([`BackendClient`]) rather than a `dyn` client: callers match on it once to
//! hand the concrete value to their own generic completion-loop entry point
//! (`run_behavior_with_client` / `run_oneshot_with_completion_client`), which
//! is the only place left that needs to be generic over the client type.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

use crate::backend_provider::BackendProviderKind;
use crate::config::ResolvedBehavior;

/// A built provider completion client, tagged by which
/// `BackendProviderKind` × wire-API combination produced it.
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
}

impl BackendClient {
    /// Read the built client's URI builder, not the mutable backend document.
    /// These are the completion paths used by the pinned Rig clients; body
    /// capture independently records the destination used at send time.
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
        }
        .body(())?;
        Ok(
            gents_loop::rendered_request::transport::replay_issuer_for_destination(
                self.provider_family(),
                request.uri(),
            ),
        )
    }

    /// Family recorded from the branch that actually constructed this client.
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
        }
    }
}

/// The Claude subscription posts to its fixed Messages URI.
pub(crate) fn claude_subscription_replay_issuer(
) -> Result<Option<gents_loop::claude_messages_body::ReplayIssuer>> {
    let request = rig::http_client::Request::post(crate::claude_messages::MESSAGES_URI).body(())?;
    Ok(
        gents_loop::rendered_request::transport::replay_issuer_for_destination(
            BackendProviderKind::ClaudeCliSubscription.as_str(),
            request.uri(),
        ),
    )
}

/// The network terminal of an OpenAI-compatible backend, recording usage
/// headers for the backend when it has an id.
fn api_key_http(
    node: &Arc<EmbeddedNode>,
    behavior: &ResolvedBehavior,
) -> crate::provider_http::ProviderHttpClient {
    match crate::usage_observation::UsageAccount::for_behavior(behavior) {
        Some(account) => crate::provider_http::ProviderHttpClient::with_usage(
            Default::default(),
            crate::usage_observation::UsageReporter::new(node.clone(), account),
        ),
        None => crate::provider_http::ProviderHttpClient::default(),
    }
}

/// Build the provider completion client for `behavior`'s
/// `backend_provider_kind` (and, where the provider has one, its configured
/// `openai_wire_api`).
///
/// OAuth-branch construction (ChatGPT Codex, Grok/xAI OAuth) hits DefraDB for
/// the agent's `OAuthCredential` and is bounded by `build_timeout` so a wedged
/// lookup cannot hang startup forever — the same protection the daemon has
/// always applied, now shared by one-shot too (#1338).
pub(crate) async fn build_backend_client(
    node: Arc<EmbeddedNode>,
    behavior: &ResolvedBehavior,
    api_key: &str,
    build_timeout: Duration,
) -> Result<BackendClient> {
    match behavior.backend_provider_kind {
        BackendProviderKind::OpenAiCompatible => {
            let build_context = format!(
                "building OpenAI-compatible completion client for behavior {} against {}",
                behavior.behavior_id, behavior.backend_endpoint
            );
            if behavior.openai_wire_api == crate::OpenAiWireApi::ChatCompletions {
                let client = crate::inference_http::build_openai_chat_completions_client(
                    api_key,
                    &behavior.backend_endpoint,
                    crate::inference_http::SessionTaggingHttpClient::new(
                        crate::rendered_request::RenderedRequestCapturingHttpClient::new(
                            api_key_http(&node, behavior),
                        ),
                    ),
                )
                .with_context(|| build_context.clone())?;
                Ok(BackendClient::OpenAiChatCompletions(client))
            } else {
                let client = crate::inference_http::build_openai_responses_client(
                    api_key,
                    &behavior.backend_endpoint,
                    crate::inference_http::SessionTaggingHttpClient::new(
                        crate::inference_http::ResponsesNormalizingHttpClient::new(
                            crate::rendered_request::RenderedRequestCapturingHttpClient::new(
                                api_key_http(&node, behavior),
                            ),
                        ),
                    ),
                    Default::default(),
                )
                .with_context(|| build_context.clone())?;
                Ok(BackendClient::OpenAiResponses(client))
            }
        }
        BackendProviderKind::OpenRouter => {
            let build_context = format!(
                "building OpenRouter completion client for behavior {} against {}",
                behavior.behavior_id, behavior.backend_endpoint
            );
            let client: rig::providers::openrouter::Client<
                crate::rendered_request::RenderedRequestCapturingHttpClient<
                    crate::provider_http::ProviderHttpClient,
                >,
            > = rig::providers::openrouter::Client::builder()
                .api_key(api_key)
                .base_url(&behavior.backend_endpoint)
                .http_client(
                    crate::rendered_request::RenderedRequestCapturingHttpClient::<
                        crate::provider_http::ProviderHttpClient,
                    >::default(),
                )
                .build()
                .with_context(|| build_context.clone())?;
            Ok(BackendClient::OpenRouter(client))
        }
        BackendProviderKind::ChatGptCodex => {
            let client = tokio::time::timeout(
                build_timeout,
                crate::chatgpt_codex::build_responses_client(
                    node,
                    behavior.agent_did(),
                    behavior.backend_auth.oauth_account_ref(),
                    &behavior.backend_endpoint,
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
                    "building ChatGPT Codex completion client for behavior {} against {}",
                    behavior.behavior_id, behavior.backend_endpoint
                )
            })?;
            Ok(BackendClient::ChatGptCodex(client))
        }
        BackendProviderKind::XaiGrokOAuth => {
            let build_context = format!(
                "building Grok OAuth completion client for behavior {} against {}",
                behavior.behavior_id, behavior.backend_endpoint
            );
            let timeout_error = || {
                anyhow::anyhow!(
                    "timed out after {build_timeout:?} building the Grok OAuth completion client"
                )
            };
            if behavior.openai_wire_api == crate::OpenAiWireApi::ChatCompletions {
                let client = tokio::time::timeout(
                    build_timeout,
                    crate::xai_grok_oauth::build_chat_completions_client(
                        node,
                        behavior.agent_did(),
                        behavior.backend_auth.oauth_account_ref(),
                        &behavior.backend_endpoint,
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
                    crate::xai_grok_oauth::build_responses_client(
                        node,
                        behavior.agent_did(),
                        behavior.backend_auth.oauth_account_ref(),
                        &behavior.backend_endpoint,
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
                crate::claude_subscription::ClaudeSubscriptionClient::build(
                    node,
                    behavior.agent_did(),
                    behavior.backend_auth.oauth_account_ref(),
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
                    "building Claude subscription completion client for behavior {}",
                    behavior.behavior_id
                )
            })?;
            Ok(BackendClient::ClaudeSubscription(client))
        }
    }
}

/// Dispatch a built `BackendClient` to `$body`, binding the unwrapped
/// concrete client to `$c`. Single owner of the six-arm match every caller
/// otherwise has to repeat: each `BackendProviderKind` × wire-API
/// combination produces a distinct concrete `rig` client type, and `$body`
/// is typically a call into a `CompletionClient`-generic continuation
/// (`run_behavior_with_client` / `run_oneshot_with_completion_client`) that
/// Rust monomorphizes once per concrete type it's invoked with — so the
/// match itself can't become a plain function, only written once.
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
        }
    };
}
pub(crate) use with_backend_client;

#[cfg(test)]
mod usage_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::PendingAgentBehavior;
    use crate::identity::KeyIdentity;
    use crate::oauth_credential::BearerSource;

    async fn test_node() -> Arc<EmbeddedNode> {
        Arc::new(EmbeddedNode::builder().build().await.unwrap())
    }

    pub(super) fn test_behavior(
        kind: BackendProviderKind,
        wire_api: crate::OpenAiWireApi,
    ) -> ResolvedBehavior {
        let identity = KeyIdentity::load_or_create(
            std::env::temp_dir().join(format!("backend-client-table-{}.key", uuid::Uuid::new_v4())),
            None,
        )
        .unwrap();
        let mut behavior = PendingAgentBehavior::new("backend-client-table")
            .build_with_identity_for_test(identity);
        behavior.backend_provider_kind = kind;
        behavior.openai_wire_api = wire_api;
        behavior.backend_endpoint = "http://127.0.0.1:1/v1".to_string();
        behavior
    }

    /// OpenAI-compatible and OpenRouter clients are built synchronously from
    /// strings (api key/endpoint) with no I/O, so the table test can assert
    /// on the exact concrete variant the shared constructor returns for every
    /// `BackendProviderKind` that doesn't need a live OAuthCredential.
    #[tokio::test]
    async fn each_openai_shaped_provider_kind_yields_the_expected_client_variant() {
        let node = test_node().await;

        let chat = test_behavior(
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

        let responses = test_behavior(
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

        let mut openrouter = test_behavior(
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

    async fn seed_oauth_credential(
        node: &EmbeddedNode,
        behavior: &ResolvedBehavior,
        provider: &str,
    ) {
        seed_oauth_account(node, behavior, provider, None).await;
    }

    async fn seed_oauth_account(
        node: &EmbeddedNode,
        behavior: &ResolvedBehavior,
        provider: &str,
        account_ref: Option<&str>,
    ) {
        let agent_did = behavior.agent_did();
        let original = crate::oauth_credential::oauth_credential_id(agent_did, provider);
        let credential = crate::oauth_credential::OAuthCredential {
            doc_id: None,
            credential_id: match account_ref {
                Some(account_ref) => format!("{original}:{account_ref}"),
                None => original,
            },
            agent_did: agent_did.to_string(),
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
            let mut behavior = test_behavior(kind, wire);
            seed_oauth_account(node.as_ref(), &behavior, provider, None).await;
            seed_oauth_account(node.as_ref(), &behavior, provider, Some("acct-b")).await;
            behavior.backend_auth = crate::document_config::BackendAuth::PrincipalOAuth {
                account_ref: Some("acct-b".into()),
            };
            let client =
                build_backend_client(node.clone(), &behavior, "key", Duration::from_secs(5))
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
                crate::oauth_credential::oauth_credential_id(behavior.agent_did(), provider)
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
            let on_a = test_behavior(kind, wire);
            seed_oauth_account(node.as_ref(), &on_a, provider, None).await;
            seed_oauth_account(node.as_ref(), &on_a, provider, Some("acct-b")).await;
            let mut on_b = on_a.clone();
            on_b.backend_auth = crate::document_config::BackendAuth::PrincipalOAuth {
                account_ref: Some("acct-b".into()),
            };
            let original = crate::oauth_credential::oauth_credential_id(on_a.agent_did(), provider);
            for (behavior, credential_id, token) in [
                (&on_a, original.clone(), "access-original"),
                (&on_b, format!("{original}:acct-b"), "access-acct-b"),
            ] {
                build_backend_client(node.clone(), behavior, "key", Duration::from_secs(5))
                    .await
                    .unwrap_or_else(|error| panic!("{kind:?} {credential_id}: {error:#}"));
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
        let mut codex = test_behavior(
            BackendProviderKind::ChatGptCodex,
            crate::OpenAiWireApi::Responses,
        );
        seed_oauth_credential(
            node.as_ref(),
            &codex,
            crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
        )
        .await;
        codex.backend_auth = crate::document_config::BackendAuth::PrincipalOAuth {
            account_ref: Some("acct-b".into()),
        };
        let error = build_backend_client(node, &codex, "key", Duration::from_secs(5))
            .await
            .err()
            .expect("an account reference must not build with the original credential");
        let missing = crate::oauth_credential::classify_chatgpt_auth_error(
            codex.agent_did(),
            crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
            &crate::oauth_credential::OAuthAuthProblem::Missing,
        );
        assert!(format!("{error:#}").contains(&missing), "{error:#}");
    }

    /// Seed credentials so every OAuth route reaches a concrete client. A
    /// missing-credential assertion cannot distinguish xAI Chat Completions
    /// from Responses because both fail in their shared bootstrap preamble.
    #[tokio::test]
    async fn each_oauth_provider_kind_yields_the_expected_client_variant() {
        let node = test_node().await;
        crate::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .unwrap();

        let codex = test_behavior(
            BackendProviderKind::ChatGptCodex,
            crate::OpenAiWireApi::Responses,
        );
        seed_oauth_credential(
            node.as_ref(),
            &codex,
            crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
        )
        .await;
        let client = build_backend_client(node.clone(), &codex, "key", Duration::from_secs(5))
            .await
            .expect("Codex client builds from the seeded credential without network I/O");
        assert!(matches!(&client, BackendClient::ChatGptCodex(_)));
        assert_eq!(
            client.provider_family(),
            BackendProviderKind::ChatGptCodex.as_str()
        );

        let xai_chat = test_behavior(
            BackendProviderKind::XaiGrokOAuth,
            crate::OpenAiWireApi::ChatCompletions,
        );
        seed_oauth_credential(
            node.as_ref(),
            &xai_chat,
            crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
        )
        .await;
        let client = build_backend_client(node.clone(), &xai_chat, "key", Duration::from_secs(5))
            .await
            .expect("Grok Chat Completions client builds without network I/O");
        assert!(matches!(&client, BackendClient::XaiGrokChatCompletions(_)));
        assert_eq!(
            client.provider_family(),
            BackendProviderKind::XaiGrokOAuth.as_str()
        );

        let xai_responses = test_behavior(
            BackendProviderKind::XaiGrokOAuth,
            crate::OpenAiWireApi::Responses,
        );
        seed_oauth_credential(
            node.as_ref(),
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

        let claude = test_behavior(
            BackendProviderKind::ClaudeCliSubscription,
            crate::OpenAiWireApi::ChatCompletions,
        );
        seed_oauth_credential(
            node.as_ref(),
            &claude,
            crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
        )
        .await;
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
