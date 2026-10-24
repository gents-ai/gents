use gents_protocol::graphql::GraphqlError;

use crate::document_config::DocumentConfigError;

#[derive(Debug, thiserror::Error)]
pub enum DefraQueryFailure {
    #[error("filter must be a JSON object")]
    FilterNotObject,
    #[error(transparent)]
    Identifier(#[from] GraphqlError),
    #[error("invalid collection name: {error}")]
    InvalidCollectionName { error: GraphqlError },
    #[error("invalid field name: {error}")]
    InvalidFieldName { error: GraphqlError },
    #[error(transparent)]
    Scope(Box<DocumentConfigError>),
    #[error("collection {collection:?} is not within the allowed query scope: [] (deny-all)")]
    ScopeDenyAll { collection: String },
    #[error("collection {collection:?} is not within the allowed query scope: [{allowed}]")]
    ScopeOutside { collection: String, allowed: String },
    #[error("`fields` must list at least one field to return")]
    FieldsEmpty,
    #[error(
        "wildcard field \"*\" must be the only entry: call with fields: [\"*\"] \
         to list the collection's queryable fields"
    )]
    WildcardNotOnly,
    #[error("field {field:?} on {collection:?} is restricted and cannot be queried")]
    FieldRestricted { field: String, collection: String },
    #[error("field {field:?} on {collection:?} is restricted and cannot be used in a filter")]
    FilterFieldRestricted { field: String, collection: String },
    #[error("field_page.doc_id must be nonempty")]
    FieldPageDocIdEmpty,
    #[error("field_page.field must be in the selected projection")]
    FieldPageFieldNotSelected,
    #[error("field_page.expected_hash is required for continuation")]
    FieldPageHashRequired,
    #[error("field_page document does not match")]
    FieldPageDocMismatch,
    #[error("field_page requires a String value")]
    FieldPageNotString,
    #[error("field_page value changed; restart at offset 0")]
    FieldPageChanged,
    #[error("field_page offset is not a UTF-8 boundary within the value")]
    FieldPageOffset,
    #[error("options.text must be nonempty search terms; call query with argv:[help,search]")]
    SearchTextMissing,
    #[error("options.search_fields is required; call query with argv:[fields] to discover names")]
    SearchFieldsMissing,
    #[error(
        "options.search_fields must be an array of field names; call query with argv:[help,search]"
    )]
    SearchFieldsInvalid(#[source] serde_json::Error),
    #[error(
        "search field {field:?} must be an available String field; call query with argv:[fields]"
    )]
    SearchFieldUnavailable { field: String },
    #[error("_score is reserved for the native BM25 score; omit it from options.fields")]
    ScoreFieldReserved,
    #[error("search limit must be 1–1000; call query with argv:[help,search]")]
    SearchLimit,
    #[error("query renderer omitted {boundary} boundary")]
    RendererBoundaryMissing { boundary: &'static str },
    #[error("unknown query command; call query with {{\"argv\":[\"help\"]}}")]
    UnknownQueryCommand,
    #[error("offset exceeds 100000; use a narrower filter")]
    OffsetTooLarge,
    #[error("order must be an object or array of single-field objects")]
    OrderShape,
    #[error("order must contain 1–16 fields")]
    OrderCount,
    #[error("each order entry must contain one field and ASC or DESC")]
    OrderEntry,
    #[error("order direction must be ASC or DESC")]
    OrderDirection,
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
}

impl From<DocumentConfigError> for DefraQueryFailure {
    fn from(error: DocumentConfigError) -> Self {
        Self::Scope(Box::new(error))
    }
}
