use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use rig::agent::Agent;
use rig::client::CompletionClient;
use rig::completion::CompletionModel;
use rig::completion::Prompt;
use rig::tool::ToolDyn;

use crate::config::DaemonConfig;
use crate::hook::DefraSessionHook;
use crate::prompt::{LayeredPromptBuilder, PromptBuilder};
use crate::schema::ensure_schemas;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneshotRunResult {
    pub session_id: String,
    pub response_text: String,
}

pub async fn run_openai_oneshot(
    node: Arc<EmbeddedNode>,
    config: &DaemonConfig,
    tools: Vec<Box<dyn ToolDyn>>,
    prompt: &str,
) -> Result<OneshotRunResult> {
    ensure_schemas(node.as_ref()).await?;

    let api_key = std::env::var("AGENT_DAEMON_API_KEY").unwrap_or_else(|_| "no-key".to_string());
    let prompt_builder = LayeredPromptBuilder::new(config);
    let preamble = prompt_builder.preamble().to_string();

    let openai_client: rig::providers::openai::CompletionsClient =
        rig::providers::openai::CompletionsClient::builder()
            .api_key(&api_key)
            .base_url(&config.model_endpoint)
            .build()
            .with_context(|| {
                format!(
                    "building OpenAI-compatible client for {}",
                    config.model_endpoint
                )
            })?;

    let agent = if tools.is_empty() {
        openai_client
            .agent(&config.model_name)
            .preamble(&preamble)
            .default_max_turns(config.max_turns)
            .build()
    } else {
        openai_client
            .agent(&config.model_name)
            .preamble(&preamble)
            .default_max_turns(config.max_turns)
            .tools(tools)
            .build()
    };

    run_oneshot_with_agent(node, config, &prompt_builder, &agent, prompt).await
}

async fn run_oneshot_with_agent<M: CompletionModel>(
    node: Arc<EmbeddedNode>,
    config: &DaemonConfig,
    prompt_builder: &LayeredPromptBuilder,
    agent: &Agent<M>,
    prompt: &str,
) -> Result<OneshotRunResult> {
    let hook = DefraSessionHook::new(node, &config.data_room);
    let mut history = prompt_builder.build(&[], &[]).await?.messages;

    let response = agent
        .prompt(prompt)
        .with_history(&mut history)
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
