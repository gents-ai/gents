use super::{ConfigApplyTxn, ConfigClientError};
use crate::graphql::{cursor_page_data, cursor_query, GraphqlResponseError, CURSOR_PAGE_SIZE};
use rapidhash::{HashSetExt, RapidHashSet};
use serde_json::Value;

impl ConfigApplyTxn<'_> {
    /// Enumerate within this transaction and its DID. Only native `hasNext`
    /// establishes completion; a short authorized page is not an EOF signal.
    /// Collection/field selections are caller-owned schema syntax, and filter
    /// values must already be escaped through the GraphQL owner.
    pub(crate) async fn cursor_rows(
        &self,
        collection: &str,
        fields: &str,
        filter: &str,
    ) -> Result<Vec<Value>, ConfigClientError> {
        let mut after = None;
        let mut cursors = RapidHashSet::new();
        let mut ids = RapidHashSet::new();
        let mut rows = Vec::new();
        loop {
            let response = self
                .execute(&cursor_query(collection, fields, filter, after.as_deref()))
                .await?;
            let (page, next) = cursor_page_data::<Value>(response.get("data"), collection)?;
            if page.len() > CURSOR_PAGE_SIZE {
                return Err(GraphqlResponseError::CursorPageOverflow.into());
            }
            for row in page {
                let id = row
                    .get("_docID")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or(GraphqlResponseError::CursorRowIdentityMissing)?;
                if !ids.insert(id.to_owned()) {
                    return Err(GraphqlResponseError::CursorRepeatedDocument.into());
                }
                rows.push(row);
            }
            let Some(next) = next else { break };
            if next.is_empty() || !cursors.insert(next.clone()) {
                return Err(GraphqlResponseError::CursorStalled.into());
            }
            after = Some(next);
        }
        Ok(rows)
    }
}
