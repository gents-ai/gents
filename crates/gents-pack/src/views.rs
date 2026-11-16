use gents_protocol::document_query::{
    fields::pack_installation as fields, Direction, Filter, Query, Selection,
};
use gents_protocol::views::View;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct InstalledPackRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub coordinate: Option<String>,
    pub version: Option<String>,
    pub digest: Option<String>,
    pub source: Option<crate::pack::PackSource>,
    pub plugin_store: Option<bool>,
}

pub struct InstalledPacks {
    owner: String,
    coordinate: Option<String>,
}

impl InstalledPacks {
    pub fn new(owner: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            coordinate: None,
        }
    }

    pub fn coordinate(mut self, coordinate: impl Into<String>) -> Self {
        self.coordinate = Some(coordinate.into());
        self
    }
}

impl View for InstalledPacks {
    type Collection = fields::Doc;
    type Row = InstalledPackRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let query = Query::select(
            Selection::doc_id()
                .field(fields::COORDINATE)
                .field(fields::VERSION)
                .field(fields::DIGEST)
                .field(fields::SOURCE)
                .field(fields::PLUGIN_STORE),
        )
        .filter(Filter::eq(fields::NODE_DID, &self.owner));
        match &self.coordinate {
            Some(coordinate) => query
                .filter(Filter::eq(fields::COORDINATE, coordinate))
                .limit(2),
            None => query.order_by(fields::COORDINATE, Direction::Asc),
        }
    }
}
