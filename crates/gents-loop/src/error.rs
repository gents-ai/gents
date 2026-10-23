use thiserror::Error;

use crate::provider_limit::{classify_provider_limit, strip_provider_limit_marker, ProviderLimit};

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Watcher(#[from] WatcherError),

    #[error(transparent)]
    Inference(#[from] InferenceError),

    #[error(transparent)]
    Stream(#[from] StreamError),

    #[error(transparent)]
    Hook(#[from] HookError),

    #[error("compaction failed for session {session_id}: {reason}")]
    Compaction { session_id: String, reason: String },

    #[error("session error: {0}")]
    Session(String),

    #[error("shutdown requested")]
    Shutdown,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required configuration: {key}")]
    Missing { key: String },

    #[error("invalid value for {key}: {reason}")]
    Invalid { key: String, reason: String },

    #[error("config file I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("config parse: {0}")]
    Parse(#[from] toml::de::Error),
}

#[derive(Debug, Error)]
pub enum WatcherError {
    #[error("event bus closed")]
    EventBusClosed,

    #[error("query failed: {reason}")]
    QueryFailed { reason: String },

    #[error("claim failed for {doc_id}: {reason}")]
    ClaimFailed { doc_id: String, reason: String },
}

#[derive(Debug, Error)]
pub enum InferenceError {
    #[error("model unreachable at {endpoint}")]
    ModelUnreachable { endpoint: String },

    #[error("transient inference failure: {reason}")]
    TransientFailure { reason: String },

    #[error("permanent inference failure: {reason}")]
    PermanentFailure { reason: String },

    #[error("inference timed out after {timeout:?}")]
    Timeout { timeout: std::time::Duration },

    #[error(
        "rate limited{}",
        retry_after.map(|wait| format!(", retry after {wait:?}")).unwrap_or_default()
    )]
    RateLimited {
        retry_after: Option<std::time::Duration>,
    },

    #[error("{0}")]
    UsageLimited(crate::provider_limit::UsageLimit),

    #[error("context length exceeded: {reason}")]
    ContextLengthExceeded { reason: String },

    #[error("retries exhausted ({max_retries} attempts): {last_error}")]
    RetriesExhausted {
        max_retries: u32,
        last_error: String,
    },
}

impl InferenceError {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            InferenceError::ModelUnreachable { .. }
                | InferenceError::TransientFailure { .. }
                | InferenceError::Timeout { .. }
                | InferenceError::RateLimited { .. }
        )
    }
}

#[derive(Debug, Error)]
pub enum StreamError {
    #[error("stream write failed for doc {doc_id}: {reason}")]
    WriteFailed { doc_id: String, reason: String },

    #[error("no active buffer for doc {doc_id}")]
    NoBuffer { doc_id: String },

    #[error("stream finalize failed: {reason}")]
    FinalizeFailed { reason: String },
}

#[derive(Debug, Error)]
pub enum HookError {
    #[error("persistence failed: {reason}")]
    PersistenceFailed { reason: String },

    #[error("session not initialized")]
    SessionNotInitialized,
}

const PERMANENT_HTTP_STATUSES: &[u16] = &[400, 401, 403, 404, 422, 426];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopFailureCause {
    Completion {
        failure: CompletionFailure,
        reason: String,
    },
    MaxTurns,
    Prompt,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionFailure {
    Http,
    Provider(String),
    Request,
    Response,
}

#[derive(Debug)]
pub struct LoopStreamError {
    cause: LoopFailureCause,
    error: Box<dyn std::error::Error + Send + Sync>,
}

impl LoopStreamError {
    pub fn new(
        cause: LoopFailureCause,
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            cause,
            error: Box::new(error),
        }
    }

    pub fn cause(&self) -> &LoopFailureCause {
        &self.cause
    }

    pub fn provider_message(&self) -> Option<&str> {
        match &self.cause {
            LoopFailureCause::Completion {
                failure: CompletionFailure::Provider(message),
                ..
            } => Some(message),
            _ => None,
        }
    }

    pub fn classify(&self) -> InferenceError {
        classify_loop_failure(&self.cause, &self.error.to_string())
    }
}

impl std::fmt::Display for LoopStreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for LoopStreamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

pub fn classify_loop_failure(cause: &LoopFailureCause, message: &str) -> InferenceError {
    let msg = strip_provider_limit_marker(message).to_string();

    if msg.contains("context_length_exceeded") || msg.contains("maximum context length") {
        return InferenceError::ContextLengthExceeded { reason: msg };
    }

    match cause {
        LoopFailureCause::Completion { failure, reason } => {
            if matches!(
                failure,
                CompletionFailure::Http | CompletionFailure::Provider(_)
            ) {
                match classify_provider_limit(reason, chrono::Utc::now()) {
                    Some(ProviderLimit::UsageExhausted(limit)) => {
                        return InferenceError::UsageLimited(limit);
                    }
                    Some(ProviderLimit::Throttled { retry_after }) => {
                        return InferenceError::RateLimited { retry_after };
                    }
                    None => {}
                }
            }
            let reason = strip_provider_limit_marker(reason).to_string();
            match failure {
                CompletionFailure::Http => {
                    if message_has_any_status(&reason, PERMANENT_HTTP_STATUSES) {
                        InferenceError::PermanentFailure { reason }
                    } else {
                        InferenceError::TransientFailure { reason }
                    }
                }
                CompletionFailure::Provider(provider_msg) => {
                    let provider_msg_lower = provider_msg.to_ascii_lowercase();
                    if provider_message_is_tool_call_json_parse_failure(provider_msg) {
                        InferenceError::TransientFailure { reason }
                    } else if message_has_any_status(provider_msg, PERMANENT_HTTP_STATUSES)
                        || provider_msg_lower.contains("invalid_api_key")
                        || provider_msg_lower.contains("invalid api key")
                        || provider_msg_lower.contains("authentication")
                        || provider_msg_lower.contains("unauthorized")
                        || provider_msg_lower.contains("invalid_request")
                        || provider_msg_lower.contains("invalid request")
                        || provider_message_is_model_refusal(provider_msg)
                    {
                        InferenceError::PermanentFailure { reason }
                    } else {
                        InferenceError::TransientFailure { reason }
                    }
                }
                CompletionFailure::Request => InferenceError::PermanentFailure { reason },
                CompletionFailure::Response => InferenceError::TransientFailure { reason },
            }
        }
        LoopFailureCause::MaxTurns | LoopFailureCause::Prompt | LoopFailureCause::Tool => {
            InferenceError::PermanentFailure { reason: msg }
        }
    }
}

fn error_message_has_status(message: &str, status: u16) -> bool {
    message.contains(&format!("InvalidStatusCodeWithMessage({status}"))
        || message.contains(&format!("Invalid status code {status}"))
        || message.contains(&format!("Invalid status code: {status}"))
        || message.contains(&format!("status code {status}"))
        || message.contains(&format!("HTTP status {status}"))
}

fn message_has_any_status(message: &str, statuses: &[u16]) -> bool {
    statuses
        .iter()
        .any(|status| error_message_has_status(message, *status))
}

pub(crate) fn provider_message_is_tool_call_json_parse_failure(message: &str) -> bool {
    message.contains("BadRequestError")
        && message.contains(" line ")
        && message.contains(" column ")
        && message.contains("(char ")
}

pub(crate) fn provider_message_is_fail_closed_tool_use(message: &str) -> bool {
    message.contains("fail-closed: ") && message.contains("tool_use")
}

/// The model declined the request. The Claude Messages parser raises
/// `MessagesParseError::ProviderRefusal` when a turn closes with
/// `stop_reason: refusal` and no usable content. Resending the identical body
/// earns the identical refusal, so this is permanent and never resampled.
pub(crate) fn provider_message_is_model_refusal(message: &str) -> bool {
    message.contains("stop_reason: refusal")
}

pub(crate) fn provider_message_is_reasoning_rejection(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("expected `thinking`")
        || lower.contains("must start with a thinking block")
        || (lower.contains("signature") && lower.contains("thinking") && lower.contains("invalid"))
        || (lower.contains("redacted_thinking") && lower.contains("invalid"))
        || lower.contains("invalid_encrypted_content")
        || (lower.contains("encrypted")
            && (lower.contains("could not be decrypted")
                || lower.contains("could not be verified")))
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
