use super::ConfigApplyTxn;
use crate::graphql::{cursor_page, cursor_query, CURSOR_PAGE_SIZE};
use anyhow::{Context, Result};
use rapidhash::{HashSetExt, RapidHashSet};
use serde_json::Value;

impl ConfigApplyTxn<'_> {
    /// Enumerate within this transaction and its DID. Only native `hasNext`
    /// establishes completion; a short authorized page is not an EOF signal.
    /// Collection/field selections are caller-owned schema syntax, and filter
    /// values must already be escaped through the GraphQL owner.
    pub(crate) async fn cursor_rows_local(
        &self,
        collection: &str,
        fields: &str,
        filter: &str,
    ) -> Result<Vec<Value>> {
        let mut after = None;
        let mut cursors = RapidHashSet::new();
        let mut ids = RapidHashSet::new();
        let mut rows = Vec::new();
        loop {
            let response = self
                .execute_local_response(&cursor_query(collection, fields, filter, after.as_deref()))
                .await?;
            let (page, next) = cursor_page::<Value>(&response, collection)?;
            anyhow::ensure!(
                page.len() <= CURSOR_PAGE_SIZE,
                "cursor page exceeded its native row bound"
            );
            for row in page {
                let id = row
                    .get("_docID")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .context("cursor row omitted physical identity")?;
                anyhow::ensure!(
                    ids.insert(id.to_owned()),
                    "cursor repeated a physical document"
                );
                rows.push(row);
            }
            let Some(next) = next else { break };
            anyhow::ensure!(
                !next.is_empty() && cursors.insert(next.clone()),
                "cursor did not advance"
            );
            after = Some(next);
        }
        Ok(rows)
    }
}
