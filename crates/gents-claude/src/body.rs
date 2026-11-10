//! The Messages body fields Claude Code 2.1.311 derives from the model and the
//! request: `max_tokens`, `thinking`, `temperature`, `context_management`,
//! `output_config.effort`, `diagnostics` and the per-turn effort row (`zN` in
//! the query generator and the capability helpers `zci`, `Gci`, `E$e`, `vot`,
//! `oXo`, `rar`, `QS`, `lse`, `vEt`, `Xqr`, `Wsn`). Lean:
//! `PromptAssembly.ClaudeBody`, whose generated cases bind every function here.
//!
//! The decisions are pure. [`ModelFacts::of`] derives the model side from the
//! catalog entry for a first-party request, which is the only provider a
//! Gents Claude request uses; the request side is the caller's data. Claude
//! Code's own environment knobs and server-served overrides are not inputs.

use serde_json::Number;

use crate::catalog;
use crate::wire::{
    ContextEdit, ContextManagement, Diagnostics, Message, MessageContent, OutputConfig, Role,
    Thinking, ThinkingDisplay,
};

/// Smallest `budget_tokens` Claude Code sends.
const MIN_BUDGET: u64 = 1024;

/// Claude Code's exclusion lists, as canonical ids: a model on one lacks the
/// capability the list names (`KEEPS_DISABLED_ALLOWED`: it does not reject a
/// disabled thinking mode). Every `claude-3-` model is on each list too.
const NO_EFFORT: [&str; 5] = [
    "claude-opus-4-0",
    "claude-opus-4-1",
    "claude-sonnet-4-0",
    "claude-sonnet-4-5",
    "claude-haiku-4-5",
];
const NO_MAX_EFFORT: [&str; 6] = [
    "claude-opus-4-0",
    "claude-opus-4-1",
    "claude-opus-4-5",
    "claude-sonnet-4-0",
    "claude-sonnet-4-5",
    "claude-haiku-4-5",
];
const NO_XHIGH_EFFORT: [&str; 8] = [
    "claude-opus-4-0",
    "claude-opus-4-1",
    "claude-opus-4-5",
    "claude-opus-4-6",
    "claude-sonnet-4-0",
    "claude-sonnet-4-5",
    "claude-sonnet-4-6",
    "claude-haiku-4-5",
];
const NO_ADAPTIVE: [&str; 6] = [
    "claude-opus-4-0",
    "claude-opus-4-1",
    "claude-opus-4-5",
    "claude-sonnet-4-0",
    "claude-sonnet-4-5",
    "claude-haiku-4-5",
];
const KEEPS_DISABLED_ALLOWED: [&str; 12] = [
    "claude-opus-4-0",
    "claude-opus-4-1",
    "claude-opus-4-5",
    "claude-opus-4-6",
    "claude-opus-4-7",
    "claude-opus-4-8",
    "claude-opus-5",
    "claude-sonnet-4-0",
    "claude-sonnet-4-5",
    "claude-sonnet-4-6",
    "claude-sonnet-5",
    "claude-haiku-4-5",
];
const TAKES_TEMPERATURE: [&str; 8] = [
    "claude-opus-4-0",
    "claude-sonnet-4-0",
    "claude-opus-4-1",
    "claude-sonnet-4-5",
    "claude-haiku-4-5",
    "claude-opus-4-5",
    "claude-opus-4-6",
    "claude-sonnet-4-6",
];

/// Claude Code `QS`: the model accepts `output_config.effort`. Outside the
/// exclusion list the catalog capability or mythos-5 decides, and a provider
/// that takes capability betas (`Gq`) accepts every remaining model.
pub fn effort_capable(id: &str, catalog_effort: bool, capability_provider: bool) -> bool {
    !(id.contains("claude-3-") || NO_EFFORT.contains(&id))
        && (catalog_effort || id == "claude-mythos-5" || capability_provider)
}

/// An effort level, ordered from `low` to `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effort {
    /// `low`.
    Low,
    /// `medium`.
    Medium,
    /// `high`.
    High,
    /// `xhigh`.
    Xhigh,
    /// `max`.
    Max,
}

impl Effort {
    /// The level named `name`, `None` for anything else.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "xhigh" => Self::Xhigh,
            "max" => Self::Max,
            _ => return None,
        })
    }

    /// The wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// What the model supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFacts {
    /// The model takes `{type: "adaptive"}` and ignores a budget.
    pub adaptive: bool,
    /// Thinking cannot be turned off: a plain disable becomes adaptive.
    pub always_thinks: bool,
    /// The model accepts a `thinking` field.
    pub thinking_supported: bool,
    /// The API rejects `{type: "disabled"}`.
    pub rejects_disabled: bool,
    /// The model accepts `temperature`.
    pub accepts_temperature: bool,
    /// The model accepts `output_config.effort`.
    pub effort_capable: bool,
    /// The model accepts the `max` effort.
    pub max_effort: bool,
    /// The model accepts the `xhigh` effort.
    pub xhigh_effort: bool,
    /// The model holds effort to `high` while thinking is disabled.
    pub caps_effort_when_disabled: bool,
    /// The model takes the per-turn effort control.
    pub per_turn_effort: bool,
    /// Default `max_tokens`.
    pub output_default: u64,
    /// Largest `max_tokens`.
    pub output_upper: u64,
    /// The effort sent when none is chosen.
    pub default_effort: Option<Effort>,
}

impl ModelFacts {
    /// The facts of a catalog model on the first-party provider.
    ///
    /// On a first-party provider the capability lists resolve to true for every
    /// model outside their exclusion list, so a catalog capability name adds
    /// nothing there; the names read are `rejects_disabled_thinking`,
    /// `thinking_disabled_effort_cap` and `per_turn_effort`.
    pub fn of(model: &catalog::Model) -> Self {
        Self::from_parts(
            model.id,
            model.capabilities,
            (
                model.max_output_tokens.default,
                model.max_output_tokens.upper,
            ),
            model.default_effort.and_then(Effort::parse),
        )
    }

    /// The facts of a model the catalog does not list: no capability names,
    /// Claude Code's fallback output limits (`claude-3-opus` and `-haiku` 4096,
    /// `claude-3-sonnet` 8192, otherwise 32000 up to 128000) and no default effort.
    /// The id-based exclusion lists still apply.
    pub fn unlisted(id: &str) -> Self {
        let limits = if id.contains("claude-3-opus") || id.contains("claude-3-haiku") {
            (4096, 4096)
        } else if id.contains("claude-3-sonnet") {
            (8192, 8192)
        } else {
            (32_000, 128_000)
        };
        Self::from_parts(id, &[], limits, None)
    }

    fn from_parts(
        id: &str,
        capabilities: &[&str],
        (output_default, output_upper): (u64, u64),
        default_effort: Option<Effort>,
    ) -> Self {
        let has = |capability: &str| capabilities.contains(&capability);
        let claude3 = id.contains("claude-3-");
        let listed = |list: &[&str]| claude3 || list.contains(&id);
        let rejects_disabled = !listed(&KEEPS_DISABLED_ALLOWED);
        Self {
            adaptive: !listed(&NO_ADAPTIVE),
            always_thinks: has("rejects_disabled_thinking") && rejects_disabled,
            thinking_supported: !claude3,
            rejects_disabled,
            accepts_temperature: listed(&TAKES_TEMPERATURE),
            effort_capable: effort_capable(id, has("effort"), true),
            max_effort: !listed(&NO_MAX_EFFORT),
            xhigh_effort: !listed(&NO_XHIGH_EFFORT),
            caps_effort_when_disabled: has("thinking_disabled_effort_cap") || id == "claude-opus-5",
            per_turn_effort: has("per_turn_effort"),
            output_default,
            output_upper,
            default_effort,
        }
    }
}

/// The session's thinking config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThinkingConfig {
    /// Adaptive thinking.
    #[default]
    Adaptive,
    /// Thinking off; `mechanical` marks a disable an always-thinking model must
    /// not override (titles and other helper calls).
    Disabled {
        /// The disable is mechanical.
        mechanical: bool,
    },
    /// Thinking on with an optional budget.
    Enabled {
        /// The requested budget.
        budget: Option<u64>,
    },
}

/// The request side of the body decisions.
#[derive(Debug, Clone, Copy, Default)]
pub struct Request<'a> {
    /// The session's thinking config.
    pub thinking: ThinkingConfig,
    /// The context-overflow retry value for `max_tokens`; zero is no override.
    pub max_tokens_override: Option<u64>,
    /// The effort the session asked for; an unknown name falls back to the default.
    pub requested_effort: Option<&'a str>,
    /// The request goes to the first-party provider.
    pub first_party: bool,
    /// The base URL is the first-party default.
    pub first_party_default_url: bool,
    /// Experimental betas are disabled.
    pub experimental_betas_disabled: bool,
    /// The query runs the agent loop (`isAgenticQuery`).
    pub agentic: bool,
    /// The call carries a fork point.
    pub fork: bool,
    /// The display-updates beta is taken, see [`crate::betas::display_updates`].
    pub display_updates: bool,
    /// `context-management-2025-06-27` is among the betas.
    pub context_management_beta: bool,
    /// `per-turn-control-2026-07-01` is among the betas.
    pub per_turn_control_beta: bool,
    /// The previous assistant message id, `None` on a conversation's first request.
    pub previous_message_id: Option<&'a str>,
}

/// Every decision of one request.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// `max_tokens`.
    pub max_tokens: u64,
    /// `thinking`.
    pub thinking: Option<Thinking>,
    /// `temperature`.
    pub temperature: Option<Number>,
    /// `context_management`.
    pub context_management: Option<ContextManagement>,
    /// `output_config.effort`.
    pub effort: Option<Effort>,
    /// `diagnostics`.
    pub diagnostics: Option<Diagnostics>,
    /// The effort of the per-turn row.
    pub per_turn_effort: Option<Effort>,
}

/// Every decision of one request.
pub fn evaluate(facts: &ModelFacts, request: &Request<'_>) -> Decision {
    Decision {
        max_tokens: max_tokens(facts, request),
        thinking: thinking(facts, request),
        temperature: temperature(facts, request),
        context_management: context_management(facts, request),
        effort: effort(facts, request),
        diagnostics: diagnostics(request),
        per_turn_effort: per_turn_effort(facts, request),
    }
}

/// `max_tokens`: the catalog default, lowered by a non-zero override.
pub fn max_tokens(facts: &ModelFacts, request: &Request<'_>) -> u64 {
    request
        .max_tokens_override
        .filter(|&n| n != 0)
        .unwrap_or(facts.output_default)
        .min(facts.output_default)
}

/// An always-thinking model turns a plain disable into adaptive.
fn resolved(facts: &ModelFacts, config: ThinkingConfig) -> ThinkingConfig {
    match config {
        ThinkingConfig::Disabled { mechanical: false } if facts.always_thinks => {
            ThinkingConfig::Adaptive
        }
        other => other,
    }
}

/// Whether thinking is on for the request.
pub fn thinking_on(facts: &ModelFacts, request: &Request<'_>) -> bool {
    !matches!(
        resolved(facts, request.thinking),
        ThinkingConfig::Disabled { .. }
    )
}

fn mechanically_disabled(request: &Request<'_>) -> bool {
    matches!(
        request.thinking,
        ThinkingConfig::Disabled { mechanical: true }
    )
}

/// The `thinking` field: adaptive models ignore a budget, other supported models
/// send the requested budget (or the upper limit minus one) clamped to
/// `[1024, max_tokens - 1]` with the floor winning, and a disable goes only to a
/// first-party model that accepts it.
pub fn thinking(facts: &ModelFacts, request: &Request<'_>) -> Option<Thinking> {
    let config = resolved(facts, request.thinking);
    let display = request.display_updates.then_some(ThinkingDisplay::Updates);
    if thinking_on(facts, request) && facts.thinking_supported {
        if facts.adaptive {
            return Some(Thinking::Adaptive { display });
        }
        let requested = match config {
            ThinkingConfig::Enabled { budget: Some(b) } => b,
            _ => facts.output_upper.saturating_sub(1),
        };
        let budget = requested
            .min(max_tokens(facts, request).saturating_sub(1))
            .max(MIN_BUDGET);
        return Some(Thinking::Enabled {
            budget_tokens: budget,
            display,
        });
    }
    (matches!(config, ThinkingConfig::Disabled { .. })
        && request.first_party
        && facts.thinking_supported
        && !facts.rejects_disabled)
        .then_some(Thinking::Disabled)
}

/// `temperature`: `1` only while thinking is off and the model takes one.
pub fn temperature(facts: &ModelFacts, request: &Request<'_>) -> Option<Number> {
    (!thinking_on(facts, request) && facts.accepts_temperature).then(|| Number::from(1u64))
}

/// `context_management`: the `clear_thinking_20251015` edit while thinking is on.
pub fn context_management(facts: &ModelFacts, request: &Request<'_>) -> Option<ContextManagement> {
    (thinking_on(facts, request) && request.context_management_beta).then(|| ContextManagement {
        edits: vec![ContextEdit::ClearThinking],
    })
}

/// The effort before the thinking-disabled clamp: the requested level, else the
/// model's default, else `high`, with `max` and `xhigh` held to `high` on a model
/// without them; `None` on a model without effort.
pub fn base_effort(facts: &ModelFacts, request: &Request<'_>) -> Option<Effort> {
    facts.effort_capable.then(|| {
        let chosen = request
            .requested_effort
            .and_then(Effort::parse)
            .or(facts.default_effort)
            .unwrap_or(Effort::High);
        match chosen {
            Effort::Max if !facts.max_effort => Effort::High,
            Effort::Xhigh if !facts.xhigh_effort => Effort::High,
            other => other,
        }
    })
}

/// `output_config.effort`: held to `high` while thinking is disabled when the
/// disable is mechanical or the model caps effort.
pub fn effort(facts: &ModelFacts, request: &Request<'_>) -> Option<Effort> {
    let capped = thinking(facts, request) == Some(Thinking::Disabled)
        && (mechanically_disabled(request) || facts.caps_effort_when_disabled);
    base_effort(facts, request).map(|e| {
        if capped && e > Effort::High {
            Effort::High
        } else {
            e
        }
    })
}

/// `diagnostics`: the previous assistant message id (`null` on the first request)
/// of a first-party agent-loop or fork request.
pub fn diagnostics(request: &Request<'_>) -> Option<Diagnostics> {
    (request.first_party
        && request.first_party_default_url
        && !request.experimental_betas_disabled
        && (request.agentic || request.fork))
        .then(|| Diagnostics {
            previous_message_id: request.previous_message_id.map(str::to_owned),
        })
}

/// The effort of the per-turn control row.
pub fn per_turn_effort(facts: &ModelFacts, request: &Request<'_>) -> Option<Effort> {
    if request.per_turn_control_beta && facts.per_turn_effort && !mechanically_disabled(request) {
        base_effort(facts, request)
    } else {
        None
    }
}

/// Where the per-turn effort row goes: after the first run of user messages.
/// `None` when no message is a user's.
pub fn row_index(is_user: impl Iterator<Item = bool>) -> Option<usize> {
    let mut rest = is_user.enumerate().skip_while(|(_, user)| !user);
    let (first, _) = rest.next()?;
    Some(first + 1 + rest.take_while(|(_, user)| *user).count())
}

/// Inserts the per-turn effort row, `{"role":"system","content":[],"output_config":{"effort":..}}`,
/// after the first run of user messages. Returns whether a row was inserted.
pub fn insert_per_turn_row(messages: &mut Vec<Message>, effort: Effort) -> bool {
    let Some(index) = row_index(messages.iter().map(|m| m.role == Role::User)) else {
        return false;
    };
    messages.insert(
        index,
        Message {
            role: Role::System,
            content: MessageContent::Blocks(Vec::new()),
            output_config: Some(OutputConfig {
                effort: Some(effort.as_str().to_owned()),
                format: None,
            }),
        },
    );
    true
}

#[cfg(test)]
#[path = "body_tests.rs"]
mod tests;
