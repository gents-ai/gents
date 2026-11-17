/// The successor budget (`AgentRequest.max_retries`) a request's first claim
/// pins: a retry successor of a pinned parent inherits that pin; otherwise a
/// normal request takes the claiming Agent's configured
/// `InferenceExecution.max_request_retries` and an auxiliary one takes zero.
pub fn claim_pin(
    normal: bool,
    configured: u64,
    retry_successor: bool,
    parent_pin: Option<i64>,
) -> anyhow::Result<u64> {
    match (retry_successor, parent_pin) {
        (true, Some(pin)) => u64::try_from(pin)
            .map_err(|_| anyhow::anyhow!("retry parent max_retries must not be negative")),
        _ if normal => Ok(configured),
        _ => Ok(0),
    }
}

/// Whether a terminal request's lineage may take one more successor. A
/// request without a pin never reached a claim: an operator reissue may follow
/// it, automated background redrive does not.
pub fn successor_budget_remains(
    automated: bool,
    retry_count: i64,
    max_retries: Option<i64>,
) -> bool {
    match max_retries {
        None => !automated,
        Some(max_retries) => (0..max_retries).contains(&retry_count),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_claim_pins_match_the_model() {
        use gents_lean_vocab::families::request_pinned_retry_budget_claim_pins as family;
        family::replay(|input| {
            let result = claim_pin(
                input.normal,
                input.configured,
                input.retry_successor,
                input.parent_pin,
            );
            family::Outcome {
                accepted: result.is_ok(),
                error: result.as_ref().err().map(ToString::to_string),
                max_retries: result.ok(),
            }
        });
    }

    #[test]
    fn generated_successor_budgets_match_the_model() {
        use gents_lean_vocab::families::request_pinned_retry_budget_successor_budgets as family;
        family::replay(|input| {
            successor_budget_remains(input.automated, input.retry_count, input.max_retries)
        });
    }
}
