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

use crate::llm::message::{Message, Text, UserContent};
use anyhow::Result;

use crate::config::AgentBehavior;
use crate::tool_surface::ToolSurface;

const TITLE_GENERATION_SUFFIX: &str =
    "Generate concise conversation titles. Return only a lowercase hyphenated 3-5 word title. Never call tools. Never explain.";

pub(crate) fn continuation_checkpoint_reminder(checkpoints: &str) -> String {
    format!(
        "Continuation checkpoints from earlier conversation (oldest to newest):\n\n{checkpoints}\n\n\
Continue from these checkpoints and the retained conversation. Treat recorded results as \
evidence, not as a prohibition on verification. Re-check facts when state may have changed, \
the checkpoint is ambiguous, or correctness depends on them. Avoid repeating completed or \
expensive work without a concrete reason."
    )
}

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

#[derive(Debug, Clone)]
pub struct BuiltPrompt {
    pub preamble: String,
    pub messages: Vec<Message>,
}

pub trait PromptBuilder: Send + Sync {
    fn build(
        &self,
        messages: &[Message],
        compaction_summaries: &[String],
    ) -> impl std::future::Future<Output = Result<BuiltPrompt>> + Send;
}

pub struct LayeredPromptBuilder {
    preamble: String,
    skills: Vec<crate::skills::Skill>,
    skill_ceiling: crate::skills::SkillToolCeiling,
}

impl LayeredPromptBuilder {
    pub fn new(
        behavior: &AgentBehavior,
        tool_surface: &ToolSurface,
        allowed_targets: &[(String, String)],
    ) -> Self {
        let tool_names = tool_surface.tool_names();
        let tool_refs = tool_names.iter().map(String::as_str).collect::<Vec<_>>();
        let mut builder = Self::for_behavior(
            &behavior.system_prompt,
            &behavior.behavior_id,
            &tool_refs,
            tool_surface.includes_meta_tools(),
            allowed_targets,
        );
        if let Some(catalog) = crate::skills::render_skill_catalog(&behavior.skills) {
            builder.preamble.push_str("\n\n");
            builder.preamble.push_str(&catalog);
        }
        builder.skills = behavior.skills.clone();
        builder.skill_ceiling = crate::skills::skill_tool_ceiling(
            tool_names.iter().cloned(),
            tool_surface.allowed_mcp_service_ids(),
            tool_surface.includes_meta_tools(),
        );
        builder
    }

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
        allowed_targets: &[(String, String)],
    ) -> Self {
        let preamble = build_preamble_with_targets(
            system_prompt,
            behavior_name,
            tool_names,
            include_meta_tool_guidance,
            allowed_targets,
        );
        Self {
            preamble,
            skills: Vec::new(),
            skill_ceiling: crate::skills::SkillToolCeiling::default(),
        }
    }

    pub fn preamble(&self) -> &str {
        &self.preamble
    }

    pub fn system_reminder(text: &str) -> Message {
        Message::User {
            content: vec![UserContent::Text(Text {
                text: format!("<system-reminder>\n{}\n</system-reminder>", text),
            })],
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

        if let Some(summary_message) = compaction_summary_message(compaction_summaries) {
            assembled.push(summary_message);
        }

        assembled.extend_from_slice(messages);

        Ok(BuiltPrompt {
            preamble: self.preamble.clone(),
            messages: assembled,
        })
    }
}

pub fn join_compaction_summaries(compaction_summaries: &[String]) -> String {
    compaction_summaries.join("\n\n---\n\n")
}

/// Render durable compaction summaries exactly as they appear in provider input.
pub fn compaction_summary_message(compaction_summaries: &[String]) -> Option<Message> {
    if compaction_summaries.is_empty() {
        return None;
    }
    let summary_text = join_compaction_summaries(compaction_summaries);
    Some(LayeredPromptBuilder::system_reminder(
        &continuation_checkpoint_reminder(&summary_text),
    ))
}

pub(crate) fn build_preamble_with_targets(
    system_prompt: &str,
    behavior_name: &str,
    tool_names: &[&str],
    include_meta_tool_guidance: bool,
    allowed_targets: &[(String, String)],
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

    if !allowed_targets.is_empty() {
        let mut lines = Vec::with_capacity(allowed_targets.len() + 3);
        lines.push("## Spawnable Sub-Agents".to_string());
        lines.push(
            "You may spawn the following sub-agents by passing one of these names as the \
             `spawn_subagent` tool's `name` argument:"
                .to_string(),
        );
        for (name, description) in allowed_targets {
            lines.push(format!("- {name}: {description}"));
        }
        parts.push(lines.join("\n"));
    }

    parts.join("\n\n")
}

/// Thin wrapper that builds a preamble with no subagent targets.
/// Kept for existing tests that exercise preamble construction without targets.
#[cfg(test)]
fn build_preamble(
    system_prompt: &str,
    behavior_name: &str,
    tool_names: &[&str],
    include_meta_tool_guidance: bool,
) -> String {
    build_preamble_with_targets(
        system_prompt,
        behavior_name,
        tool_names,
        include_meta_tool_guidance,
        &[],
    )
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

#[cfg(test)]
mod tests;
