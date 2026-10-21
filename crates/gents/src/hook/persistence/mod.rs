use std::time::Duration;

use crate::llm::message::Message;
use crate::llm::{HookAction, ToolCallHookAction};
use serde::Deserialize;
use serde_json::json;
use tracing::Instrument;

use crate::background_tools::r4c_args::{ListBackgroundToolsArgs, ReadToolOutputArgs};
use crate::background_tools::{
    handle_list_background_tools, handle_read_tool_output, BackgroundToolArgs, CancelToolArgs,
    ProcessControlScope, ReadToolOutputOutcome, WaitToolArgs,
};
use crate::document_config::load_agent;
use crate::tool_call_lifecycle::query::load_tool_call_result;
use crate::tool_call_lifecycle::{AwaitMode, CancelCause, FailureClass, ToolCallLifecycle};
use crate::toolset::{
    AGENT_NEW_TOOL_NAME, CANCEL_PROCESS_TOOL_NAME, LIST_PROCESSES_TOOL_NAME,
    READ_PROCESS_TOOL_NAME, SPAWN_PROCESS_TOOL_NAME, WAIT_PROCESS_TOOL_NAME,
};
use crate::truncation::{truncate_text, TruncationMode};

use super::DefraSessionHook;

pub(crate) const MAX_BACKGROUNDED_TOOLS_PER_PARENT: usize = 8;

mod background_control;
mod background_tools;
mod foreground_handoff;
mod goal_tools;
mod helpers;
mod prompt_hook;
mod session_message;

pub(crate) use helpers::count_live_backgrounded_rows_in;
pub(crate) use helpers::BackgroundCapacityExceeded;
use helpers::*;
pub(crate) use prompt_hook::HookOwnedTool;

impl DefraSessionHook {
    pub(super) fn skip_tool_result(
        &self,
        tool_name: &str,
        result: impl Into<String>,
    ) -> ToolCallHookAction {
        let result = result.into();
        ToolCallHookAction::skip(bounded_tool_result_for_model(
            tool_name,
            &result,
            &self.truncation_limits,
        ))
    }
}
