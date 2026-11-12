use gents_automation::test_support::loop_stream::capture::*;
use gents_automation::test_support::loop_stream::invalid_tool_progress::*;
use gents_automation::test_support::loop_stream::repeated_tool_failure::*;
use gents_automation::test_support::loop_stream::request_assembly::*;
use gents_automation::test_support::loop_stream::schema_argument_repair::*;
use gents_automation::test_support::loop_stream::tool_execution::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gents_inference::llm::message::{AssistantContent, ToolResultContent, UserContent};
use gents_loop::tool::ToolDyn;
use rig::completion::{CompletionError, CompletionRequest};

use gents_inference::llm::message::Message;
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall};
use tokio::sync::Mutex;

use super::*;
use gents_toolcalls::call::hook::FailurePolicy;

use gents_automation::test_support::loop_stream::*;

include!("budgeting.rs");
include!("capture.rs");
include!("claude.rs");
include!("responses_replay.rs");
include!("request_assembly.rs");
include!("retry.rs");
include!("streaming.rs");
include!("tool_execution.rs");

include!("invalid_tool_progress.rs");
include!("repeated_tool_failure.rs");
include!("schema_argument_repair.rs");
