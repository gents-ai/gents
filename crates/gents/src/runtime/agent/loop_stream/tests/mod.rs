use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::inference::llm::message::{AssistantContent, ToolResultContent, UserContent};
use crate::inference::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use futures::{stream, Stream, StreamExt};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};

use crate::inference::llm::message::Message;
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall, StreamingCompletionResponse};
use tokio::sync::Mutex;

use super::*;
use crate::ensure_runtime_schemas;
use crate::test_support::first_content;
use crate::tools::call::hook::{DefraSessionHook, FailurePolicy};

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
