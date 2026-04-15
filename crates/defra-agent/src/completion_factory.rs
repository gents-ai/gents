use rig::agent::{Agent, AgentBuilder, PromptHook};
use rig::client::CompletionClient;
use rig::completion::CompletionModel;
use rig::message::ToolChoice;
use rig::tool::ToolDyn;

use crate::admission::{AdmissionRegistry, AdmittedCompletionClient};
use crate::backend_provider::BackendProviderKind;
use crate::config::BehaviorConfig;

pub(crate) fn build_agent<C>(
    client: &C,
    behavior: &BehaviorConfig,
    preamble: &str,
    tools: Vec<Box<dyn ToolDyn>>,
) -> Agent<C::CompletionModel>
where
    C: CompletionClient,
{
    let builder = configure_agent_builder(
        client
            .agent(&behavior.model_name)
            .preamble(preamble)
            .default_max_turns(behavior.max_turns),
        behavior,
        tools.len(),
    );

    if tools.is_empty() {
        builder.build()
    } else {
        builder.tools(tools).build()
    }
}

pub(crate) fn build_admitted_agent<C>(
    client: C,
    admission: AdmissionRegistry,
    behavior: &BehaviorConfig,
    preamble: &str,
    tools: Vec<Box<dyn ToolDyn>>,
) -> Agent<<AdmittedCompletionClient<C> as CompletionClient>::CompletionModel>
where
    C: CompletionClient,
    C::CompletionModel: 'static,
    <C::CompletionModel as CompletionModel>::Response: 'static,
    <C::CompletionModel as CompletionModel>::StreamingResponse: 'static,
{
    let client = AdmittedCompletionClient::new(client, admission);
    let builder = configure_agent_builder(
        client
            .agent(&behavior.model_name)
            .preamble(preamble)
            .default_max_turns(behavior.max_turns),
        behavior,
        tools.len(),
    );

    if tools.is_empty() {
        builder.build()
    } else {
        builder.tools(tools).build()
    }
}

fn configure_agent_builder<M, P, ToolState>(
    mut builder: AgentBuilder<M, P, ToolState>,
    behavior: &BehaviorConfig,
    tool_count: usize,
) -> AgentBuilder<M, P, ToolState>
where
    M: CompletionModel,
    P: PromptHook<M>,
{
    if tool_count > 0 {
        builder = builder.tool_choice(ToolChoice::Auto);
    }

    if let Some(additional_params) = provider_additional_params(behavior.backend_provider_kind) {
        builder = builder.additional_params(additional_params);
    }

    builder
}

fn provider_additional_params(kind: BackendProviderKind) -> Option<serde_json::Value> {
    match kind {
        BackendProviderKind::OpenAiCompatible => None,
        BackendProviderKind::OpenRouter => Some(
            rig::providers::openrouter::ProviderPreferences::new()
                .require_parameters(true)
                .to_json(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrouter_additional_params_require_parameters() {
        let value = provider_additional_params(BackendProviderKind::OpenRouter)
            .expect("OpenRouter should contribute additional params");

        assert_eq!(value["provider"]["require_parameters"], true);
    }

    #[test]
    fn openai_compatible_has_no_provider_specific_additional_params() {
        assert!(provider_additional_params(BackendProviderKind::OpenAiCompatible).is_none());
    }
}
