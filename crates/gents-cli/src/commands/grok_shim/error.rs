use gents::config_client::ConfigClientError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum GrokShimError {
    #[error(transparent)]
    Graphql(#[from] ConfigClientError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("session ID must not be empty")]
    SessionIdEmpty,
    #[error("session history requires a node DID")]
    NodeDidRequired,
    #[error("missing session owner rows")]
    MissingOwnerRows,
    #[error("missing session request rows")]
    MissingRequestRows,
    #[error("missing history session owners")]
    MissingHistoryOwners,
    #[error("session request pagination did not advance")]
    PaginationDidNotAdvance,
    #[error("history request lacks physical identity")]
    RequestWithoutDocId,
    #[error("session is not readable by this bound agent")]
    SessionNotReadable,
    #[error("legacy session has no authorized request history")]
    LegacySessionWithoutHistory,
    #[error("context accounting is unavailable for this session's persisted inference calls")]
    ContextAccountingUnavailable,
    #[error("session list limit must be positive")]
    SessionListLimitZero,
    #[error("unknown headless policy")]
    UnknownHeadlessPolicy,
    #[error("facetFilters must be an object")]
    FacetFiltersNotObject,
    #[error("unsupported session facet filter")]
    UnsupportedFacetFilter,
    #[error("kind must be a string or string array")]
    InvalidKindFacet,
    #[error("invalid session cursor")]
    InvalidSessionCursor(#[source] serde_json::Error),
    #[error("session cursor belongs to another search")]
    SessionCursorOtherSearch,
}
