pub mod account_usage;
pub mod backend_provider;
pub mod claude_messages_body;
pub mod compaction;
pub mod completion_retry;
pub mod error;
pub mod execution_origin;
pub mod execution_policy;
pub mod live_output;
pub mod loop_stream;
pub mod openai_wire;
pub mod output_obligation;
pub mod parallel_tools;
pub mod prompt;
pub mod provider_activity;
pub mod provider_audit;
pub mod provider_input;
pub mod provider_limit;
pub mod provider_patches;
pub mod provider_stream;
pub mod provider_usage;
pub mod rendered_request;
pub mod request_lifecycle;
pub mod responses_normalize;
pub mod rig_compat;
pub mod session_hook;
pub mod stream_processor;
pub mod stream_writer;
pub mod tool;
pub mod tool_call_lifecycle;
pub mod tool_effects;
pub mod tool_policy;
pub mod truncation;

pub use session_hook::SessionHook;

pub use gents_protocol::message;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Specific {
        function_names: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAction {
    Continue,
    Terminate { reason: String },
}

impl HookAction {
    pub fn cont() -> Self {
        Self::Continue
    }

    pub fn terminate(reason: impl Into<String>) -> Self {
        Self::Terminate {
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallHookAction {
    Continue,
    Skip { reason: String },
    Terminate { reason: String },
}

impl ToolCallHookAction {
    pub fn cont() -> Self {
        Self::Continue
    }

    pub fn skip(reason: impl Into<String>) -> Self {
        Self::Skip {
            reason: reason.into(),
        }
    }

    pub fn terminate(reason: impl Into<String>) -> Self {
        Self::Terminate {
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) const CORRUPT_TOOL_ARGS_589: &str = "{\"raw_schema\": false, \
         \"service_id\": \"observability-mcp\", \"tool房\n</think\": \"\n<tool_call>\n\
         <function=describe_tool>\", \"raw_schema\": false, \
         \"service_id\": \"observability-mcp\", \"tool_name\": \"list_hosts\"}";
}

#[cfg(test)]
mod end_to_end_test;
