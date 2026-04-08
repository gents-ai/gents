//! Layered prompt construction for KV cache reuse.
//!
//! The prompt is assembled in a fixed order so vLLM's automatic prefix
//! caching can reuse KV computation across turns:
//!
//! ```text
//! ┌─────────────────────────────────────┐
//! │ Layer 1: Static system prompt       │  ← cached globally, never changes
//! │ Layer 2: Data room context          │  ← cached per-agent, set at init
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
//! Data room updates flow through `<system-reminder>` tags injected into
//! conversation messages — never by mutating the preamble.

use anyhow::Result;
use rig::completion::message::{Message, Text, UserContent};
use rig::one_or_many::OneOrMany;

use crate::config::DaemonConfig;
use crate::config::ProfileConfig;

/// Guidance appended to the preamble so the LLM knows how to discover and
/// invoke data-service tools via the meta-tool workflow.
const TOOL_DISCOVERY_GUIDANCE: &str = "\
## Tool Discovery

You have access to data service tools via three meta-tools:

1. **discover_tools** — Browse or search available data services. Call with no \
arguments to see all services, or provide a query to filter. Returns service \
names, descriptions, and tool listings.

2. **describe_tool** — Get the full input schema for a specific tool. Call this \
before using call_tool so you know the required arguments.

3. **call_tool** — Invoke a tool on a data service. Pass the service_id, \
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

/// Layered prompt builder backed by DaemonConfig.
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
}

impl LayeredPromptBuilder {
    /// Create a new prompt builder from daemon configuration.
    ///
    /// Assembles the preamble from the static system prompt and data room
    /// context. This preamble is frozen for the daemon's lifetime.
    pub fn new(config: &DaemonConfig) -> Self {
        let preamble = build_preamble(
            &config.system_prompt,
            &config.data_room,
            &["list_files", "read_file", "bash"],
        );
        Self {
            preamble,
            context_window: config.context_window,
            max_output_tokens: config.max_output_tokens,
        }
    }

    pub fn from_profile(profile: &ProfileConfig) -> Self {
        let tool_names = profile.native_tools.tool_names();
        let tool_refs = tool_names.iter().map(String::as_str).collect::<Vec<_>>();
        let preamble = build_preamble(&profile.system_prompt, &profile.name, &tool_refs);
        Self {
            preamble,
            context_window: profile.context_window,
            max_output_tokens: profile.max_output_tokens,
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
    /// Used for data room updates without mutating the preamble.
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

/// Assemble the frozen preamble from system prompt, data room context, and
/// tool discovery guidance.
fn build_preamble(system_prompt: &str, data_room: &str, tool_names: &[&str]) -> String {
    let mut parts = Vec::new();

    if !system_prompt.is_empty() {
        parts.push(system_prompt.to_string());
    }

    if !data_room.is_empty() {
        parts.push(format!("You are the {} agent.", data_room));
    }

    parts.push(TOOL_DISCOVERY_GUIDANCE.to_string());
    parts.push(direct_tool_guidance(tool_names));

    parts.join("\n\n")
}

fn direct_tool_guidance(tool_names: &[&str]) -> String {
    if tool_names.is_empty() {
        "You do not have any profile-specific native tools beyond the meta-tools and delegate_to_agent."
            .to_string()
    } else {
        format!(
            "You also have direct access to these native tools: {}.",
            tool_names.join(", ")
        )
    }
}

/// Rough token estimate: ~4 chars per token.
fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Estimate tokens for a message sequence via JSON serialization.
fn estimate_message_tokens(messages: &[Message]) -> usize {
    let serialized = serde_json::to_string(messages).unwrap_or_default();
    estimate_tokens(&serialized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::completion::message::AssistantContent;

    fn user_msg(text: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: text.to_string(),
            })),
        }
    }

    fn assistant_msg(text: &str) -> Message {
        Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: text.to_string(),
            })),
        }
    }

    #[test]
    fn preamble_combines_prompt_and_data_room() {
        let preamble = build_preamble("You are a helpful assistant.", "research", &["list_files"]);
        assert!(preamble.contains("You are a helpful assistant."));
        assert!(preamble.contains("You are the research agent."));
        assert!(preamble.contains("## Tool Discovery"));
        assert!(preamble.contains("discover_tools"));
        assert!(preamble.contains("describe_tool"));
        assert!(preamble.contains("call_tool"));
        assert!(preamble.contains("list_files"));
    }

    #[test]
    fn preamble_handles_empty_system_prompt() {
        let preamble = build_preamble("", "general", &["bash"]);
        assert!(preamble.contains("You are the general agent."));
        assert!(preamble.contains("## Tool Discovery"));
    }

    #[test]
    fn preamble_handles_empty_data_room() {
        let preamble = build_preamble("Be helpful.", "", &[]);
        assert!(preamble.contains("Be helpful."));
        assert!(preamble.contains("## Tool Discovery"));
    }

    #[test]
    fn preamble_is_frozen() {
        let config = DaemonConfig {
            system_prompt: "System prompt v1.".to_string(),
            data_room: "test".to_string(),
            ..Default::default()
        };
        let builder = LayeredPromptBuilder::new(&config);

        // Preamble should be identical across calls.
        assert_eq!(builder.preamble(), builder.preamble());
        assert!(builder.preamble().contains("System prompt v1."));
    }

    #[tokio::test]
    async fn build_without_summaries() {
        let config = DaemonConfig {
            system_prompt: "Be helpful.".to_string(),
            data_room: "general".to_string(),
            context_window: 100000,
            ..Default::default()
        };
        let builder = LayeredPromptBuilder::new(&config);

        let messages = vec![user_msg("hello"), assistant_msg("hi")];
        let prompt = builder.build(&messages, &[]).await.unwrap();

        assert_eq!(prompt.messages.len(), 2);
        assert!(prompt.estimated_tokens > 0);
        assert!(prompt.preamble.contains("Be helpful."));
    }

    #[tokio::test]
    async fn build_with_summaries_prepends() {
        let config = DaemonConfig {
            system_prompt: "Be helpful.".to_string(),
            data_room: "general".to_string(),
            context_window: 100000,
            ..Default::default()
        };
        let builder = LayeredPromptBuilder::new(&config);

        let messages = vec![user_msg("what were we discussing?")];
        let summaries = vec!["We discussed project architecture.".to_string()];
        let prompt = builder.build(&messages, &summaries).await.unwrap();

        // Summary injected as first message, conversation message second.
        assert_eq!(prompt.messages.len(), 2);

        if let Message::User { content } = &prompt.messages[0] {
            if let UserContent::Text(t) = content.first_ref() {
                assert!(t.text.contains("<system-reminder>"));
                assert!(t.text.contains("project architecture"));
            } else {
                panic!("expected text");
            }
        } else {
            panic!("expected user message");
        }
    }

    #[test]
    fn system_reminder_format() {
        let msg = LayeredPromptBuilder::system_reminder("The time is 3pm.");
        if let Message::User { content } = &msg {
            if let UserContent::Text(t) = content.first_ref() {
                assert!(t.text.starts_with("<system-reminder>"));
                assert!(t.text.ends_with("</system-reminder>"));
                assert!(t.text.contains("The time is 3pm."));
            } else {
                panic!("expected text");
            }
        } else {
            panic!("expected user message");
        }
    }

    #[test]
    fn message_budget_accounts_for_preamble_and_output() {
        let config = DaemonConfig {
            system_prompt: "x".repeat(4000), // ~1000 tokens
            context_window: 10000,
            max_output_tokens: 2000,
            ..Default::default()
        };
        let builder = LayeredPromptBuilder::new(&config);

        // Budget should be context_window - preamble_tokens - output_tokens
        let budget = builder.message_budget();
        assert!(budget < 10000);
        assert!(budget > 5000); // rough check
    }

    #[test]
    fn would_exceed_budget_short_messages() {
        let config = DaemonConfig {
            system_prompt: "Be helpful.".to_string(),
            context_window: 100000,
            max_output_tokens: 8192,
            ..Default::default()
        };
        let builder = LayeredPromptBuilder::new(&config);

        let messages = vec![user_msg("hi")];
        assert!(!builder.would_exceed_budget(&messages));
    }

    #[test]
    fn would_exceed_budget_long_messages() {
        let config = DaemonConfig {
            system_prompt: "Be helpful.".to_string(),
            context_window: 100,
            max_output_tokens: 50,
            ..Default::default()
        };
        let builder = LayeredPromptBuilder::new(&config);

        let big = user_msg(&"x".repeat(10000));
        assert!(builder.would_exceed_budget(&[big]));
    }
}
