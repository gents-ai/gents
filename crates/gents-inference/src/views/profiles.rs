use gents_protocol::document_query::{
    fields::inference_profile, Direction, Filter, Query, Selection,
};
use gents_protocol::views::View;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProfileRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    #[serde(flatten)]
    pub profile: gents_documents::InferenceProfile,
}

#[derive(Debug, Clone)]
pub struct ProfileList {
    node_did: String,
    profile_id: Option<String>,
    after: Option<String>,
    doc_ids: Option<Vec<String>>,
    limit: Option<u64>,
}

impl ProfileList {
    pub fn new(node_did: impl Into<String>) -> Self {
        Self {
            node_did: node_did.into(),
            profile_id: None,
            after: None,
            doc_ids: None,
            limit: None,
        }
    }

    pub fn profile_id(mut self, profile_id: impl Into<String>) -> Self {
        self.profile_id = Some(profile_id.into());
        self
    }

    pub fn after(mut self, after: impl Into<String>) -> Self {
        self.after = Some(after.into());
        self
    }

    pub fn doc_ids(mut self, doc_ids: Vec<String>) -> Self {
        self.doc_ids = Some(doc_ids);
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }
}

impl View for ProfileList {
    type Collection = inference_profile::Doc;
    type Row = ProfileRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let selection = Selection::new()
            .field(inference_profile::DOC_ID)
            .field(inference_profile::NODE_DID)
            .field(inference_profile::PROFILE_ID)
            .field(inference_profile::DISPLAY_NAME)
            .field(inference_profile::DESCRIPTION)
            .field(inference_profile::BACKEND_ID)
            .field(inference_profile::MODEL_NAME)
            .field(inference_profile::REASONING_EFFORT)
            .field(inference_profile::SERVICE_TIER)
            .field(inference_profile::CONTEXT_WINDOW)
            .field(inference_profile::MAX_OUTPUT_TOKENS)
            .field(inference_profile::SAMPLING_ID)
            .field(inference_profile::EXECUTION_ID)
            .field(inference_profile::TAGS);
        let mut query = Query::select(selection)
            .filter(Filter::eq(inference_profile::NODE_DID, &self.node_did))
            .order_by(inference_profile::PROFILE_ID, Direction::Asc);
        if let Some(profile_id) = &self.profile_id {
            query = query.filter(Filter::eq(inference_profile::PROFILE_ID, profile_id));
        }
        if let Some(after) = &self.after {
            query = query.filter(Filter::gt(inference_profile::PROFILE_ID, after));
        }
        if let Some(doc_ids) = &self.doc_ids {
            query = query.filter(Filter::any_of(inference_profile::DOC_ID, doc_ids.iter()));
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}
