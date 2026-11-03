use super::{ConfigApplyTxn, ConfigClientError};
use crate::store::graphql::{
    cursor_page_data, cursor_query, GraphqlResponseError, CURSOR_PAGE_SIZE,
};
use rapidhash::{HashSetExt, RapidHashSet};
use serde_json::Value;

/// Cursor enumeration bound to one transaction and its authenticated actor.
pub struct ConfigCursor<'a, 'txn> {
    txn: &'a ConfigApplyTxn<'txn>,
    collection: &'a str,
    fields: &'a str,
    filter: &'a str,
    after: Option<String>,
    cursors: RapidHashSet<String>,
    ids: RapidHashSet<String>,
    done: bool,
}

impl ConfigCursor<'_, '_> {
    /// Only native `hasNext` establishes completion; a short authorized page is not EOF.
    pub async fn next_page(&mut self) -> Result<Option<Vec<Value>>, ConfigClientError> {
        if self.done {
            return Ok(None);
        }
        let response = self
            .txn
            .execute(&cursor_query(
                self.collection,
                self.fields,
                self.filter,
                self.after.as_deref(),
            ))
            .await?;
        let (page, next) = cursor_page_data::<Value>(response.get("data"), self.collection)?;
        if page.len() > CURSOR_PAGE_SIZE {
            return Err(GraphqlResponseError::CursorPageOverflow.into());
        }
        for row in &page {
            let id = row
                .get("_docID")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or(GraphqlResponseError::CursorRowIdentityMissing)?;
            if !self.ids.insert(id.to_owned()) {
                return Err(GraphqlResponseError::CursorRepeatedDocument.into());
            }
        }
        if let Some(next) = &next {
            if next.is_empty() || !self.cursors.insert(next.clone()) {
                return Err(GraphqlResponseError::CursorStalled.into());
            }
        }
        self.done = next.is_none();
        self.after = next;
        Ok(Some(page))
    }
}

impl<'txn> ConfigApplyTxn<'txn> {
    /// `collection` and `fields` are schema syntax; filter values must already
    /// be escaped through the GraphQL owner. Include `_docID` in the selection.
    pub fn cursor_scan<'a>(
        &'a self,
        collection: &'a str,
        fields: &'a str,
        filter: &'a str,
    ) -> ConfigCursor<'a, 'txn> {
        ConfigCursor {
            txn: self,
            collection,
            fields,
            filter,
            after: None,
            cursors: RapidHashSet::new(),
            ids: RapidHashSet::new(),
            done: false,
        }
    }

    pub(crate) async fn cursor_rows(
        &self,
        collection: &str,
        fields: &str,
        filter: &str,
    ) -> Result<Vec<Value>, ConfigClientError> {
        let mut rows = Vec::new();
        let mut cursor = self.cursor_scan(collection, fields, filter);
        while let Some(page) = cursor.next_page().await? {
            rows.extend(page);
        }
        Ok(rows)
    }

    /// Visits authorized pages in this transaction's snapshot. Discard any
    /// external output if the transaction fails or its callback is replayed.
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
        let mut cursor = self.cursor_scan(collection, fields, filter);
        while let Some(page) = cursor.next_page().await.map_err(E::from)? {
            visit(&page)?;
        }
        Ok(())
    }
}
