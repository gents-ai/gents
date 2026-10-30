use thiserror::Error;

use crate::provider_limit::{
    classify_provider_limit, strip_provider_limit_marker, ProviderLimit, ProviderLimitHeaders,
};

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

    /// The provider rejected the request because its input exceeds the
    /// model's context window. `input_tokens` and `limit` are the provider's
    /// own counts when its message states them.
    #[error("context length exceeded: {reason}")]
    ContextLengthExceeded {
        reason: String,
        input_tokens: Option<u32>,
        limit: Option<u32>,
    },

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
        _ => ProviderReport {
            account_limited: ProviderLimitHeaders::from_text(message).account_rotated,
            ..ProviderReport::classify_failure_text(&text)
        },
    };
    let error = if let Some(overflow) = report.context_overflow {
        InferenceError::ContextLengthExceeded {
            reason: text.clone(),
            input_tokens: overflow.input_tokens,
            limit: overflow.limit,
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
/// rejections of replayed thinking or encrypted reasoning, and the context
/// overflow rejections: Anthropic's 400 `prompt is too long: N tokens > M
/// maximum`, OpenAI's `context_length_exceeded` / `maximum context length is
/// M tokens ... resulted in N tokens`, xAI's `maximum prompt length is M ...
/// contains N tokens`, and the generic `input is too long` / `exceeds the
/// context window`. A 413 is a body-size limit, not a token overflow, so the
/// status alone never reports one. This is the one reader of that text; the
/// loop decides on the report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProviderReport {
    pub context_overflow: Option<ContextOverflow>,
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
    /// The failure text carries the `account-rotated` provider-limit marker:
    /// another account of the pool can serve the request now. Read from the
    /// marker, which `classify_failure_text` never sees.
    pub account_limited: bool,
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
            context_overflow: CONTEXT_OVERFLOW_MARKERS
                .iter()
                .any(|marker| lower.contains(marker))
                .then(|| ContextOverflow::from_lowercase_text(&lower)),
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
            account_limited: false,
        }
    }
}

const CONTEXT_OVERFLOW_MARKERS: &[&str] = &[
    "prompt is too long",
    "input is too long",
    "context_length_exceeded",
    "maximum context length",
    "maximum prompt length",
    "exceeds the context window",
    "exceed context limit",
];

/// The token counts a provider's context overflow rejection states, when it
/// states them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContextOverflow {
    /// The provider's count of the rejected request's input.
    pub input_tokens: Option<u32>,
    /// The provider's maximum input for the model.
    pub limit: Option<u32>,
}

impl ContextOverflow {
    fn from_lowercase_text(lower: &str) -> Self {
        let first = |markers: &[&str]| {
            markers
                .iter()
                .find_map(|marker| leading_count_after(lower, marker))
        };
        // Claude reports `input + max_tokens > limit` when the input fits but
        // the requested output does not.
        let summed_limit = lower
            .split_once("exceed context limit:")
            .and_then(|(_, rest)| leading_count_after(rest, ">"));
        Self {
            input_tokens: requested_message_tokens(lower).or_else(|| {
                first(&[
                    "prompt is too long:",
                    "exceed context limit:",
                    "resulted in",
                    "request contains",
                    "you requested",
                ])
            }),
            limit: summed_limit.or_else(|| {
                first(&[
                    "tokens >",
                    "maximum context length is",
                    "maximum prompt length is",
                ])
            }),
        }
    }
}

/// OpenAI's `you requested N tokens (X in the messages, Y in the completion)`
/// counts the requested completion too; the input is the messages share.
fn requested_message_tokens(lower: &str) -> Option<u32> {
    let (_, rest) = lower.split_once("you requested")?;
    let (_, breakdown) = rest.split_once("tokens (")?;
    let messages = leading_count_after(breakdown, "")?;
    let (counted, _) = breakdown.split_once(',')?;
    counted
        .trim_end()
        .ends_with("in the messages")
        .then_some(messages)
}

fn leading_count_after(text: &str, marker: &str) -> Option<u32> {
    let (_, rest) = text.split_once(marker)?;
    let rest = rest.trim_start();
    let digits = rest
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit() || *character == ',')
        .map(|(index, character)| index + character.len_utf8())
        .last()?;
    rest[..digits].replace(',', "").parse().ok()
}

#[cfg(test)]
#[path = "error_tests.rs"]
pub(crate) mod tests;
