//! The typed Claude Code Messages request for one Gents completion request.
//!
//! The planner resolves the catalog model, the wire mode of the request's
//! origin and the per-request attribution fields, then assembles every body
//! field through the `gents_claude` owners (Lean `PromptAssembly.ClaudeBody`,
//! `ClaudeOrigin`, `ClaudeCache`, `ClaudeMap`, `ClaudeAttribution`). The
//! request context travels as the reserved additional parameter
//! [`CLAUDE_REQUEST_PARAM`], which never reaches the wire because the body is
//! typed.

use gents_claude::attribution::{self, Fields, Provider as AttributionProvider};
use gents_claude::betas::{self, ModelTraits, Provider};
use gents_claude::body::{self, ModelFacts, ThinkingConfig};
use gents_claude::cache;
use gents_claude::catalog;
use gents_claude::metadata::{self, UserIdentity};
use gents_claude::origin::{Cause, Kind, Origin, WireMode};
use gents_claude::profile;
use gents_claude::system::{self, SessionMode};
use gents_claude::wire::{CacheControl, CacheTtl, MessagesRequest, Metadata, OutputConfig, Tool};
use gents_protocol::message::{Message, UserContent};
use serde::{Deserialize, Serialize};
use serde_json::value::{to_raw_value, RawValue};
use serde_json::Value;

use super::history_params::{attach_raw_arguments, previous_request_id};
use super::messages::{system_rows, wire_messages};
use super::{
    CompletionRequest, MessagesBodyError, ADVERTISED_REASONING_EFFORTS_PARAM,
    CACHE_PREFIX_BREAKPOINT_PARAM, SESSION_KEY_PARAM,
};
use crate::prompt::{is_compaction_summary, is_system_reminder};

/// Request parameter (never sent) carrying the [`ClaudeRequestContext`].
pub const CLAUDE_REQUEST_PARAM: &str = "_gents_claude_request";

/// The identity a subagent request announces in its agent headers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeAgentIdentity {
    /// The subagent's id (`gents_claude::metadata::agent_id` of its session).
    pub agent_id: String,
    /// The id of the subagent that spawned this one; absent when the main thread did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
}

/// What a request is, supplied by the caller that knows the session and the
/// request; an absent context is a turn of a session a client opened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeRequestContext {
    /// Where the request came from and what it is for.
    pub origin: Origin,
    /// The Gents id of the main session (a subagent shares its root's); the Claude session UUID
    /// derives from it.
    pub session_key: String,
    /// The Gents request document id; the Claude prompt UUID derives from it.
    pub prompt_key: String,
    /// The Gents id of the session a forked or teammate session came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// The agent identity of a subagent turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<ClaudeAgentIdentity>,
}

impl ClaudeRequestContext {
    /// A turn of a session a client opened, the context of a request that carries none.
    pub fn client_turn(session_key: &str) -> Self {
        Self {
            origin: Origin {
                cause: Cause::Client,
                root_client: true,
                scheduled: false,
                kind: Kind::Turn,
                appends_system_prompt: false,
            },
            session_key: session_key.to_owned(),
            prompt_key: String::new(),
            parent_session_id: None,
            agent: None,
        }
    }

    /// The context a request carries, or the client turn of its rotation session key.
    pub fn of_request(request: &CompletionRequest) -> Result<Self, MessagesBodyError> {
        let params = request.additional_params.as_ref();
        match params.and_then(|params| params.get(CLAUDE_REQUEST_PARAM)) {
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|error| MessagesBodyError::Context(error.to_string())),
            None => Ok(Self::client_turn(
                params
                    .and_then(|params| params.get(SESSION_KEY_PARAM))
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            )),
        }
    }

    /// The additional-parameter value that carries this context.
    pub fn to_param(&self) -> Result<Value, MessagesBodyError> {
        serde_json::to_value(self).map_err(|error| MessagesBodyError::Context(error.to_string()))
    }

    /// The Claude session and prompt UUIDs: stable per Gents session and request, so a retry
    /// and the tool loop of one request share them.
    pub fn ids(&self) -> ClaudeRequestIds {
        ClaudeRequestIds {
            session: metadata::stable_uuid(self.session_key.as_bytes()),
            prompt: metadata::stable_uuid(self.prompt_key.as_bytes()),
        }
    }
}

/// The Claude session and prompt UUIDs of a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeRequestIds {
    /// `metadata.user_id.session_id` and `X-Claude-Code-Session-Id`.
    pub session: String,
    /// `cc_prompt_id` and `x-claude-code-prompt-id`.
    pub prompt: String,
}

/// The account a request is sent with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeAccountContext {
    /// The 64 hex characters of the install id (`metadata::device_id`).
    pub device_id: String,
    /// The account UUID, empty when unknown.
    pub account_uuid: String,
    /// The credential is a subscription sign-in.
    pub subscriber: bool,
    /// The account is serving through paid overage.
    pub using_overage: bool,
    /// The credential is an API key.
    pub api_key: bool,
}

impl ClaudeAccountContext {
    /// A fixed neutral account for estimation and the replay frontier: the
    /// metadata, cache lifetime and betas are outside the replayed prefix.
    pub fn projection() -> Self {
        Self {
            device_id: "0".repeat(64),
            account_uuid: String::new(),
            subscriber: true,
            using_overage: false,
            api_key: false,
        }
    }
}

/// Everything the send path needs from one planned request.
#[derive(Debug, Clone)]
pub struct ClaudeRequestPlan {
    /// The typed body.
    pub body: MessagesRequest,
    /// The `anthropic-beta` names in Claude Code's order.
    pub betas: Vec<&'static str>,
    /// The wire mode of the request's origin.
    pub mode: WireMode,
    /// The Claude session UUID.
    pub session_id: String,
    /// The Claude prompt UUID.
    pub prompt_id: String,
    /// The agent headers of a subagent turn.
    pub agent: Option<ClaudeAgentIdentity>,
}

/// The planned request of `request` for `account`.
pub fn plan_claude_request(
    model: &str,
    request: &CompletionRequest,
    account: &ClaudeAccountContext,
) -> Result<ClaudeRequestPlan, MessagesBodyError> {
    let context = ClaudeRequestContext::of_request(request)?;
    plan_with_ids(model, request, account, &context, &context.ids())
}

/// [`plan_claude_request`] with the session and prompt UUIDs supplied, for callers
/// that fix them (the generated conformance cases).
pub fn plan_with_ids(
    model: &str,
    request: &CompletionRequest,
    account: &ClaudeAccountContext,
    context: &ClaudeRequestContext,
    ids: &ClaudeRequestIds,
) -> Result<ClaudeRequestPlan, MessagesBodyError> {
    let mut history: Vec<Message> = request
        .chat_history
        .iter()
        .map(crate::rig_compat::from_rig_message)
        .collect();
    attach_raw_arguments(&mut history, request.additional_params.as_ref());
    let origin = context.origin;
    let mode = origin.wire_mode();
    let resolved = ResolvedModel::of(model);
    let params = request.additional_params.as_ref();
    let previous_message_id = previous_message_id(&history);

    let mut fields = body::Request {
        thinking: match origin.kind {
            Kind::Turn | Kind::Compaction => ThinkingConfig::Adaptive,
            Kind::Title | Kind::OneShot => ThinkingConfig::Disabled { mechanical: true },
        },
        max_tokens_override: None,
        requested_effort: params.and_then(selected_effort),
        first_party: true,
        first_party_default_url: true,
        experimental_betas_disabled: false,
        agentic: mode.agentic,
        fork: origin.kind == Kind::Compaction,
        display_updates: false,
        context_management_beta: false,
        per_turn_control_beta: false,
        previous_message_id: previous_message_id.as_deref(),
    };
    let ttl = cache::ttl(
        account.subscriber,
        mode.main_thread_source,
        account.using_overage,
    );
    let traits = resolved.traits();
    let beta_request = betas::Request {
        provider: Provider::FirstParty,
        first_party_base_url: true,
        subscriber: account.subscriber,
        agentic: mode.agentic,
        interactive: mode.session == SessionMode::Interactive,
        show_thinking_summaries: false,
        thinking: body::thinking_on(&resolved.facts, &fields),
        long_context_suffix: resolved.long_context,
        cache_one_hour: ttl == CacheTtl::OneHour,
        output_format: request.output_schema.is_some(),
        experimental_betas_disabled: false,
    };
    let selected = betas::select(&traits, &beta_request);
    fields.display_updates = betas::display_updates(&traits, &beta_request);
    fields.context_management_beta = selected.contains(&betas::CONTEXT_MANAGEMENT);
    fields.per_turn_control_beta = selected.contains(&betas::PER_TURN_CONTROL);
    let decision = body::evaluate(&resolved.facts, &fields);

    let caching = origin.kind != Kind::Title;
    let marker = CacheControl::for_ttl(ttl);
    let skip_write = params
        .and_then(|params| params.get(CACHE_PREFIX_BREAKPOINT_PARAM))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut messages = wire_messages(&history)?;
    cache::place(&mut messages, caching, skip_write, &marker);
    if let Some(effort) = decision.per_turn_effort {
        body::insert_per_turn_row(&mut messages, effort);
    }

    let position =
        origin.turn_position(prompt_rows(&history), history.iter().any(is_checkpoint_row));
    let stamped = matches!(origin.kind, Kind::Turn | Kind::Compaction);
    let header = attribution::header(&Fields {
        version: profile::VERSION.to_owned(),
        fingerprint: attribution::fingerprint(first_user_text(&history), profile::VERSION),
        entrypoint: mode.entrypoint.to_owned(),
        provider: AttributionProvider::FirstParty,
        first_party_base_url: true,
        workload: mode.workload.map(str::to_owned),
        subagent: mode.subagent,
        previous_request_id: previous_request_id(params)
            .map(str::to_owned)
            .filter(|_| stamped),
        prompt_id: Some(ids.prompt.clone()),
        turn_origin: origin.turn_origin().map(str::to_owned),
        prompt_index: position.map(|(prompt, _)| prompt),
        turn_index: position.map(|(_, turn)| turn),
        gateway: None,
        project: None,
    });
    let preamble = request
        .preamble
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let system = system::system_blocks(
        [header.as_str(), system::identity_for(false, mode.session)]
            .into_iter()
            .chain(preamble)
            .chain(system_rows(&history)),
        caching.then(|| marker.clone()),
    );

    let tools = request
        .tools
        .iter()
        .map(|tool| {
            Ok(Tool {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: to_raw_value(&tool.parameters)?,
                eager_input_streaming: false,
            })
        })
        .collect::<Result<Vec<_>, serde_json::Error>>()?;
    let user_id = metadata::user_id(
        &UserIdentity {
            device_id: account.device_id.clone(),
            account_uuid: account.account_uuid.clone(),
            session_id: ids.session.clone(),
            parent_session_id: context
                .parent_session_id
                .as_deref()
                .map(|id| metadata::stable_uuid(id.as_bytes())),
        },
        &[],
    )?;
    let format = request
        .output_schema
        .as_ref()
        .map(output_format)
        .transpose()?;

    Ok(ClaudeRequestPlan {
        body: MessagesRequest {
            model: resolved.wire_id,
            messages,
            system,
            tools: Some(tools),
            metadata: Metadata { user_id },
            max_tokens: decision.max_tokens,
            thinking: decision.thinking,
            temperature: decision.temperature,
            context_management: decision.context_management,
            output_config: Some(OutputConfig {
                effort: decision.effort.map(|effort| effort.as_str().to_owned()),
                format,
            }),
            diagnostics: decision.diagnostics,
            stream: true,
        },
        betas: selected,
        mode,
        session_id: ids.session.clone(),
        prompt_id: ids.prompt.clone(),
        agent: context.agent.clone().filter(|_| mode.subagent),
    })
}

/// The model the request names: the id sent, the facts and the catalog entry.
struct ResolvedModel {
    wire_id: String,
    long_context: bool,
    facts: ModelFacts,
    canonical: String,
    capabilities: &'static [&'static str],
    native_1m: bool,
}

impl ResolvedModel {
    /// Claude Code sends a model id as given (the `[1m]` suffix becomes a beta) and
    /// resolves an alias to the first-party id of its catalog entry.
    fn of(name: &str) -> Self {
        let base = catalog::strip_1m_suffix(name);
        let long_context = base.len() != name.len();
        let aliased = catalog::resolve_alias(base, None).and_then(catalog::find);
        match aliased.or_else(|| catalog::find(base)) {
            Some(model) => Self {
                wire_id: if aliased.is_some() {
                    model
                        .id_for(catalog::CatalogProvider::FirstParty)
                        .unwrap_or(base)
                        .to_owned()
                } else {
                    base.to_owned()
                },
                long_context,
                facts: ModelFacts::of(model),
                canonical: model.id.to_owned(),
                capabilities: model.capabilities,
                native_1m: model.context.is_some_and(|context| context.native_1m),
            },
            None => Self {
                wire_id: base.to_owned(),
                long_context,
                facts: ModelFacts::unlisted(base),
                canonical: base.to_owned(),
                capabilities: &[],
                native_1m: false,
            },
        }
    }

    fn traits(&self) -> ModelTraits<'_> {
        ModelTraits {
            canonical: &self.canonical,
            capabilities: self.capabilities,
            native_1m: self.native_1m,
            thinking_supported: self.facts.thinking_supported,
        }
    }
}

/// The requested effort the agent's advertised levels allow (Lean
/// `ClaudeMap.selectedEffort`): unknown support selects nothing.
fn selected_effort(params: &Value) -> Option<&str> {
    let supported = params.get(ADVERTISED_REASONING_EFFORTS_PARAM)?.as_array()?;
    let effort = params["output_config"]["effort"].as_str()?;
    (supported.iter().any(|value| value.as_str() == Some(effort))
        && matches!(effort, "low" | "medium" | "high" | "xhigh" | "max"))
    .then_some(effort)
}

/// A request that asks for structured output must carry it; one that cannot is an
/// error, never a silently unguided request.
fn output_format(schema: &schemars::Schema) -> Result<Box<RawValue>, MessagesBodyError> {
    let unsendable = |reason: String| MessagesBodyError::UnsendableOutputSchema(reason);
    let value = serde_json::to_value(schema).map_err(|error| unsendable(error.to_string()))?;
    if !value.is_object() {
        return Err(unsendable("the schema is not a JSON object".to_string()));
    }
    let raw = to_raw_value(&value).map_err(|error| unsendable(error.to_string()))?;
    Ok(OutputConfig::json_schema_format(&raw)?)
}

/// The id of the last assistant message that Anthropic issued (`msg_`).
fn previous_message_id(history: &[Message]) -> Option<String> {
    history.iter().rev().find_map(|message| match message {
        Message::Assistant { id: Some(id), .. } if id.starts_with("msg_") => Some(id.clone()),
        _ => None,
    })
}

fn user_content(message: &Message) -> Option<&[UserContent]> {
    match message {
        Message::User { content } => Some(content),
        _ => None,
    }
}

fn is_checkpoint_row(message: &Message) -> bool {
    user_content(message).is_some_and(|content| {
        content.iter().any(
            |block| matches!(block, UserContent::Text(text) if is_compaction_summary(&text.text)),
        )
    })
}

/// The user rows that carry a prompt: text, no tool result and not a system reminder
/// (which covers the compaction summary).
fn prompt_rows(history: &[Message]) -> u64 {
    history
        .iter()
        .filter_map(user_content)
        .filter(|content| {
            content.iter().any(
                |block| matches!(block, UserContent::Text(text) if !is_system_reminder(&text.text)),
            ) && !content
                .iter()
                .any(|block| matches!(block, UserContent::ToolResult(_)))
        })
        .count() as u64
}

/// The text the version fingerprint hashes: the first text block of the first non-meta user row.
fn first_user_text(history: &[Message]) -> &str {
    history
        .iter()
        .filter_map(user_content)
        .find_map(|content| {
            content.iter().find_map(|block| match block {
                UserContent::Text(text) if !is_system_reminder(&text.text) => {
                    Some(text.text.as_str())
                }
                _ => None,
            })
        })
        .unwrap_or("")
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
