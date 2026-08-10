use super::*;

pub(super) async fn load_rows<T>(
    access: &ConfigAccess,
    collection: &str,
    query: &str,
) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    rows_or_empty_if_collection_missing(access, collection, query)
        .await?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("decoding {collection} rows"))
}

pub(super) async fn rows_or_empty_if_collection_missing(
    access: &ConfigAccess,
    collection_name: &str,
    query: &str,
) -> Result<Vec<Value>> {
    let rows = match access.execute(query).await {
        Ok(response) => Ok(graphql_rows_from_response(&response, collection_name)),
        Err(error) => Err(error),
    };
    match rows {
        Ok(rows) => Ok(rows),
        Err(error)
            if gents_protocol::graphql::is_collection_missing_error_message(
                collection_name,
                &error.to_string(),
            ) =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(error),
    }
}
