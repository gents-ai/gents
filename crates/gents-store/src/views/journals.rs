use gents_protocol::document_query::{Filter, Query, Selection};
use gents_protocol::views::View;
use gents_schemas::fields::pending_chat_submission;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct PendingSubmissionRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub payload: String,
}

pub struct PendingSubmission<'a> {
    pub request_id: &'a str,
}

impl View for PendingSubmission<'_> {
    type Collection = pending_chat_submission::Doc;
    type Row = PendingSubmissionRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        Query::select(
            Selection::new()
                .field(pending_chat_submission::DOC_ID)
                .field(pending_chat_submission::PAYLOAD),
        )
        .filter(Filter::eq(
            pending_chat_submission::REQUEST_ID,
            self.request_id,
        ))
        .limit(2)
    }
}
