use super::*;

pub(super) async fn load_rows<T>(
    access: &ConfigAccess,
    collection: &str,
    query: &str,
) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    required_rows(access, collection, query)
        .await?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("decoding {collection} rows"))
}

/// Execute one timeline query and return its rows. A missing collection is a
/// hard error, not a silently empty section: every collection read here
/// (`AgentMessage`, `AgentToolCall`, `InferenceCall`, `AgentSession`, `Goal`,
/// `AgentRequest`, ...) is required for a truthful timeline. Only a genuinely
/// empty result set returns `Ok(vec![])`.
pub(super) async fn required_rows(
    access: &ConfigAccess,
    collection_name: &str,
    query: &str,
) -> Result<Vec<Value>> {
    let response = access
        .execute(query)
        .await
        .with_context(|| format!("reading required timeline collection {collection_name}"))?;
    decode_required_rows(&response, collection_name)
}

fn decode_required_rows(response: &Value, collection: &str) -> Result<Vec<Value>> {
    anyhow::ensure!(
        response
            .get("errors")
            .is_none_or(|errors| errors.is_null() || errors.as_array().is_some_and(Vec::is_empty)),
        "timeline {collection} query failed: {}",
        response["errors"]
    );
    let rows = response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .with_context(|| format!("timeline query omitted required {collection} row array"))?;
    anyhow::ensure!(
        rows.iter().all(Value::is_object),
        "timeline {collection} query returned a non-document row"
    );
    Ok(rows.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_malformed_required_rows_are_not_empty_timelines() {
        for response in [
            serde_json::json!({}),
            serde_json::json!({"data": {}}),
            serde_json::json!({"data": {"AgentMessage": null}}),
            serde_json::json!({"data": {"AgentMessage": {}}}),
            serde_json::json!({"data": {"AgentMessage": [null]}}),
            serde_json::json!({"errors": [{"message": "denied"}], "data": {"AgentMessage": []}}),
        ] {
            assert!(
                decode_required_rows(&response, "AgentMessage").is_err(),
                "{response}"
            );
        }
        assert!(decode_required_rows(
            &serde_json::json!({
                "errors": [], "data": {"AgentMessage": []}
            }),
            "AgentMessage"
        )
        .unwrap()
        .is_empty());
    }
}
