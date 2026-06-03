//! Layered prompt construction for KV cache reuse.
//!
//! The prompt is assembled in a fixed order so vLLM's automatic prefix
//! caching can reuse KV computation across turns:
//!
//! ```text
//! ┌─────────────────────────────────────┐
//! │ Layer 1: Static system prompt       │  ← cached globally, never changes
//! │ Layer 2: Behavior context           │  ← cached per-behavior, set at init
//! ├─────────────────────────────────────┤
//! │ Layer 3: Compaction summaries       │  ← cached between compactions
//! ├─────────────────────────────────────┤
//! │ Layer 4: Conversation messages      │  ← grows each turn
//! └─────────────────────────────────────┘
//! ```
//!
//! Layers 1-2 form the preamble (Rig's system prompt). They're set once
//! at daemon startup and locked for the session lifetime. Tool definitions
//! live in Rig's separate `tools` field, also fixed at startup.
//!
//! Behavior updates flow through `<system-reminder>` tags injected into
//! conversation messages — never by mutating the preamble.

use anyhow::Result;
use rig::completion::message::{Message, Text, UserContent};
use rig::one_or_many::OneOrMany;

use crate::config::AgentBehavior;
use crate::tool_surface::ToolSurface;

const TITLE_GENERATION_SUFFIX: &str =
    "Generate concise conversation titles. Return only a lowercase hyphenated 3-5 word title. Never call tools. Never explain.";

/// Guidance appended to the preamble so the LLM knows how to discover and
/// invoke data-service tools via the meta-tool workflow.
const TOOL_DISCOVERY_GUIDANCE: &str = "\
## Tool Discovery

You have access to MCP data service tools via three meta-tools. These meta-tools \
are only for MCP data services; native direct tools such as file and bash tools \
are already present in your direct tool list and should not be described or \
called through a synthetic `native` service.

1. **discover_tools** — Browse or search available data services. Call with no \
arguments to see all services, or provide a query to filter. Returns service \
names, descriptions, and tool listings.

2. **describe_tool** — Get a compact contract for a specific MCP data-service tool. Call this \
before using call_tool so you know required arguments, optional arguments, \
defaults, constraints, examples, and unknown-field behavior. Set raw_schema=true \
only when you need the exact JSON Schema.

3. **call_tool** — Invoke a tool on an MCP data service. Pass the service_id, \
tool_name, and arguments object.

Workflow: discover_tools -> describe_tool -> call_tool
";

/// A fully constructed prompt ready for Rig's agent loop.
#[derive(Debug, Clone)]
pub struct BuiltPrompt {
    /// Preamble (layers 1+2). Set once, passed to Rig's AgentBuilder.
    /// Do NOT mutate after init — this is the cached prefix.
    pub preamble: String,
    /// Conversation messages (layers 3+4).
    pub messages: Vec<Message>,
    /// Estimated token count for the full prompt (preamble + messages).
    pub estimated_tokens: usize,
}

/// Builds prompts with a fixed prefix order for KV cache reuse.
pub trait PromptBuilder: Send + Sync {
    /// Build the message history for a turn.
    /// The preamble is fixed at init — only messages change per turn.
    fn build(
        &self,
        messages: &[Message],
        compaction_summaries: &[String],
    ) -> impl std::future::Future<Output = Result<BuiltPrompt>> + Send;
}

/// Layered prompt builder backed by loaded behavior configuration.
///
/// Created once at daemon startup. The preamble is assembled from the
/// config and frozen — all subsequent calls to `build()` only vary
/// the message portion (layers 3+4).
pub struct LayeredPromptBuilder {
    /// Frozen preamble (layers 1+2). Set at init, never mutated.
    preamble: String,
    /// Context window size for token budgeting.
    context_window: usize,
    /// Max output tokens reserved for the response.
    max_output_tokens: usize,
    /// The behavior's effective skills (D5) and the D3 tool ceiling, used to
    /// render explicitly-selected skill bodies as per-turn system reminders.
    /// Empty for non-behavior builders (e.g. title generation).
    skills: Vec<crate::skills::Skill>,
    skill_ceiling: crate::skills::SkillToolCeiling,
}

impl LayeredPromptBuilder {
    pub fn new(behavior: &AgentBehavior, tool_surface: &ToolSurface) -> Self {
        let tool_names = tool_surface.tool_names();
        let tool_refs = tool_names.iter().map(String::as_str).collect::<Vec<_>>();
        let mut builder = Self::for_behavior(
            &behavior.system_prompt,
            &behavior.behavior_id,
            &tool_refs,
            tool_surface.includes_meta_tools(),
            behavior.context_window,
            behavior.max_output_tokens,
        );
        // Progressive disclosure (D2): the behavior's effective skills (D5) go
        // into the cached preamble as a CATALOG (name + description) only. The
        // model loads a skill's full body on demand via the `load_skill` tool
        // (registered in run_behavior). This keeps the always-present cost to
        // one line per skill so a large skill library stays usable.
        if let Some(catalog) = crate::skills::render_skill_catalog(&behavior.skills) {
            builder.preamble.push_str("\n\n");
            builder.preamble.push_str(&catalog);
        }
        // Retain the effective set + ceiling so an explicit skill selection (the
        // Codex "pill", carried on the request) can be deterministically injected
        // per turn via `selected_skill_reminders`.
        builder.skills = behavior.skills.clone();
        builder.skill_ceiling = crate::skills::skill_tool_ceiling(
            tool_names.iter().cloned(),
            tool_surface.allowed_mcp_service_ids(),
            tool_surface.includes_meta_tools(),
        );
        builder
    }

    /// Render per-turn system reminders for explicitly-selected skills.
    ///
    /// Each id is resolved against this behavior's EFFECTIVE set (D5): a skill
    /// not in the set — un-opted-in, excluded, disabled, or another principal's
    /// — is silently skipped, so the explicit pick can never escalate beyond
    /// what the behavior already has. The body is rendered with the real D3
    /// ceiling, so the unavailable-tools degrade note is included (parity with
    /// the model-driven `load_skill` path). This is the deterministic,
    /// runtime-side activation of an explicit user selection.
    pub fn selected_skill_reminders(&self, selected_ids: &[String]) -> Vec<Message> {
        let mut seen = std::collections::HashSet::new();
        let mut reminders = Vec::new();
        for id in selected_ids {
            let id = id.trim();
            if id.is_empty() || !seen.insert(id.to_string()) {
                continue;
            }
            if let Some(skill) = crate::skills::find_skill(&self.skills, id) {
                let body = crate::skills::render_activated_skill(skill, &self.skill_ceiling);
                reminders.push(Self::system_reminder(&body));
            }
        }
        reminders
    }

    pub fn for_behavior(
        system_prompt: &str,
        behavior_name: &str,
        tool_names: &[&str],
        include_meta_tool_guidance: bool,
        context_window: usize,
        max_output_tokens: usize,
    ) -> Self {
        let preamble = build_preamble(
            system_prompt,
            behavior_name,
            tool_names,
            include_meta_tool_guidance,
        );
        Self {
            preamble,
            context_window,
            max_output_tokens,
            skills: Vec::new(),
            skill_ceiling: crate::skills::SkillToolCeiling::default(),
        }
    }

    /// Get the frozen preamble for Rig's AgentBuilder.
    pub fn preamble(&self) -> &str {
        &self.preamble
    }

    /// Available tokens for messages (context window - preamble - output reserve).
    pub fn message_budget(&self) -> usize {
        let preamble_tokens = estimate_tokens(&self.preamble);
        self.context_window
            .saturating_sub(preamble_tokens)
            .saturating_sub(self.max_output_tokens)
    }

    /// Check whether the given messages would exceed the context budget.
    pub fn would_exceed_budget(&self, messages: &[Message]) -> bool {
        let msg_tokens = estimate_message_tokens(messages);
        msg_tokens > self.message_budget()
    }

    /// Inject a system reminder into the message stream.
    /// Used for behavior-context updates without mutating the preamble.
    pub fn system_reminder(text: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: format!("<system-reminder>\n{}\n</system-reminder>", text),
            })),
        }
    }
}

impl PromptBuilder for LayeredPromptBuilder {
    async fn build(
        &self,
        messages: &[Message],
        compaction_summaries: &[String],
    ) -> Result<BuiltPrompt> {
        let mut assembled = Vec::new();

        // Layer 3: Compaction summaries (injected as system reminders).
        if !compaction_summaries.is_empty() {
            let summary_text = compaction_summaries.join("\n\n---\n\n");
            assembled.push(Self::system_reminder(&format!(
                "Previous conversation summary (compacted):\n\n{}",
                summary_text,
            )));
        }

        // Layer 4: Conversation messages.
        assembled.extend_from_slice(messages);

        let preamble_tokens = estimate_tokens(&self.preamble);
        let message_tokens = estimate_message_tokens(&assembled);

        Ok(BuiltPrompt {
            preamble: self.preamble.clone(),
            messages: assembled,
            estimated_tokens: preamble_tokens + message_tokens,
        })
    }
}

fn build_preamble(
    system_prompt: &str,
    behavior_name: &str,
    tool_names: &[&str],
    include_meta_tool_guidance: bool,
) -> String {
    let mut parts = Vec::new();
    let system_prompt = strip_title_generation_suffix(system_prompt);

    if !system_prompt.is_empty() {
        parts.push(system_prompt.to_string());
    }

    if !behavior_name.is_empty() {
        parts.push(format!("You are the {} agent.", behavior_name));
    }

    if include_meta_tool_guidance {
        parts.push(TOOL_DISCOVERY_GUIDANCE.to_string());
    }
    parts.push(direct_tool_guidance(tool_names));

    parts.join("\n\n")
}

fn strip_title_generation_suffix(system_prompt: &str) -> &str {
    let trimmed = system_prompt.trim();
    if let Some(stripped) = trimmed.strip_suffix(TITLE_GENERATION_SUFFIX) {
        stripped.trim_end()
    } else {
        trimmed
    }
}

fn direct_tool_guidance(tool_names: &[&str]) -> String {
    if tool_names.is_empty() {
        "You do not have any configured tools.".to_string()
    } else {
        format!("You have access to these tools: {}.", tool_names.join(", "))
    }
}

/// Rough token estimate: ~4 chars per token.
fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

pub(crate) fn estimate_message_tokens(messages: &[Message]) -> usize {
    let serialized = serde_json::to_string(messages).unwrap_or_default();
    estimate_tokens(&serialized)
}

#[cfg(test)]
mod tests;
