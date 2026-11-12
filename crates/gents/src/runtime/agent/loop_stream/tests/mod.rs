use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::{stream, Stream, StreamExt};
use gents_inference::llm::message::{AssistantContent, ToolResultContent, UserContent};
use gents_loop::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};

use gents_inference::llm::message::Message;
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall, StreamingCompletionResponse};
use tokio::sync::Mutex;

use super::*;
use gents_store::test_support::first_content;
use gents_toolcalls::call::hook::{DefraSessionHook, FailurePolicy};

mod support;
use support::*;

include!("budgeting.rs");
include!("capture.rs");
include!("claude.rs");
include!("responses_replay.rs");
include!("one_shot.rs");
include!("request_assembly.rs");
include!("retry.rs");
include!("streaming.rs");
include!("tool_execution.rs");

include!("invalid_tool_progress.rs");
include!("repeated_tool_failure.rs");
include!("schema_argument_repair.rs");
