use std::time::Duration;

use gents_inference::llm::message::Message;
use gents_inference::llm::{HookAction, ToolCallHookAction};
use serde::Deserialize;
use serde_json::json;
use tracing::Instrument;

use crate::background::status::{ProcessControlScope, ReadToolOutputOutcome};
use crate::call::lifecycle::ToolCallLifecycle;
use gents_loop::tool_call_lifecycle::FailureClass;
use gents_loop::truncation::{truncate_text, TruncationMode};
use gents_protocol::tool_call::AwaitMode;
use gents_session::session::tool_call_state::CancelCause;
use gents_session::session::tool_output_rows::BackgroundToolArgs;
use gents_session::tool_call::query::result::load_tool_call_result;
use gents_tools::native::background_args::{
    CancelToolArgs, ListBackgroundToolsArgs, ReadToolOutputArgs, WaitToolArgs,
};

use super::DefraSessionHook;

use gents_protocol::job::MAX_BACKGROUNDED_TOOLS_PER_PARENT;

mod background_control;
mod background_tools;
mod foreground_handoff;
pub mod goal_tools;
pub mod helpers;
mod prompt_hook;
mod session_message;

pub(crate) use helpers::count_live_backgrounded_rows_in;
pub(crate) use helpers::BackgroundSlotRefusal;
use helpers::*;
pub(crate) use helpers::{reserve_background_slot_in_txn, BackgroundSlot};
pub use prompt_hook::HookOwnedTool;

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
