#[derive(Debug, thiserror::Error)]
pub(crate) enum GrokShimError {
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
