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

pub fn aggregate_token_budget_exhaustion_message(
    error: &(dyn std::error::Error + 'static),
) -> Option<String> {
    std::iter::successors(Some(error), |cause| cause.source()).find_map(|cause| {
        let reason = cause
            .downcast_ref::<crate::error::LoopStreamError>()?
            .provider_message()?;
        reason
            .starts_with(AGGREGATE_TOKEN_BUDGET_EXHAUSTED_PREFIX)
            .then(|| reason.to_string())
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
    estimated_input_tokens: usize,
) -> Result<(), StreamingError> {
    let Some(budget) = budget else {
        return Ok(());
    };
    let ledger = budget.snapshot()?;
    let input_tokens = u64::try_from(estimated_input_tokens).unwrap_or(u64::MAX);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

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
