//! The system prompt blocks: attribution, identity prefix and the joined
//! prompt (Claude Code `Qzs`, `D0o`, `f6n`). Lean: `PromptAssembly.ClaudeMap`.

use crate::wire::{CacheControl, SystemBlock};

/// The interactive CLI identity prefix.
pub const CLI_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
/// The Agent SDK identity prefix (a non-interactive session that appends a prompt).
pub const SDK_IDENTITY: &str =
    "You are Claude Code, Anthropic's official CLI for Claude, running within the Claude Agent SDK.";
/// The bare agent identity prefix (a non-interactive session with no appended prompt).
pub const AGENT_IDENTITY: &str = "You are a Claude agent, built on Anthropic's Claude Agent SDK.";

const IDENTITIES: [&str; 3] = [CLI_IDENTITY, SDK_IDENTITY, AGENT_IDENTITY];
const ATTRIBUTION_PREFIX: &str = "x-anthropic-billing-header:";

/// How the session runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    /// An interactive session.
    Interactive,
    /// A non-interactive session.
    NonInteractive {
        /// Whether the session appends a system prompt.
        appends_system_prompt: bool,
    },
}

/// Picks the identity prefix (Claude Code `f6n`): Vertex always takes the CLI identity.
pub fn identity_for(vertex: bool, mode: SessionMode) -> &'static str {
    match (vertex, mode) {
        (true, _) | (_, SessionMode::Interactive) => CLI_IDENTITY,
        (
            false,
            SessionMode::NonInteractive {
                appends_system_prompt: true,
            },
        ) => SDK_IDENTITY,
        (
            false,
            SessionMode::NonInteractive {
                appends_system_prompt: false,
            },
        ) => AGENT_IDENTITY,
    }
}

/// Splits `[attribution, identity, ...prompt]` into system blocks (Claude Code `Qzs`, `D0o`).
///
/// Empty strings are dropped, a later attribution or identity replaces an earlier
/// one, and every other string joins one block with a blank line. Output order:
/// attribution (never marked), identity, joined prompt; the last two carry `cache`,
/// which is `None` when prompt caching is off.
pub fn system_blocks<'a, I>(strings: I, cache: Option<CacheControl>) -> Vec<SystemBlock>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut attribution = None;
    let mut identity = None;
    let mut prompt: Vec<&str> = Vec::new();
    for text in strings {
        if text.is_empty() {
            continue;
        }
        if text.starts_with(ATTRIBUTION_PREFIX) {
            attribution = Some(text);
        } else if IDENTITIES.contains(&text) {
            identity = Some(text);
        } else {
            prompt.push(text);
        }
    }
    let mut blocks = Vec::with_capacity(3);
    if let Some(text) = attribution {
        blocks.push(SystemBlock {
            text: text.to_owned(),
            cache_control: None,
        });
    }
    if let Some(text) = identity {
        blocks.push(SystemBlock {
            text: text.to_owned(),
            cache_control: cache.clone(),
        });
    }
    if !prompt.is_empty() {
        blocks.push(SystemBlock {
            text: prompt.join("\n\n"),
            cache_control: cache,
        });
    }
    blocks
}

#[cfg(test)]
#[path = "system_tests.rs"]
mod tests;
