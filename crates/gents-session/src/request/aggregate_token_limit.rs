/// Decode the execution owner's pinned request limit. Zero is a valid
/// exhausted durable state; configured limits are validated positive before claim.
pub fn parse_aggregate_token_limit(max_total_tokens: Option<i64>) -> anyhow::Result<Option<u64>> {
    max_total_tokens
        .map(|limit| {
            let limit = u64::try_from(limit)
                .map_err(|_| anyhow::anyhow!("pinned max_total_tokens must not be negative"))?;
            Ok(limit)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_pinned_token_limits_preserve_signed_integer_boundaries() {
        use gents_lean_vocab::families::request_pinned_token_limit_pinned_limit as family;
        family::replay(|input| {
            let result = parse_aggregate_token_limit(input.limit);
            family::Outcome {
                accepted: result.is_ok(),
                limit: result.ok().flatten(),
            }
        });
    }
}
