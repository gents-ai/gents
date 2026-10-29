use std::sync::{Arc, Mutex};

use rig::agent::StreamingError;
use rig::completion::{CompletionError, CompletionRequest, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateTokenCharge {
    Missing,
    Within,
    Exhausted,
    Overrun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregatePostChargeAction {
    Continue,
    Succeed,
    Fail,
}

pub fn aggregate_post_charge_action(
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
pub struct AggregateTokenLedger {
    pub limit: u64,
    pub used: u64,
}

pub const AGGREGATE_TOKEN_BUDGET_EXHAUSTED_PREFIX: &str = "aggregate_token_budget_exhausted: ";

impl crate::error::LoopStreamError {
    /// The provider message that ended the loop because the aggregate token
    /// budget was spent.
    pub fn aggregate_token_budget_exhaustion(&self) -> Option<&str> {
        match self.loop_verdict()? {
            (crate::error::LoopVerdict::AggregateTokenBudgetExhausted, message) => Some(message),
            _ => None,
        }
    }
}

/// The aggregate budget exhaustion anywhere in an erased compactor error,
/// asked of each loop owner it reaches.
pub fn aggregate_token_budget_exhaustion_message(
    error: &(dyn std::error::Error + 'static),
) -> Option<String> {
    gents_protocol::error_chain::find_map_source(error, |cause| {
        cause
            .downcast_ref::<crate::error::LoopStreamError>()
            .and_then(crate::error::LoopStreamError::aggregate_token_budget_exhaustion)
            .or_else(|| {
                cause
                    .downcast_ref::<super::OneShotError>()?
                    .aggregate_token_budget_exhaustion()
            })
            .or_else(|| {
                cause
                    .downcast_ref::<crate::compaction::CompactionError>()?
                    .aggregate_token_budget_exhaustion()
            })
            .map(ToString::to_string)
            .or_else(|| {
                cause
                    .downcast_ref::<super::TurnCompactionError>()?
                    .aggregate_token_budget_exhaustion()
            })
    })
}

#[derive(Debug, Clone)]
pub struct AggregateTokenBudget {
    ledger: Arc<Mutex<Option<AggregateTokenLedger>>>,
}

impl AggregateTokenBudget {
    pub fn new(limit: u64) -> Self {
        Self::with_prior_usage(limit, 0)
    }

    pub fn with_prior_usage(limit: u64, used: u64) -> Self {
        Self {
            ledger: Arc::new(Mutex::new(Some(AggregateTokenLedger { limit, used }))),
        }
    }

    pub fn snapshot(&self) -> Result<AggregateTokenLedger, StreamingError> {
        self.lock()?
            .as_ref()
            .copied()
            .ok_or_else(|| ledger_unavailable("provider usage was incomplete"))
    }

    pub fn invalidate(&self) -> Result<(), StreamingError> {
        *self.lock()? = None;
        Ok(())
    }

    fn lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<AggregateTokenLedger>>, StreamingError> {
        self.ledger
            .lock()
            .map_err(|_| ledger_unavailable("request budget lock was poisoned"))
    }

    pub(super) fn charge_reported(
        &self,
        usage: Option<Usage>,
    ) -> Result<(AggregateTokenCharge, AggregateTokenLedger), StreamingError> {
        let mut shared = self.lock()?;
        let ledger = shared
            .as_mut()
            .ok_or_else(|| ledger_unavailable("provider usage was incomplete"))?;
        let charge = ledger.charge_reported(usage);
        let snapshot = *ledger;
        if charge == AggregateTokenCharge::Missing {
            *shared = None;
        }
        Ok((charge, snapshot))
    }
}

fn ledger_unavailable(reason: &str) -> StreamingError {
    StreamingError::Completion(CompletionError::ProviderError(format!(
        "aggregate_token_ledger_unavailable: {reason}"
    )))
}

impl AggregateTokenLedger {
    pub fn remaining(self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    pub fn effective_output_tokens(self, input_tokens: u64, configured_max: u64) -> u64 {
        configured_max.min(self.remaining().saturating_sub(input_tokens))
    }

    pub fn can_dispatch(self, input_tokens: u64, configured_max: u64) -> bool {
        self.effective_output_tokens(input_tokens, configured_max) > 0
    }

    pub fn charge_reported(&mut self, usage: Option<Usage>) -> AggregateTokenCharge {
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

pub(super) fn clamp_request_aggregate_token_budget(
    request: &mut CompletionRequest,
    budget: Option<&AggregateTokenBudget>,
    context: &crate::provider_input::budget::ContextEstimate,
) -> Result<(), crate::error::LoopStreamError> {
    let Some(budget) = budget else {
        return Ok(());
    };
    let ledger = budget.snapshot()?;
    // Only the raw estimate refuses; the calibrated count reserves the
    // output cap so the request cannot overrun the budget it was granted.
    let input_tokens = u64::try_from(context.raw).unwrap_or(u64::MAX);
    let reserved_input = u64::try_from(context.tokens).unwrap_or(u64::MAX);
    let configured_max = request.max_tokens.unwrap_or(u64::MAX);
    let effective_max = match ledger.effective_output_tokens(reserved_input, configured_max) {
        0 => ledger.effective_output_tokens(input_tokens, configured_max),
        reserved => reserved,
    };
    if !ledger.can_dispatch(input_tokens, configured_max) {
        return Err(crate::rig_compat::loop_verdict_error(
            crate::error::LoopVerdict::AggregateTokenBudgetExhausted,
            format!(
                "{AGGREGATE_TOKEN_BUDGET_EXHAUSTED_PREFIX}limit={}, used={}, \
                 estimated_input_tokens={input_tokens}, remaining={}",
                ledger.limit,
                ledger.used,
                ledger.remaining(),
            ),
        ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[test]
    fn budget_exhaustion_classifies_through_every_loop_owner() {
        use crate::compaction::CompactionError;
        use crate::error::{CompletionFailure, LoopFailureCause, LoopStreamError};
        use crate::loop_stream::{OneShotError, TurnCompactionError};
        let reason = format!("{AGGREGATE_TOKEN_BUDGET_EXHAUSTED_PREFIX}limit=1 used=2");
        let loop_error = |message: &str| {
            LoopStreamError::new(
                LoopFailureCause::Completion {
                    failure: CompletionFailure::Loop {
                        verdict: crate::error::LoopVerdict::AggregateTokenBudgetExhausted,
                        message: message.to_string(),
                    },
                    reason: String::new(),
                },
                std::io::Error::other("provider"),
            )
        };
        let one_shot = || OneShotError::LoopStream(loop_error(&reason));
        #[derive(Debug, thiserror::Error)]
        #[error("compactor step")]
        struct Step(#[source] CompactionError);
        let turns = [
            TurnCompactionError::Compaction(CompactionError::BuildSummaryRequest(loop_error(
                &reason,
            ))),
            TurnCompactionError::Compaction(CompactionError::GuidedBudgetExhausted(one_shot())),
            TurnCompactionError::Compaction(CompactionError::FallbackBudgetExhausted(
                OneShotError::ErrorCleanup {
                    original: String::new(),
                    source: Box::new(one_shot()),
                },
            )),
            TurnCompactionError::Compactor(Box::new(Step(CompactionError::GuidedBudgetExhausted(
                one_shot(),
            )))),
            TurnCompactionError::Compactor(Box::new(loop_error(&reason))),
        ];
        for turn in turns {
            assert_eq!(
                turn.aggregate_token_budget_exhaustion().as_deref(),
                Some(reason.as_str()),
                "{turn:?}"
            );
            assert_eq!(
                aggregate_token_budget_exhaustion_message(&turn).as_deref(),
                Some(reason.as_str())
            );
        }
        let other = TurnCompactionError::Compaction(CompactionError::BuildSummaryRequest(
            LoopStreamError::new(
                LoopFailureCause::Completion {
                    failure: CompletionFailure::Provider(reason.clone()),
                    reason: String::new(),
                },
                std::io::Error::other("provider"),
            ),
        ));
        assert_eq!(other.aggregate_token_budget_exhaustion(), None);
        assert_eq!(aggregate_token_budget_exhaustion_message(&other), None);
    }

    #[derive(Deserialize)]
    struct Snapshot {
        aggregate_token_budget_cases: Vec<Case>,
    }

    #[derive(Deserialize)]
    struct Case {
        name: String,
        limit: u64,
        used: u64,
        usage_present: bool,
        reported_input_tokens: u64,
        reported_output_tokens: u64,
        reported_total_tokens: u64,
        next_used: Option<u64>,
        abandoned_used: Option<u64>,
    }

    #[test]
    fn generated_budget_cases_keep_all_borrowers_on_the_same_allowance() {
        let snapshot: Snapshot = gents_lean_contract::load_contract_snapshot().unwrap();
        assert!(!snapshot.aggregate_token_budget_cases.is_empty());
        for case in snapshot.aggregate_token_budget_cases {
            let budget = AggregateTokenBudget::with_prior_usage(case.limit, case.used);
            let borrower = budget.clone();
            let usage = case.usage_present.then_some(Usage {
                input_tokens: case.reported_input_tokens,
                output_tokens: case.reported_output_tokens,
                total_tokens: case.reported_total_tokens,
                ..Default::default()
            });
            budget.charge_reported(usage).unwrap();
            for observed in [&budget, &borrower] {
                assert_eq!(
                    observed.snapshot().ok().map(|ledger| ledger.used),
                    case.next_used,
                    "{}",
                    case.name
                );
            }
            borrower.invalidate().unwrap();
            assert!(budget.charge_reported(usage).is_err(), "{}", case.name);
            assert_eq!(
                budget.snapshot().ok().map(|ledger| ledger.used),
                case.abandoned_used,
                "{}",
                case.name
            );
        }
    }
}
