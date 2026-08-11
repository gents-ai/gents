use std::sync::{Arc, Mutex};

use rig::agent::StreamingError;
use rig::completion::{CompletionError, CompletionRequest, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AggregateTokenCharge {
    Missing,
    Within,
    Exhausted,
    Overrun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AggregatePostChargeAction {
    Continue,
    Succeed,
    Fail,
}

pub(super) fn aggregate_post_charge_action(
    charge: AggregateTokenCharge,
    terminal_valid: bool,
) -> AggregatePostChargeAction {
    match charge {
        AggregateTokenCharge::Missing | AggregateTokenCharge::Overrun => {
            AggregatePostChargeAction::Fail
        }
        AggregateTokenCharge::Within if terminal_valid => AggregatePostChargeAction::Succeed,
        AggregateTokenCharge::Within => AggregatePostChargeAction::Continue,
        AggregateTokenCharge::Exhausted if terminal_valid => AggregatePostChargeAction::Succeed,
        AggregateTokenCharge::Exhausted => AggregatePostChargeAction::Fail,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AggregateTokenLedger {
    pub(crate) limit: u64,
    pub(crate) used: u64,
}

pub(crate) const AGGREGATE_TOKEN_BUDGET_EXHAUSTED_PREFIX: &str =
    "aggregate_token_budget_exhausted: ";

/// Recover only the typed request-budget failure from an anyhow context
/// chain. Matching the underlying `StreamingError` rather than arbitrary text
/// prevents provider/model content from forging Harbor's scoreable outcome.
pub(crate) fn aggregate_token_budget_exhaustion_message(error: &anyhow::Error) -> Option<String> {
    error.chain().find_map(|cause| {
        let streaming_error = cause.downcast_ref::<StreamingError>()?;
        let StreamingError::Completion(CompletionError::ProviderError(reason)) = streaming_error
        else {
            return None;
        };
        reason
            .starts_with(AGGREGATE_TOKEN_BUDGET_EXHAUSTED_PREFIX)
            .then(|| reason.clone())
    })
}

/// Cloneable handle to the single monotone ledger for one durable request.
///
/// Nested compaction runs execute their own owned loop, so carrying only the
/// numeric limit would mint a fresh allowance. Sharing this handle makes all
/// provider calls compete for and charge the same request-wide budget.
#[derive(Debug, Clone)]
pub(crate) struct AggregateTokenBudget {
    ledger: Arc<Mutex<AggregateTokenLedger>>,
}

impl AggregateTokenBudget {
    pub(crate) fn new(limit: u64) -> Self {
        Self::with_prior_usage(limit, 0)
    }

    /// Mint a ledger that already reflects durable `InferenceCall` usage for
    /// this physical request (`request_doc_id`). Crash redrive and mid-request
    /// restart must not reset `used` to zero or the budget can be exceeded.
    pub(crate) fn with_prior_usage(limit: u64, used: u64) -> Self {
        Self {
            ledger: Arc::new(Mutex::new(AggregateTokenLedger { limit, used })),
        }
    }

    pub(crate) fn snapshot(&self) -> Result<AggregateTokenLedger, StreamingError> {
        self.ledger.lock().map(|ledger| *ledger).map_err(|_| {
            StreamingError::Completion(CompletionError::ProviderError(
                "aggregate_token_ledger_unavailable: request budget lock was poisoned".to_string(),
            ))
        })
    }

    pub(super) fn charge_reported(
        &self,
        usage: Option<Usage>,
    ) -> Result<(AggregateTokenCharge, AggregateTokenLedger), StreamingError> {
        let mut ledger = self.ledger.lock().map_err(|_| {
            StreamingError::Completion(CompletionError::ProviderError(
                "aggregate_token_ledger_unavailable: request budget lock was poisoned".to_string(),
            ))
        })?;
        let charge = ledger.charge_reported(usage);
        Ok((charge, *ledger))
    }
}

impl AggregateTokenLedger {
    pub(crate) fn remaining(self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    pub(crate) fn effective_output_tokens(self, input_tokens: u64, configured_max: u64) -> u64 {
        configured_max.min(self.remaining().saturating_sub(input_tokens))
    }

    pub(crate) fn can_dispatch(self, input_tokens: u64, configured_max: u64) -> bool {
        self.effective_output_tokens(input_tokens, configured_max) > 0
    }

    pub(super) fn charge_reported(&mut self, usage: Option<Usage>) -> AggregateTokenCharge {
        let Some(usage) = usage else {
            return AggregateTokenCharge::Missing;
        };
        let charged = crate::provider_usage::charged_usage_total(usage);
        if charged == 0 {
            return AggregateTokenCharge::Missing;
        }
        self.used = self.used.saturating_add(charged);
        match self.used.cmp(&self.limit) {
            std::cmp::Ordering::Less => AggregateTokenCharge::Within,
            std::cmp::Ordering::Equal => AggregateTokenCharge::Exhausted,
            std::cmp::Ordering::Greater => AggregateTokenCharge::Overrun,
        }
    }
}

/// Apply the request-wide token ledger immediately before every provider
/// dispatch. The input estimate is the same complete rendered-request estimate
/// fenced by `PromptAssembly.Budget`; provider tokenization remains an external
/// boundary, so the post-call usage report is checked independently.
pub(super) fn clamp_request_aggregate_token_budget(
    request: &mut CompletionRequest,
    budget: Option<&AggregateTokenBudget>,
) -> Result<(), StreamingError> {
    let Some(budget) = budget else {
        return Ok(());
    };
    let ledger = budget.snapshot()?;
    let input_tokens =
        u64::try_from(super::completion_request_input_estimate(request)).unwrap_or(u64::MAX);
    let configured_max = request.max_tokens.unwrap_or(u64::MAX);
    let effective_max = ledger.effective_output_tokens(input_tokens, configured_max);
    if !ledger.can_dispatch(input_tokens, configured_max) {
        return Err(StreamingError::Completion(CompletionError::ProviderError(
            format!(
                "{AGGREGATE_TOKEN_BUDGET_EXHAUSTED_PREFIX}limit={}, used={}, \
                 estimated_input_tokens={input_tokens}, remaining={}",
                ledger.limit,
                ledger.used,
                ledger.remaining(),
            ),
        )));
    }
    if effective_max < configured_max {
        tracing::debug!(
            token_limit = ledger.limit,
            tokens_used = ledger.used,
            input_tokens,
            configured_max_output_tokens = configured_max,
            effective_max_output_tokens = effective_max,
            "clamped completion output to the request-wide token budget"
        );
    }
    request.max_tokens = Some(effective_max);
    Ok(())
}
