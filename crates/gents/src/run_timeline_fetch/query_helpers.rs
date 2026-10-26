use super::error::{Result, RunTimelineError};
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
        .collect::<std::result::Result<Vec<T>, _>>()
        .map_err(|source| RunTimelineError::RowsDecode {
            collection: collection.to_owned(),
            source,
        })
}

pub(super) async fn required_rows(
    access: &ConfigAccess,
    collection_name: &str,
    query: &str,
) -> Result<Vec<Value>> {
    let response =
        access
            .execute(query)
            .await
            .map_err(|source| RunTimelineError::RequiredRowsRead {
                collection: collection_name.to_owned(),
                source,
            })?;
    decode_required_rows(&response, collection_name)
}

fn decode_required_rows(response: &Value, collection: &str) -> Result<Vec<Value>> {
    if !response
        .get("errors")
        .is_none_or(|errors| errors.is_null() || errors.as_array().is_some_and(Vec::is_empty))
    {
        return Err(RunTimelineError::RequiredRowsFailed {
            collection: collection.to_owned(),
            errors: response["errors"].clone(),
        });
    }
    let rows = response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .ok_or_else(|| RunTimelineError::RequiredRowsOmitted {
            collection: collection.to_owned(),
        })?;
    if !rows.iter().all(Value::is_object) {
        return Err(RunTimelineError::NonDocumentRows {
            collection: collection.to_owned(),
        });
    }
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
