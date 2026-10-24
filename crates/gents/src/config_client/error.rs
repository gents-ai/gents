#[derive(Debug, thiserror::Error)]
pub enum ConfigClientError {
    #[error("a DefraDB Bearer authorization is required")]
    BearerRequired,
    #[error("invalid authorization header")]
    AuthorizationHeader(#[source] reqwest::header::InvalidHeaderValue),
    #[error("expected GraphQL endpoint ending in /graphql, got {graphql}")]
    GraphqlApiBase { graphql: String },
    #[error("ConfigAccess::write requires a GraphQL mutation document")]
    MutationDocumentRequired,
    #[error("ConfigAccess::execute requires a GraphQL query document")]
    QueryDocumentRequired,
}
