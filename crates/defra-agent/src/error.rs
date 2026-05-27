//! Domain-specific error types for the agent daemon.
//!
//! Replaces bare `anyhow::Result` on public boundaries with typed errors
//! so callers can distinguish retryable failures from permanent ones.

use thiserror::Error;

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

/// Configuration errors — detected at startup.
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

/// Inference / LLM errors — includes retry classification.
#[derive(Debug, Error)]
pub enum InferenceError {
    #[error("model unreachable at {endpoint}")]
    ModelUnreachable { endpoint: String },

    #[error("transient inference failure: {reason}")]
    TransientFailure { reason: String },

    #[error("permanent inference failure: {reason}")]
    PermanentFailure { reason: String },

    #[error("inference timed out after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },

    #[error("rate limited, retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },

    #[error("context length exceeded: {reason}")]
    ContextLengthExceeded { reason: String },

    #[error("retries exhausted ({max_retries} attempts): {last_error}")]
    RetriesExhausted {
        max_retries: u32,
        last_error: String,
    },
}

impl InferenceError {
    /// Whether this error is worth retrying with backoff.
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

pub fn classify_completion_error(error: &rig::agent::StreamingError) -> InferenceError {
    let msg = error.to_string();

    if msg.contains("context_length_exceeded") || msg.contains("maximum context length") {
        return InferenceError::ContextLengthExceeded { reason: msg };
    }

    match error {
        rig::agent::StreamingError::Completion(completion_err) => {
            let reason = completion_err.to_string();
            match completion_err {
                // HTTP errors (connection reset, DNS, timeout) are transient.
                rig::completion::CompletionError::HttpError(_) => {
                    if error_message_has_status(&reason, 429) {
                        InferenceError::RateLimited {
                            retry_after_secs: 60,
                        }
                    } else if error_message_has_status(&reason, 400)
                        || error_message_has_status(&reason, 401)
                        || error_message_has_status(&reason, 403)
                        || error_message_has_status(&reason, 404)
                        || error_message_has_status(&reason, 422)
                    {
                        InferenceError::PermanentFailure { reason }
                    } else {
                        InferenceError::TransientFailure { reason }
                    }
                }
                rig::completion::CompletionError::ProviderError(provider_msg) => {
                    if provider_msg.contains("rate_limit") || provider_msg.contains("429") {
                        InferenceError::RateLimited {
                            retry_after_secs: 60,
                        }
                    } else if error_message_has_status(provider_msg, 404) {
                        InferenceError::PermanentFailure { reason }
                    } else if provider_msg.contains("401")
                        || provider_msg.contains("invalid_api_key")
                        || provider_msg.contains("authentication")
                        || provider_msg.contains("unauthorized")
                    {
                        // Auth errors are permanent — retrying won't help.
                        InferenceError::PermanentFailure { reason }
                    } else if provider_msg.contains("400")
                        || provider_msg.contains("invalid_request")
                    {
                        // Bad request errors are permanent.
                        InferenceError::PermanentFailure { reason }
                    } else if provider_msg.contains("500")
                        || provider_msg.contains("502")
                        || provider_msg.contains("503")
                        || provider_msg.contains("overloaded")
                    {
                        // Server errors are transient.
                        InferenceError::TransientFailure { reason }
                    } else {
                        // Unknown provider errors — assume transient to be safe.
                        InferenceError::TransientFailure { reason }
                    }
                }
                // JSON/URL parse errors are permanent (bad request shape).
                rig::completion::CompletionError::JsonError(_)
                | rig::completion::CompletionError::UrlError(_) => {
                    InferenceError::PermanentFailure { reason }
                }
                _ => InferenceError::TransientFailure { reason },
            }
        }
        // Tool and prompt errors are permanent (not retryable).
        rig::agent::StreamingError::Tool(_) => InferenceError::PermanentFailure { reason: msg },
        rig::agent::StreamingError::Prompt(_) => InferenceError::PermanentFailure { reason: msg },
    }
}

fn error_message_has_status(message: &str, status: u16) -> bool {
    message.contains(&format!("InvalidStatusCodeWithMessage({status}"))
        || message.contains(&format!("Invalid status code {status}"))
        || message.contains(&format!("Invalid status code: {status}"))
        || message.contains(&format!("status code {status}"))
        || message.contains(&format!("HTTP status {status}"))
}

#[cfg(test)]
mod tests;
