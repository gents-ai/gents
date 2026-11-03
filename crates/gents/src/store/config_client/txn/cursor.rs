use super::{ConfigApplyTxn, ConfigClientError};
use crate::store::graphql::{
    cursor_page_data, cursor_query, GraphqlResponseError, CURSOR_PAGE_SIZE,
};
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
        let mut rows = Vec::new();
        self.visit_cursor_pages(collection, fields, filter, |page| {
            rows.extend_from_slice(page);
            Ok::<(), ConfigClientError>(())
        })
        .await?;
        Ok(rows)
    }

    /// Visits authorized cursor pages in this transaction's snapshot. A caller
    /// that writes external output must discard it if the transaction fails or
    /// its callback is replayed. `collection` and `fields` are schema syntax;
    /// escape filter values using the GraphQL owner before passing them here.
    pub async fn visit_cursor_pages<E, F>(
        &self,
        collection: &str,
        fields: &str,
        filter: &str,
        mut visit: F,
    ) -> Result<(), E>
    where
        E: From<ConfigClientError>,
        F: FnMut(&[Value]) -> Result<(), E>,
    {
        let mut after = None;
        let mut cursors = RapidHashSet::new();
        let mut ids = RapidHashSet::new();
        loop {
            let response = self
                .execute(&cursor_query(collection, fields, filter, after.as_deref()))
                .await
                .map_err(E::from)?;
            let (page, next) = cursor_page_data::<Value>(response.get("data"), collection)
                .map_err(ConfigClientError::from)
                .map_err(E::from)?;
            if page.len() > CURSOR_PAGE_SIZE {
                return Err(E::from(GraphqlResponseError::CursorPageOverflow.into()));
            }
            for row in &page {
                let id = row
                    .get("_docID")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or(GraphqlResponseError::CursorRowIdentityMissing)
                    .map_err(ConfigClientError::from)
                    .map_err(E::from)?;
                if !ids.insert(id.to_owned()) {
                    return Err(E::from(GraphqlResponseError::CursorRepeatedDocument.into()));
                }
            }
            if let Some(next) = &next {
                if next.is_empty() || !cursors.insert(next.clone()) {
                    return Err(E::from(GraphqlResponseError::CursorStalled.into()));
                }
            }
            visit(&page)?;
            let Some(next) = next else { break };
            after = Some(next);
        }
        Ok(())
    }
}
