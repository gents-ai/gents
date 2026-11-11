/// Decode the execution owner's pinned request limit. Zero is a valid
/// exhausted durable state; configured limits are validated positive before claim.
pub(crate) fn parse_aggregate_token_limit(
    max_total_tokens: Option<i64>,
) -> anyhow::Result<Option<u64>> {
    max_total_tokens
        .map(|limit| {
            let limit = u64::try_from(limit)
                .map_err(|_| anyhow::anyhow!("pinned max_total_tokens must not be negative"))?;
            Ok(limit)
        })
        .transpose()
}
