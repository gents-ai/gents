use gents_protocol::request_admission::AgentRequestCreate;

#[derive(Debug, thiserror::Error)]
pub enum ClientRequestInputError {
    #[error("invalid client request: {0}")]
    Canonical(&'static str),
    #[error("parsing client request mutation")]
    Parse(#[source] Box<query::error::QueryError>),
    #[error("client request is not a create mutation")]
    NotMutation,
    #[error("client request must create exactly one document")]
    NotSingleRequestCreate,
    #[error("serializing client request input")]
    Encode(#[from] serde_json::Error),
    #[error("client request input is not an object")]
    NotObject,
}

/// Translate the canonical request owner's GraphQL input into the native
/// signed-create adapter's variables without constructing another wire shape.
pub fn client_request_input(
    create: &AgentRequestCreate,
) -> Result<serde_json::Value, ClientRequestInputError> {
    let document = create
        .graphql_client_publication_mutation()
        .map_err(ClientRequestInputError::Canonical)?;
    let parsed = query::query_parse::parse_request_with_limits(
        &document,
        None,
        None,
        query::QueryLimits::default(),
    )
    .map_err(|error| ClientRequestInputError::Parse(Box::new(error)))?;
    let query::query_parse::ParsedOperation::Mutation {
        mutations,
        explain: None,
    } = parsed
    else {
        return Err(ClientRequestInputError::NotMutation);
    };
    if mutations.len() != 1
        || mutations[0].collection_name != "AgentRequest"
        || mutations[0].mutation_type != query::MutationType::Add
        || mutations[0].create_input.len() != 1
    {
        return Err(ClientRequestInputError::NotSingleRequestCreate);
    }
    let mut input = serde_json::to_value(&mutations[0].create_input[0])?;
    input
        .as_object_mut()
        .ok_or(ClientRequestInputError::NotObject)?
        .insert("relationships_pending".into(), false.into());
    Ok(input)
}
