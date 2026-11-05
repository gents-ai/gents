use super::*;

pub(super) async fn load_rows<T>(
    access: &ConfigAccess,
    collection: &str,
    query: &str,
) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    query_rows(access, collection, query)
        .await?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("decoding {collection} rows"))
}

pub(super) async fn query_rows(
    access: &ConfigAccess,
    collection_name: &str,
    query: &str,
) -> Result<Vec<Value>> {
    let response = access
        .execute(query)
        .await
        .with_context(|| format!("reading timeline collection {collection_name}"))?;
    decode_rows(&response, collection_name)
}

pub(super) fn decode_rows(response: &Value, collection: &str) -> Result<Vec<Value>> {
    anyhow::ensure!(
        response
            .get("errors")
            .is_none_or(|errors| errors.is_null() || errors.as_array().is_some_and(Vec::is_empty)),
        "timeline {collection} query failed: {}",
        response["errors"]
    );
    let data = response
        .get("data")
        .and_then(Value::as_object)
        .context("timeline query omitted or malformed data")?;
    let Some(value) = data.get(collection) else {
        return Ok(Vec::new());
    };
    let rows = value
        .as_array()
        .with_context(|| format!("timeline query returned malformed {collection} row array"))?;
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
    fn absent_roots_are_empty_but_malformed_responses_and_errors_fail() {
        for response in [
            serde_json::json!({"data": {}}),
            serde_json::json!({"data": {"s0": [{"_docID": "other"}]}}),
            serde_json::json!({"errors": [], "data": {"AgentMessage": []}}),
        ] {
            assert!(decode_rows(&response, "AgentMessage").unwrap().is_empty());
        }
        for response in [
            serde_json::json!({}),
            serde_json::json!({"data": null}),
            serde_json::json!({"data": []}),
            serde_json::json!({"data": {"AgentMessage": null}}),
            serde_json::json!({"data": {"AgentMessage": {}}}),
            serde_json::json!({"data": {"AgentMessage": [null]}}),
            serde_json::json!({"errors": [{"message": "denied"}], "data": {"AgentMessage": []}}),
        ] {
            assert!(
                decode_rows(&response, "AgentMessage").is_err(),
                "{response}"
            );
        }
    }
}
