use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use rig::agent::Agent;
use rig::client::CompletionClient;
use rig::completion::CompletionModel;
use rig::completion::Prompt;
use rig::tool::ToolDyn;

use crate::backend_provider::BackendProviderKind;
use crate::completion_factory::build_agent;
use crate::config::AgentBehavior;
use crate::hook::{BackgroundToolRegistry, DefraSessionHook, FailurePolicy};
use crate::prompt::{LayeredPromptBuilder, PromptBuilder};
use crate::schema::ensure_schemas;
use crate::tool_surface::ToolRuntimeContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneshotRunResult {
    pub session_id: String,
    pub response_text: String,
}

pub async fn run_openai_oneshot(
    node: Arc<EmbeddedNode>,
    behavior: &AgentBehavior,
    prompt: &str,
) -> Result<OneshotRunResult> {
    run_openai_oneshot_with_tools(node, behavior, Vec::new(), prompt).await
}

pub async fn run_openai_oneshot_with_tools(
    node: Arc<EmbeddedNode>,
    behavior: &AgentBehavior,
    extra_tools: Vec<Box<dyn ToolDyn>>,
    prompt: &str,
) -> Result<OneshotRunResult> {
    ensure_schemas(node.as_ref()).await?;
    crate::migration::ensure_tool_call_migrations(node.clone()).await?;

    let api_key = behavior.completion_client_api_key()?;
    let tool_runtime = ToolRuntimeContext::oneshot(node.clone());
    let tool_surface = behavior.tools.resolve(node.as_ref()).await?;
    let prompt_builder = LayeredPromptBuilder::new(behavior, &tool_surface);
    let preamble = prompt_builder.preamble().to_string();

    let mut tools = tool_surface.build_tools(&tool_runtime)?;
    tools.extend(extra_tools);
    let background_tool_registry = BackgroundToolRegistry::from_tools(
        tool_surface
            .build_tools(&tool_runtime)?
            .into_iter()
            .map(crate::tool_call_lifecycle::runtime::wrap_tool)
            .collect(),
        &tool_surface.background_tools().allowlist,
    );

    match behavior.backend_provider_kind {
        BackendProviderKind::OpenAiCompatible => {
            let build_context = format!(
                "building OpenAI-compatible completion client for behavior {} against {}",
                behavior.behavior_id, behavior.backend_endpoint
            );
            let client: rig::providers::openai::CompletionsClient =
                rig::providers::openai::CompletionsClient::builder()
                    .api_key(&api_key)
                    .base_url(&behavior.backend_endpoint)
                    .build()
                    .with_context(|| build_context.clone())?;
            run_oneshot_with_completion_client(
                node,
                behavior,
                prompt,
                prompt_builder,
                preamble,
                tools,
                background_tool_registry,
                client,
            )
            .await
        }
        BackendProviderKind::OpenRouter => {
            let build_context = format!(
                "building OpenRouter completion client for behavior {} against {}",
                behavior.behavior_id, behavior.backend_endpoint
            );
            let client: rig::providers::openrouter::Client =
                rig::providers::openrouter::Client::builder()
                    .api_key(&api_key)
                    .base_url(&behavior.backend_endpoint)
                    .build()
                    .with_context(|| build_context.clone())?;
            run_oneshot_with_completion_client(
                node,
                behavior,
                prompt,
                prompt_builder,
                preamble,
                tools,
                background_tool_registry,
                client,
            )
            .await
        }
    }
}

async fn run_oneshot_with_completion_client<C>(
    node: Arc<EmbeddedNode>,
    behavior: &AgentBehavior,
    prompt: &str,
    prompt_builder: LayeredPromptBuilder,
    preamble: String,
    tools: Vec<Box<dyn ToolDyn>>,
    background_tool_registry: BackgroundToolRegistry,
    client: C,
) -> Result<OneshotRunResult>
where
    C: CompletionClient,
    C::CompletionModel: 'static,
{
    let agent = build_agent(&client, behavior, &preamble, tools);
    run_oneshot_with_agent(
        node,
        behavior,
        &prompt_builder,
        &agent,
        prompt,
        background_tool_registry,
    )
    .await
}

async fn run_oneshot_with_agent<M: CompletionModel + 'static>(
    node: Arc<EmbeddedNode>,
    behavior: &AgentBehavior,
    prompt_builder: &LayeredPromptBuilder,
    agent: &Agent<M>,
    prompt: &str,
    background_tool_registry: BackgroundToolRegistry,
) -> Result<OneshotRunResult> {
    let hook = DefraSessionHook::with_identity(
        node,
        &behavior.behavior_id,
        behavior.agent_did(),
        FailurePolicy::default(),
    )
    .with_background_tool_registry(background_tool_registry);
    let history = prompt_builder.build(&[], &[]).await?.messages;

    let response = agent
        .prompt(prompt)
        .with_history(&history)
        .with_hook(hook.clone())
        .await
        .map_err(|error| anyhow!("agent prompt failed: {error}"));

    let session_id = hook.session_id().await;
    let close_result = hook.close().await;

    match response {
        Ok(response_text) => {
            let session_id = session_id.context("one-shot run did not create a session")?;
            close_result.with_context(|| format!("closing one-shot session {session_id}"))?;

            Ok(OneshotRunResult {
                session_id,
                response_text,
            })
        }
        Err(error) => {
            if let Some(session_id) = session_id {
                if let Err(close_error) = close_result {
                    return Err(anyhow!(
                        "agent prompt failed: {error}; additionally failed to close session {session_id}: {close_error}"
                    ));
                }
            } else if let Err(close_error) = close_result {
                return Err(anyhow!(
                    "agent prompt failed: {error}; additionally failed to close one-shot hook: {close_error}"
                ));
            }

            Err(error)
        }
    }
}
