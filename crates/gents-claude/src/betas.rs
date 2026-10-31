//! The `anthropic-beta` registry and the per-request selection
//! (`chunk-dhhbffxt.js` registry, `chunk-v2tvd4xz.js` base list `DK` and
//! agentic additions `kvn`, `chunk-ps7vnt4g.js` query and per-attempt pushes).
//!
//! The selection reproduces the order in which Claude Code 2.1.311 pushes betas
//! onto a request: the base list, the agentic additions, the query additions,
//! then the per-attempt additions; a later push never duplicates an earlier
//! one. GrowthBook flags are evaluated at their shipped defaults and features a
//! Gents request never uses (advisor, task budgets, fast mode, auto mode, server
//! fallbacks, cache eviction and keepalive, message threads, context hints) are
//! not selected.

pub const CLAUDE_CODE: &str = "claude-code-20250219";
pub const OAUTH: &str = "oauth-2025-04-20";
pub const CONTEXT_1M: &str = "context-1m-2025-08-07";
pub const INTERLEAVED_THINKING: &str = "interleaved-thinking-2025-05-14";
pub const REDACT_THINKING: &str = "redact-thinking-2026-02-12";
pub const CONTEXT_MANAGEMENT: &str = "context-management-2025-06-27";
pub const STRUCTURED_OUTPUTS: &str = "structured-outputs-2025-12-15";
pub const WEB_SEARCH: &str = "web-search-2025-03-05";
pub const PROMPT_CACHING_SCOPE: &str = "prompt-caching-scope-2026-01-05";
pub const PER_TURN_CONTROL: &str = "per-turn-control-2026-07-01";
pub const MID_CONVERSATION_TOOL_CHANGES: &str = "mid-conversation-tool-changes-2026-07-01";
pub const THINKING_TOKEN_COUNT: &str = "thinking-token-count-2026-05-13";
pub const EFFORT: &str = "effort-2025-11-24";
pub const THINKING_BINDING_CONTROLS: &str = "thinking-binding-controls-2026-08-01";
pub const THINKING_DISPLAY_UPDATES: &str = "thinking-display-updates-2026-08-18";
pub const EXTENDED_CACHE_TTL: &str = "extended-cache-ttl-2025-04-11";
pub const CACHE_DIAGNOSIS: &str = "cache-diagnosis-2026-04-07";

/// Claude Code `Te()` provider kinds that change the beta set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    FirstParty,
    AnthropicAws,
    Mantle,
    Foundry,
    Vertex,
    Bedrock,
    AnthropicGoogleCloud,
    Gateway,
}

impl Provider {
    /// `upe()`: providers that accept first-party-only betas.
    fn first_party_like(self) -> bool {
        matches!(
            self,
            Self::FirstParty | Self::AnthropicAws | Self::AnthropicGoogleCloud | Self::Foundry
        )
    }

    /// `Gq()`: providers that accept capability betas.
    fn capability_betas(self) -> bool {
        self.first_party_like() || self == Self::Mantle
    }
}

/// The model facts the selection reads, taken from the catalog entry.
#[derive(Clone, Copy, Debug)]
pub struct ModelTraits<'a> {
    /// Canonical model id (`Je()`), e.g. `claude-opus-5-5`.
    pub canonical: &'a str,
    /// Catalog capability names.
    pub capabilities: &'a [&'a str],
    /// The model serves a 1M context without the `[1m]` beta.
    pub native_1m: bool,
    /// The model supports extended thinking at all.
    pub thinking_supported: bool,
}

impl ModelTraits<'_> {
    fn has(&self, capability: &str) -> bool {
        self.capabilities.contains(&capability)
    }

    /// `Svn`: interleaved thinking; capability-beta providers allow it for
    /// every model except the Claude 3 generation.
    fn interleaved_thinking(&self, provider: Provider) -> bool {
        if provider == Provider::Foundry {
            return true;
        }
        if provider.capability_betas() {
            return !self.canonical.contains("claude-3-");
        }
        !(self.canonical == "claude-haiku-4-5" || self.canonical.contains("claude-3-"))
    }

    /// `OK`: the model takes the context-management beta. Foundry takes every
    /// model, a capability-beta provider every non-Claude-3 model, any other
    /// provider the catalog capability or mythos-5. The beta and the body's
    /// `context_management` field both follow this one decision.
    pub fn context_management(&self, provider: Provider) -> bool {
        if provider == Provider::Foundry {
            return true;
        }
        if provider.capability_betas() {
            return !self.canonical.contains("claude-3-");
        }
        self.has("context_management") || self.canonical == "claude-mythos-5"
    }

    /// `yYn`: the thinking-token-count and thinking-display betas apply to
    /// the model on the provider.
    fn thinking_betas(&self, request: &Request) -> bool {
        !request.experimental_betas_disabled
            && self.interleaved_thinking(request.provider)
            && (request.provider.first_party_like() || !older_than_opus_4_7(self.canonical))
    }
}

/// Claude Code's model order (`ow`), oldest first.
const MODEL_ORDER: [&str; 10] = [
    "claude-opus-4-0",
    "claude-sonnet-4-0",
    "claude-opus-4-1",
    "claude-sonnet-4-5",
    "claude-haiku-4-5",
    "claude-opus-4-5",
    "claude-opus-4-6",
    "claude-sonnet-4-6",
    "claude-opus-4-7",
    "claude-opus-4-8",
];

/// Index of `claude-opus-4-7` in [`MODEL_ORDER`].
const OPUS_4_7: usize = 8;

/// `Or(id, "claude-opus-4-7")`: a Claude 3 model, or one listed before
/// opus-4-7; a model off the list is never older.
fn older_than_opus_4_7(id: &str) -> bool {
    id.contains("claude-3-")
        || MODEL_ORDER
            .iter()
            .position(|listed| *listed == id)
            .is_some_and(|at| at < OPUS_4_7)
}

/// The request facts the selection reads.
#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub provider: Provider,
    /// Claude Code `xi()`: the base URL is unset or `api.anthropic.com`.
    pub first_party_base_url: bool,
    /// A claude.ai subscription credential (`kt()`).
    pub subscriber: bool,
    /// The query runs the agent loop (`isAgenticQuery`): main thread and agents.
    pub agentic: bool,
    /// An interactive session (`!_e()`).
    pub interactive: bool,
    /// The operator asked to see thinking summaries (`showThinkingSummaries`).
    pub show_thinking_summaries: bool,
    /// Thinking is on for this request (`Wu`).
    pub thinking: bool,
    /// The request was made against a `[1m]` model name.
    pub long_context_suffix: bool,
    /// The cache TTL decision is one hour.
    pub cache_one_hour: bool,
    /// The request asks for a structured output format.
    pub output_format: bool,
    /// `CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS` or the organization's policy (`aW()`).
    pub experimental_betas_disabled: bool,
}

/// The betas of one request attempt in Claude Code's push order. O(1): the
/// list has a fixed bound of 17 entries.
pub fn select(model: &ModelTraits<'_>, request: &Request) -> Vec<&'static str> {
    let mut betas: Vec<&'static str> = Vec::with_capacity(16);
    let first_party_only =
        request.provider.first_party_like() && !request.experimental_betas_disabled;
    let capability_betas =
        request.provider.capability_betas() && !request.experimental_betas_disabled;
    let interleaved = model.interleaved_thinking(request.provider);
    let first_party_default_url =
        request.provider == Provider::FirstParty && request.first_party_base_url;

    // DK, the base list.
    if !model.canonical.contains("haiku") {
        betas.push(CLAUDE_CODE);
    }
    if request.subscriber {
        betas.push(OAUTH);
    }
    if request.long_context_suffix && !model.native_1m {
        betas.push(CONTEXT_1M);
    }
    if interleaved {
        betas.push(INTERLEAVED_THINKING);
    }
    if first_party_only && interleaved && request.interactive && !request.show_thinking_summaries {
        betas.push(REDACT_THINKING);
    }
    if capability_betas && model.context_management(request.provider) {
        betas.push(CONTEXT_MANAGEMENT);
    }
    if request.provider == Provider::Foundry
        || (request.provider == Provider::Vertex && !model.canonical.contains("claude-3-"))
    {
        betas.push(WEB_SEARCH);
    }
    // `_6()`: the global cache scope needs first-party-only betas on the default base URL.
    if first_party_only
        && request.first_party_base_url
        && matches!(
            request.provider,
            Provider::FirstParty | Provider::AnthropicAws
        )
    {
        betas.push(PROMPT_CACHING_SCOPE);
    }

    // kvn, the agentic additions.
    push_once(&mut betas, CLAUDE_CODE, request.agentic);
    push_once(
        &mut betas,
        PER_TURN_CONTROL,
        first_party_only && capability_betas && model.has("per_turn_effort"),
    );
    push_once(
        &mut betas,
        MID_CONVERSATION_TOOL_CHANGES,
        first_party_only && model.has("mid_conv_tool_change"),
    );

    // Query additions: `yYn` gates the thinking token count.
    push_once(
        &mut betas,
        THINKING_TOKEN_COUNT,
        model.thinking_supported && model.thinking_betas(request),
    );

    // Per-attempt additions (`zN`).
    // `S8e` pushes the effort beta exactly when `QS` holds.
    push_once(
        &mut betas,
        EFFORT,
        crate::body::effort_capable(
            model.canonical,
            model.has("effort"),
            request.provider.capability_betas(),
        ),
    );
    push_once(&mut betas, STRUCTURED_OUTPUTS, request.output_format);
    // `RR`: latched for first party on the default base URL (`Usn`) while the
    // session's thinking is not disabled.
    push_once(
        &mut betas,
        THINKING_BINDING_CONTROLS,
        first_party_only && first_party_default_url && request.thinking,
    );
    // Display updates replace redacted thinking whenever a thinking request takes them.
    if display_updates(model, request) {
        push_once(&mut betas, THINKING_DISPLAY_UPDATES, true);
        betas.retain(|beta| *beta != REDACT_THINKING);
    }
    push_once(
        &mut betas,
        EXTENDED_CACHE_TTL,
        request.cache_one_hour && first_party_only,
    );
    // `Kfn`: every first-party request on the default base URL; only the body's
    // `diagnostics` field depends on the query source.
    push_once(
        &mut betas,
        CACHE_DIAGNOSIS,
        first_party_only && first_party_default_url,
    );
    // `ree` moves these to Bedrock's request body (`anthropic_beta`), which the
    // Bedrock transport writes.
    if request.provider == Provider::Bedrock {
        betas.retain(|beta| !BEDROCK_BODY_BETAS.contains(beta));
    }
    // `oar`: a provider that is not first-party-like only receives this subset.
    if !request.provider.first_party_like() {
        betas.retain(|beta| THIRD_PARTY_BETAS.contains(beta));
    }
    betas
}

/// Whether the request takes `thinking-display-updates` and sends
/// `thinking.display = "updates"` (`Xon` defaults to `connector_text`, `yYn`
/// gates the beta): a thinking request without summaries on a model that takes
/// the thinking betas (`upe() || aXo(model)`). `select` and the body's
/// thinking decision share this one predicate.
pub fn display_updates(model: &ModelTraits<'_>, request: &Request) -> bool {
    request.thinking && !request.show_thinking_summaries && model.thinking_betas(request)
}

/// Betas Bedrock takes in the request body rather than the header (`FEn`).
pub const BEDROCK_BODY_BETAS: [&str; 2] = [INTERLEAVED_THINKING, CONTEXT_1M];

/// The betas sent to a provider that is not first-party-like (`sw`), limited to
/// the ones this selection can produce.
const THIRD_PARTY_BETAS: [&str; 10] = [
    CLAUDE_CODE,
    INTERLEAVED_THINKING,
    CONTEXT_1M,
    CONTEXT_MANAGEMENT,
    STRUCTURED_OUTPUTS,
    WEB_SEARCH,
    EFFORT,
    THINKING_TOKEN_COUNT,
    THINKING_BINDING_CONTROLS,
    THINKING_DISPLAY_UPDATES,
];

/// The `anthropic-beta` header value: `Array#toString`, commas without spaces.
pub fn header_value(betas: &[&str]) -> String {
    betas.join(",")
}

fn push_once(betas: &mut Vec<&'static str>, beta: &'static str, when: bool) {
    if when && !betas.contains(&beta) {
        betas.push(beta);
    }
}

#[cfg(test)]
#[path = "betas_tests.rs"]
mod tests;
