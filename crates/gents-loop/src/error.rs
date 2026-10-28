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

#[derive(Clone, Debug, Error)]
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
    /// A transport failure; `status` is the HTTP status when Rig's
    /// `http_client::Error` carries one.
    Http {
        status: Option<u16>,
    },
    Provider(String),
    Request,
    Response,
    /// The owned loop ended the completion itself; `message` is the provider
    /// error text the loop renders for it.
    Loop {
        verdict: LoopVerdict,
        message: String,
    },
}

/// Why the owned loop ended a completion on its own authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopVerdict {
    /// Retries ran out on turns that produced no text and no tool call.
    NoVisibleOutput,
    /// Retries ran out on turns whose output broke the structured contract.
    StructuredOutputInvalid,
    /// The request-wide aggregate token budget is spent.
    AggregateTokenBudgetExhausted,
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

    /// The owned loop's own verdict and the message it rendered, when the loop
    /// rather than the provider ended the completion.
    pub fn loop_verdict(&self) -> Option<(LoopVerdict, &str)> {
        match &self.cause {
            LoopFailureCause::Completion {
                failure: CompletionFailure::Loop { verdict, message },
                ..
            } => Some((*verdict, message)),
            _ => None,
        }
    }

    pub fn classify(&self) -> InferenceError {
        classify_loop_failure(&self.cause, &self.error.to_string()).error
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

/// A loop failure classified for the retry policy: the inference error it
/// maps to, what the provider's text reported, and that text.
#[derive(Debug)]
pub struct ClassifiedFailure {
    pub error: InferenceError,
    pub report: ProviderReport,
    pub text: String,
}

pub fn classify_loop_failure(cause: &LoopFailureCause, message: &str) -> ClassifiedFailure {
    let text = strip_provider_limit_marker(message).to_string();
    let report = match cause {
        LoopFailureCause::Completion {
            failure: CompletionFailure::Loop { .. },
            ..
        } => ProviderReport::default(),
        _ => ProviderReport::classify_failure_text(&text),
    };
    let error = if report.context_length_exceeded {
        InferenceError::ContextLengthExceeded {
            reason: text.clone(),
        }
    } else {
        classify_cause(cause, &report, &text)
    };
    ClassifiedFailure {
        error,
        report,
        text,
    }
}

fn classify_cause(cause: &LoopFailureCause, report: &ProviderReport, text: &str) -> InferenceError {
    let LoopFailureCause::Completion { failure, reason } = cause else {
        return InferenceError::PermanentFailure {
            reason: text.to_string(),
        };
    };
    if matches!(
        failure,
        CompletionFailure::Http { .. } | CompletionFailure::Provider(_)
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
    let permanent = match failure {
        CompletionFailure::Http { status } => status.map_or(report.permanent_status, |status| {
            PERMANENT_HTTP_STATUSES.contains(&status)
        }),
        CompletionFailure::Provider(_) => {
            !report.malformed_tool_call
                && (report.permanent_status
                    || report.rejected_credentials_or_request
                    || report.refusal)
        }
        CompletionFailure::Request => true,
        CompletionFailure::Response | CompletionFailure::Loop { .. } => false,
    };
    if permanent {
        InferenceError::PermanentFailure { reason }
    } else {
        InferenceError::TransientFailure { reason }
    }
}

/// What a provider failure's rendered text reports.
///
/// External premise: Rig's `CompletionModel` carries provider failures as
/// `CompletionError::ProviderError(String)` and transport failures as
/// `http_client::Error`, so a provider's verdict reaches the loop only as text.
/// The transports render these facts into that text: HTTP statuses in Rig's
/// `InvalidStatusCode*` display or a proxy body, vLLM/LiteLLM tool-call JSON
/// parse 400s as `BadRequestError` with a `line/column/(char` position, the
/// Claude Messages parser's `MessagesParseError` fail-closed `tool_use` causes
/// and `ProviderRefusal` (`stop_reason: refusal`), and the Anthropic/OpenAI
/// rejections of replayed thinking or encrypted reasoning. This is the one
/// reader of that text; the loop decides on the report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProviderReport {
    pub context_length_exceeded: bool,
    /// A 400 for a tool call whose JSON the provider could not parse.
    pub malformed_tool_call: bool,
    /// The Claude Messages parser refused an unsafe `tool_use` stream.
    pub fail_closed_tool_use: bool,
    /// The model declined; resending the identical body earns the identical
    /// refusal, so it is permanent and never resampled.
    pub refusal: bool,
    pub reasoning_rejected: bool,
    pub permanent_status: bool,
    pub rejected_credentials_or_request: bool,
}

impl ProviderReport {
    pub fn classify_failure_text(text: &str) -> Self {
        let lower = text.to_ascii_lowercase();
        let has_status = |status: u16| {
            [
                format!("InvalidStatusCodeWithMessage({status}"),
                format!("Invalid status code {status}"),
                format!("Invalid status code: {status}"),
                format!("status code {status}"),
                format!("HTTP status {status}"),
            ]
            .iter()
            .any(|pattern| text.contains(pattern.as_str()))
        };
        Self {
            context_length_exceeded: text.contains("context_length_exceeded")
                || text.contains("maximum context length"),
            malformed_tool_call: text.contains("BadRequestError")
                && text.contains(" line ")
                && text.contains(" column ")
                && text.contains("(char "),
            fail_closed_tool_use: text.contains("fail-closed: ") && text.contains("tool_use"),
            refusal: text.contains("stop_reason: refusal"),
            reasoning_rejected: lower.contains("expected `thinking`")
                || lower.contains("must start with a thinking block")
                || (lower.contains("signature")
                    && lower.contains("thinking")
                    && lower.contains("invalid"))
                || (lower.contains("redacted_thinking") && lower.contains("invalid"))
                || lower.contains("invalid_encrypted_content")
                || (lower.contains("encrypted")
                    && (lower.contains("could not be decrypted")
                        || lower.contains("could not be verified"))),
            permanent_status: PERMANENT_HTTP_STATUSES
                .iter()
                .any(|status| has_status(*status)),
            rejected_credentials_or_request: [
                "invalid_api_key",
                "invalid api key",
                "authentication",
                "unauthorized",
                "invalid_request",
                "invalid request",
            ]
            .iter()
            .any(|marker| lower.contains(marker)),
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
