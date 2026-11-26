#[derive(Debug, thiserror::Error)]
pub enum GraphqlError {
    #[error("invalid identifier {name:?}: must start with a letter or underscore, got {first:?}")]
    IdentifierStart { name: String, first: char },
    #[error("invalid identifier: empty string")]
    EmptyIdentifier,
    #[error("invalid identifier {name:?}: only ASCII letters, digits, and underscore are allowed, got {character:?}")]
    IdentifierCharacter { name: String, character: char },
    #[error(
        "invalid collection name {name:?}: the __ prefix is reserved for GraphQL introspection"
    )]
    ReservedCollectionPrefix { name: String },
    #[error("invalid filter: empty")]
    EmptyFilter,
    #[error("invalid filter: must be an object literal")]
    FilterNotObject,
    #[error("invalid filter: object keys are unquoted GraphQL names, e.g. {{kind: {{_eq: \"review\"}}}}")]
    FilterQuotedKey,
    #[error("invalid filter: trailing content after the object literal")]
    FilterTrailingContent,
    #[error("invalid filter: unbalanced {character:?} at byte {byte}")]
    FilterUnbalanced { character: char, byte: usize },
    #[error("invalid filter: mismatched {opener:?} closed by {closer:?} at byte {byte}")]
    FilterMismatched {
        opener: char,
        closer: char,
        byte: usize,
    },
    #[error(
        "invalid filter: character {character:?} at byte {byte} is not allowed in a filter object"
    )]
    FilterCharacter { character: char, byte: usize },
    #[error("invalid filter: unterminated string literal")]
    FilterUnterminatedString,
    #[error("invalid filter: unclosed {opener:?}")]
    FilterUnclosed { opener: Option<char> },
    #[error(
        "invalid filter: unknown operator {operator:?}; supported operators: _eq, _neq, _gt, _geq, _lt, _leq, _in, _nin, _like, _nlike, _ilike, _nilike, _and, _or, _not (for prefix or pattern matching use _like with % wildcards, e.g. {{field: {{_like: \"prefix-%\"}}}})"
    )]
    FilterUnknownOperator { operator: String },
    #[error("invalid filter: {message}")]
    FilterSyntax { message: &'static str },
    #[cfg(feature = "native")]
    #[error("encoding Authorization header")]
    AuthorizationHeader(#[source] reqwest::header::InvalidHeaderValue),
    #[cfg(feature = "native")]
    #[error(transparent)]
    ClientTask(#[from] tokio::task::JoinError),
    #[cfg(feature = "native")]
    #[error(transparent)]
    ClientBuild(reqwest::Error),
    #[cfg(feature = "native")]
    #[error("posting GraphQL to {graphql}")]
    Post {
        graphql: String,
        #[source]
        source: reqwest::Error,
    },
    #[cfg(feature = "native")]
    #[error("reading GraphQL response from {graphql}")]
    ReadResponse {
        graphql: String,
        #[source]
        source: reqwest::Error,
    },
    /// A 401 or 403, with the `code` its body named, so a client can tell
    /// the edge of a Vera home asking for a delegation from any refusal.
    #[cfg(feature = "native")]
    #[error("reading GraphQL response from {graphql}")]
    Refused {
        graphql: String,
        #[source]
        source: reqwest::Error,
        code: Option<String>,
    },
    #[cfg(feature = "native")]
    #[error("decoding GraphQL response body from {graphql}")]
    DecodeBody {
        graphql: String,
        #[source]
        source: reqwest::Error,
    },
    /// A body that read whole but did not inflate or parse as JSON.
    #[cfg(feature = "native")]
    #[error("decoding GraphQL response body from {graphql}")]
    DecodeContent {
        graphql: String,
        #[source]
        source: std::io::Error,
    },
    #[cfg(feature = "native")]
    #[error("GraphQL request retries exhausted for {graphql}")]
    RetriesExhausted { graphql: String },
    #[cfg(feature = "native")]
    #[error("GraphQL read transport requires a query document")]
    QueryDocumentRequired,
    #[cfg(feature = "native")]
    #[error(transparent)]
    Database(#[from] crate::database_error::GraphqlFailure),
    #[error("graphql response missing data: {response}")]
    MissingData { response: String },
    #[error("graphql mutation returned no _docID for {collection}: {response}")]
    MutationDocId {
        collection: String,
        response: String,
    },
    #[error("graphql returned no rows for {collection}")]
    NoRows { collection: String },
    #[error("parsing RFC3339 timestamp {raw}")]
    Rfc3339 {
        raw: String,
        #[source]
        source: chrono::ParseError,
    },
}

impl GraphqlError {
    #[cfg(feature = "native")]
    pub fn transport_error(&self) -> Option<&reqwest::Error> {
        match self {
            Self::ClientBuild(source)
            | Self::Post { source, .. }
            | Self::ReadResponse { source, .. }
            | Self::Refused { source, .. }
            | Self::DecodeBody { source, .. } => Some(source),
            _ => None,
        }
    }
    /// The `code` the body of a 401 or 403 named.
    #[cfg(feature = "native")]
    pub fn refusal_code(&self) -> Option<(reqwest::StatusCode, &str)> {
        match self {
            Self::Refused {
                source,
                code: Some(code),
                ..
            } => Some((source.status()?, code)),
            _ => None,
        }
    }

    /// The GraphQL errors DefraDB answered with, which its transparent
    /// variant hides from `source()`.
    pub fn database_failure(&self) -> Option<&crate::database_error::GraphqlFailure> {
        match self {
            #[cfg(feature = "native")]
            Self::Database(failure) => Some(failure),
            _ => None,
        }
    }
}
