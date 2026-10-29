use std::time::Duration;

use chrono::{DateTime, Utc};
use rand::Rng;

use crate::error::{ClassifiedFailure, InferenceError};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletionRetryProfileFields {
    pub retry_max_transport: Option<i64>,
    pub retry_backoff_ms: Option<Vec<i64>>,
    pub retry_max_resample: Option<i64>,
    pub retry_allow_repair: Option<bool>,
    pub retry_interactive_max: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    Transport,
    ParseBadRequest,
    ReasoningRejected,
    Permanent,
}

pub fn failure_class(failure: &ClassifiedFailure) -> FailureClass {
    let ClassifiedFailure { error, report, .. } = failure;
    if report.malformed_tool_call || report.fail_closed_tool_use {
        return FailureClass::ParseBadRequest;
    }
    if report.reasoning_rejected && !matches!(error, InferenceError::UsageLimited(_)) {
        return FailureClass::ReasoningRejected;
    }

    match error {
        InferenceError::ModelUnreachable { .. }
        | InferenceError::TransientFailure { .. }
        | InferenceError::Timeout { .. }
        | InferenceError::RateLimited { .. } => FailureClass::Transport,
        InferenceError::PermanentFailure { .. }
        | InferenceError::UsageLimited(_)
        | InferenceError::ContextLengthExceeded { .. }
        | InferenceError::RetriesExhausted { .. } => FailureClass::Permanent,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRetryPolicy {
    pub transport_backoff: Vec<Duration>,
    pub max_resample: u32,
    pub allow_repair: bool,
}

impl CompletionRetryPolicy {
    pub fn resolve(
        fields: &CompletionRetryProfileFields,
        origin: crate::execution_origin::ExecutionOrigin,
    ) -> Self {
        let default = Self::default_for_origin(origin);
        let mut transport_backoff = fields
            .retry_backoff_ms
            .as_deref()
            .and_then(backoff_from_profile)
            .unwrap_or(default.transport_backoff);

        let max_transport = match origin {
            crate::execution_origin::ExecutionOrigin::Interactive => fields
                .retry_interactive_max
                .or(fields.retry_max_transport)
                .and_then(nonnegative_usize),
            crate::execution_origin::ExecutionOrigin::Scheduled => {
                fields.retry_max_transport.and_then(nonnegative_usize)
            }
        };
        if let Some(max_transport) = max_transport {
            transport_backoff = resize_backoff(transport_backoff, max_transport);
        }

        Self {
            transport_backoff,
            max_resample: fields
                .retry_max_resample
                .and_then(nonnegative_u32)
                .unwrap_or(default.max_resample),
            allow_repair: fields.retry_allow_repair.unwrap_or(default.allow_repair),
        }
    }

    pub fn default_for_origin(origin: crate::execution_origin::ExecutionOrigin) -> Self {
        match origin {
            crate::execution_origin::ExecutionOrigin::Interactive => Self::interactive_default(),
            crate::execution_origin::ExecutionOrigin::Scheduled => Self::scheduled_default(),
        }
    }

    pub fn scheduled_default() -> Self {
        Self {
            transport_backoff: vec![
                Duration::from_secs(5),
                Duration::from_secs(30),
                Duration::from_secs(120),
            ],
            max_resample: 1,
            allow_repair: true,
        }
    }

    pub fn interactive_default() -> Self {
        Self {
            transport_backoff: vec![Duration::from_secs(2)],
            max_resample: 0,
            allow_repair: true,
        }
    }

    pub fn no_retry() -> Self {
        Self {
            transport_backoff: Vec::new(),
            max_resample: 0,
            allow_repair: false,
        }
    }

    pub fn internal_immediate() -> Self {
        Self {
            transport_backoff: vec![
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(2),
            ],
            max_resample: 0,
            allow_repair: false,
        }
    }
}

fn backoff_from_profile(values: &[i64]) -> Option<Vec<Duration>> {
    if values.is_empty() {
        return None;
    }
    let backoff = values
        .iter()
        .filter_map(|value| u64::try_from(*value).ok())
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .collect::<Vec<_>>();
    (!backoff.is_empty()).then_some(backoff)
}

fn resize_backoff(mut backoff: Vec<Duration>, target_len: usize) -> Vec<Duration> {
    if target_len == 0 {
        return Vec::new();
    }
    if backoff.is_empty() {
        backoff.push(Duration::from_secs(1));
    }
    if backoff.len() > target_len {
        backoff.truncate(target_len);
    } else {
        let last = *backoff.last().expect("backoff is non-empty");
        backoff.resize(target_len, last);
    }
    backoff
}

fn nonnegative_usize(value: i64) -> Option<usize> {
    (value >= 0).then(|| usize::try_from(value).ok()).flatten()
}

fn nonnegative_u32(value: i64) -> Option<u32> {
    (value >= 0).then(|| u32::try_from(value).ok()).flatten()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryKind {
    Transport,
    Resample,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreStreamDirective {
    RetryAfter { delay: Duration, kind: RetryKind },
    Repair,
    Fail { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidStreamDirective {
    RetractAndResample { delay: Duration },
    CloseAndContinue { delay: Duration },
    Fail { reason: String },
}

#[derive(Debug, Clone)]
pub struct CompletionRetryState {
    policy: CompletionRetryPolicy,
    transport_used: u32,
    resample_used: u32,
    repair_used: bool,
    last_parse_error: Option<String>,
}

impl CompletionRetryState {
    pub fn new(policy: CompletionRetryPolicy) -> Self {
        Self {
            policy,
            transport_used: 0,
            resample_used: 0,
            repair_used: false,
            last_parse_error: None,
        }
    }

    pub fn retry_count(&self) -> u32 {
        self.transport_used + self.resample_used
    }

    pub fn mark_repair_used(&mut self) {
        self.repair_used = true;
    }

    pub fn on_pre_stream_failure(
        &mut self,
        failure: &ClassifiedFailure,
        now: DateTime<Utc>,
        deadline: Option<DateTime<Utc>>,
    ) -> PreStreamDirective {
        let (error, error_text) = (&failure.error, failure.text.as_str());
        match failure_class(failure) {
            FailureClass::Permanent => PreStreamDirective::Fail {
                reason: match error {
                    InferenceError::UsageLimited(limit) => limit.to_string(),
                    InferenceError::PermanentFailure { .. } => error.to_string(),
                    _ => format!("permanent inference failure: {error}"),
                },
            },
            FailureClass::Transport => {
                match ladder_delay(&self.policy.transport_backoff, self.transport_used) {
                    None => PreStreamDirective::Fail {
                        reason: format!(
                            "transport retry budget exhausted after {} attempt(s)",
                            self.transport_used
                        ),
                    },
                    Some(base_delay) => {
                        let base_delay = match error {
                            InferenceError::RateLimited {
                                retry_after: Some(retry_after),
                            } => base_delay.max(*retry_after),
                            _ => base_delay,
                        };
                        let delay = jitter(base_delay, &mut rand::rng());
                        if exceeds_deadline(now, delay, deadline) {
                            return PreStreamDirective::Fail {
                                reason: deadline_reason(delay, deadline),
                            };
                        }
                        self.transport_used += 1;
                        PreStreamDirective::RetryAfter {
                            delay,
                            kind: RetryKind::Transport,
                        }
                    }
                }
            }
            FailureClass::ReasoningRejected => {
                if self.policy.allow_repair && !self.repair_used {
                    self.last_parse_error = Some(error_text.to_string());
                    PreStreamDirective::Repair
                } else {
                    PreStreamDirective::Fail {
                        reason: format!(
                            "provider rejected replayed reasoning after the one repair: {error}"
                        ),
                    }
                }
            }
            FailureClass::ParseBadRequest => {
                let is_fresh = self.last_parse_error.as_deref() != Some(error_text);
                let resample_budget_spent = self.resample_used >= self.policy.max_resample;

                let resample_pacing =
                    resample_delay(&self.policy.transport_backoff, self.resample_used);

                if let Some(base_delay) =
                    resample_pacing.filter(|_| is_fresh && !resample_budget_spent)
                {
                    let delay = jitter(base_delay, &mut rand::rng());
                    if exceeds_deadline(now, delay, deadline) {
                        return PreStreamDirective::Fail {
                            reason: deadline_reason(delay, deadline),
                        };
                    }
                    self.resample_used += 1;
                    self.last_parse_error = Some(error_text.to_string());
                    PreStreamDirective::RetryAfter {
                        delay,
                        kind: RetryKind::Resample,
                    }
                } else if self.policy.allow_repair && !self.repair_used {
                    self.last_parse_error = Some(error_text.to_string());
                    PreStreamDirective::Repair
                } else {
                    PreStreamDirective::Fail {
                        reason: "parse-400 could not be resolved: resample and repair are both \
                                 exhausted or unavailable"
                            .to_string(),
                    }
                }
            }
        }
    }

    pub fn on_mid_stream_failure(
        &mut self,
        effects_this_turn: bool,
        now: DateTime<Utc>,
        deadline: Option<DateTime<Utc>>,
    ) -> MidStreamDirective {
        match ladder_delay(&self.policy.transport_backoff, self.transport_used) {
            None => MidStreamDirective::Fail {
                reason: format!(
                    "transport retry budget exhausted after {} attempt(s)",
                    self.transport_used
                ),
            },
            Some(base_delay) => {
                let delay = jitter(base_delay, &mut rand::rng());
                if exceeds_deadline(now, delay, deadline) {
                    return MidStreamDirective::Fail {
                        reason: deadline_reason(delay, deadline),
                    };
                }
                self.transport_used += 1;
                if effects_this_turn {
                    MidStreamDirective::CloseAndContinue { delay }
                } else {
                    MidStreamDirective::RetractAndResample { delay }
                }
            }
        }
    }
}

fn ladder_delay(backoff: &[Duration], used: u32) -> Option<Duration> {
    backoff.get(used as usize).copied()
}

fn resample_delay(backoff: &[Duration], used: u32) -> Option<Duration> {
    backoff
        .get(used as usize)
        .or_else(|| backoff.last())
        .copied()
}

fn exceeds_deadline(now: DateTime<Utc>, delay: Duration, deadline: Option<DateTime<Utc>>) -> bool {
    match deadline {
        None => false,
        Some(deadline) => now + chrono_duration_from_std(delay) > deadline,
    }
}

pub fn retry_wake_fits_deadline(
    observed_at: DateTime<Utc>,
    deadline: Option<DateTime<Utc>>,
) -> bool {
    deadline.is_none_or(|deadline| observed_at <= deadline)
}

fn deadline_reason(delay: Duration, deadline: Option<DateTime<Utc>>) -> String {
    match deadline {
        Some(deadline) => format!(
            "retry delay of {delay:?} would exceed the request deadline ({deadline}); \
             failing fast rather than sleeping into certain death"
        ),
        None => "retry delay would exceed the request deadline".to_string(),
    }
}

fn chrono_duration_from_std(delay: Duration) -> chrono::Duration {
    chrono::Duration::milliseconds(delay.as_millis().min(i64::MAX as u128) as i64)
}

fn jitter(base_delay: Duration, rng: &mut impl Rng) -> Duration {
    let base_ms = base_delay.as_millis().min(u64::MAX as u128) as u64;
    let jitter_range = base_ms / 4;
    let delta: i64 = if jitter_range > 0 {
        rng.random_range(0..=jitter_range * 2) as i64 - jitter_range as i64
    } else {
        0
    };
    let final_ms = (base_ms as i64 + delta).max(100) as u64;
    Duration::from_millis(final_ms)
}

#[cfg(test)]
#[path = "completion_retry_tests.rs"]
mod tests;
