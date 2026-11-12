use gents_schemas::Collection;
use serde::de::DeserializeOwned;

use crate::document_query::Query;

pub trait View {
    type Collection: Collection;
    type Row: DeserializeOwned;

    fn query(&self, reader_did: &str) -> Query<Self::Collection>;

    fn finish(&self, rows: Vec<Self::Row>) -> Vec<Self::Row> {
        rows
    }
}

impl<V> View for &V
where
    V: View + ?Sized,
{
    type Collection = V::Collection;
    type Row = V::Row;

    fn query(&self, reader_did: &str) -> Query<Self::Collection> {
        V::query(self, reader_did)
    }

    fn finish(&self, rows: Vec<Self::Row>) -> Vec<Self::Row> {
        V::finish(self, rows)
    }
}
