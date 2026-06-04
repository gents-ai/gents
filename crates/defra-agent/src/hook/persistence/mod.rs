use std::time::Duration;

use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::message::{Message, Text, ToolResult, ToolResultContent, UserContent};
use rig::completion::{CompletionModel, CompletionResponse};
use rig::one_or_many::OneOrMany;
use serde::Deserialize;
use serde_json::json;
use tracing::Instrument;

use crate::background_tools::r4c_args::{
    ListBackgroundToolsArgs, ListSubagentsArgs, ReadSubagentTranscriptArgs, ReadToolOutputArgs,
    SteerSubagentArgs,
};
use crate::background_tools::{
    active_session_request_id, append_steering_request, child_request_completed,
    context_allowed_target_names, drain_automated_wakeups_returning_ids,
    effective_context_cross_deployment_spawn_timeout_seconds, handle_list_background_tools,
    handle_list_subagents, handle_read_subagent_transcript, handle_read_tool_output,
    load_authorized_child_edge, load_child_final_response, load_child_terminal_row,
    load_parent_subagent_context, load_steer_subagent_target, pending_automated_wakeup_request_ids,
    project_child_terminal, resolve_context_target, try_load_authorized_child_edge,
    BackgroundToolArgs, CancelSubagentArgs, CancelToolArgs, ParentSubagentContext,
    ReadToolOutputOutcome, SpawnSubagentArgs, SteerSubagentTarget, WaitSubagentArgs, WaitToolArgs,
};
use crate::config::DEFAULT_DEADLINE_DURATION_SECS;
use crate::document_config::{load_agent_behavior, SubagentTarget};
use crate::session;
use crate::tool_call_lifecycle::query::load_tool_call_result;
use crate::tool_call_lifecycle::runtime::{classify_managed_tool_result, ManagedToolTerminal};
use crate::tool_call_lifecycle::{
    AwaitMode, CancelCause, CancelPolicy, CascadeDispatch, ChildTerminal, FailureClass,
    ToolCallLifecycle, MAX_SUBAGENT_DEPTH,
};
use crate::toolset::{
    CommandPolicyDenial, CANCEL_PROCESS_TOOL_NAME, CANCEL_SUBAGENT_TOOL_NAME,
    LIST_PROCESSES_TOOL_NAME, LIST_SUBAGENTS_TOOL_NAME, READ_PROCESS_TOOL_NAME,
    READ_SUBAGENT_TRANSCRIPT_TOOL_NAME, SPAWN_PROCESS_TOOL_NAME, SPAWN_SUBAGENT_TOOL_NAME,
    STEER_SUBAGENT_TOOL_NAME, WAIT_PROCESS_TOOL_NAME, WAIT_SUBAGENT_TOOL_NAME,
};
use crate::truncation::{truncate_text, DefraSpillTruncator, TruncationMode, Truncator};

use super::{non_empty, DefraSessionHook, TranscriptTurnState};

const MAX_BACKGROUNDED_TOOLS_PER_PARENT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubagentTargetHost {
    Local,
    Remote,
}

mod background_tools;
mod helpers;
mod message_spawn;
mod prompt_hook;
mod subagent_bridge;
mod subagent_tools;

use helpers::*;
