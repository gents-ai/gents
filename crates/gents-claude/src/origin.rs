//! The Claude Code wire mode of a Gents request: the identity prefix,
//! entrypoint, `x-app`, workload, subagent marking, request class and cache
//! source of the Claude Code query the request corresponds to (`f6n`, `rF`,
//! `J6o`, `rci`, `Tbe`). Lean: `PromptAssembly.ClaudeOrigin`, whose exhaustive
//! cases bind [`Origin::wire_mode`].

use serde::{Deserialize, Serialize};

use crate::headers::RequestClass;
use crate::system::SessionMode;

/// Entrypoint of an interactive CLI session.
pub const INTERACTIVE_ENTRYPOINT: &str = "cli";
/// Entrypoint of a non-interactive `claude -p` session outside the Agent SDK.
pub const PRINT_ENTRYPOINT: &str = "sdk-cli";
/// Workload tag of scheduled work.
pub const CRON_WORKLOAD: &str = "cron";

/// How the request's session was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// A client opened the session; it has no creation receipt.
    Client,
    /// Another agent opened it with `agent_new`.
    AgentNew,
    /// A task trigger opened it.
    TaskTrigger,
    /// A plugin job opened it.
    PluginJob,
}

impl Cause {
    /// The session runs as a subagent of the session that opened it.
    pub const fn subagent(self) -> bool {
        matches!(self, Self::AgentNew | Self::PluginJob)
    }
}

/// What the provider call is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A turn of the agent loop.
    Turn,
    /// Conversation compaction.
    Compaction,
    /// Session title generation.
    Title,
    /// Any other single helper call.
    OneShot,
}

/// Where a request came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub cause: Cause,
    /// The first session of the creation chain was opened by a client.
    pub root_client: bool,
    /// The request's execution origin is scheduled.
    pub scheduled: bool,
    pub kind: Kind,
    /// The agent supplies its own system prompt.
    pub appends_system_prompt: bool,
}

/// The Claude Code query mode a request is sent in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireMode {
    pub session: SessionMode,
    pub entrypoint: &'static str,
    /// `x-app: cli-bg`.
    pub background: bool,
    pub workload: Option<&'static str>,
    /// `cc_is_subagent=true` and the agent headers.
    pub subagent: bool,
    pub request_class: RequestClass,
    /// The query source is on the one-hour cache allowlist.
    pub main_thread_source: bool,
    /// `isAgenticQuery`: gates the agentic betas and `diagnostics`.
    pub agentic: bool,
}

impl Origin {
    /// Agent-opened work inherits the interactivity of the chain's root.
    pub const fn interactive(self) -> bool {
        match self.cause {
            Cause::Client => true,
            Cause::AgentNew | Cause::PluginJob => self.root_client,
            Cause::TaskTrigger => false,
        }
    }

    pub const fn subagent(self) -> bool {
        self.cause.subagent()
    }

    /// `cc_turn_origin` of the request's prompt: only a top-level turn is stamped
    /// (a subagent's first prompt carries none), `scheduled` work first.
    pub const fn turn_origin(self) -> Option<&'static str> {
        if !matches!(self.kind, Kind::Turn) || self.subagent() {
            return None;
        }
        Some(if self.scheduled {
            "scheduled"
        } else if self.interactive() {
            "human"
        } else {
            "sdk"
        })
    }

    /// `(cc_prompt_index, cc_turn_index)` of a turn whose history holds `prompts`
    /// prompt rows; `None` when the turn is unstamped or a compaction summary
    /// lost the position. Earlier prompts are taken to share this turn's origin.
    pub fn turn_position(self, prompts: u64, position_lost: bool) -> Option<(u64, u64)> {
        let origin = self.turn_origin()?;
        (!position_lost).then(|| (if origin == "human" { prompts } else { 0 }, prompts.max(1)))
    }

    pub const fn wire_mode(self) -> WireMode {
        let interactive = self.interactive();
        let turn = matches!(self.kind, Kind::Turn);
        WireMode {
            session: if interactive {
                SessionMode::Interactive
            } else {
                SessionMode::NonInteractive {
                    appends_system_prompt: self.appends_system_prompt,
                }
            },
            entrypoint: if interactive {
                INTERACTIVE_ENTRYPOINT
            } else {
                PRINT_ENTRYPOINT
            },
            background: matches!(self.cause, Cause::TaskTrigger),
            workload: if self.scheduled {
                Some(CRON_WORKLOAD)
            } else {
                None
            },
            subagent: self.subagent(),
            request_class: match (self.kind, self.cause) {
                (Kind::Compaction, _) => RequestClass::Compaction,
                (Kind::Title | Kind::OneShot, _) => RequestClass::Auxiliary,
                (Kind::Turn, Cause::AgentNew) => RequestClass::Subagent,
                (Kind::Turn, Cause::PluginJob) => RequestClass::Workflow,
                (Kind::Turn, Cause::Client | Cause::TaskTrigger) => RequestClass::Main,
            },
            main_thread_source: turn && !self.subagent(),
            agentic: turn,
        }
    }
}

#[cfg(test)]
#[path = "origin_tests.rs"]
mod tests;
