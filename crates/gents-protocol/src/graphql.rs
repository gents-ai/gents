mod error;
mod literals;
mod response;
mod transport;
mod turn_state;
mod validate;

pub use error::GraphqlError;
pub use literals::{
    graphql_bool_literal, graphql_input_literal, graphql_string_list_literal,
    normalize_optional_rfc3339, nullable_string_field, optional_bool_field, optional_f64_field,
    optional_i64_field, optional_i64_list_field, optional_string_field, string_list_field,
};
pub use response::{
    extract_mutation_doc_id, first_graphql_row, graphql_rows_from_response,
    is_collection_missing_error_message,
};
pub use transport::GraphqlRequestOptions;
#[cfg(feature = "native")]
pub use transport::{
    execute_graphql_async, graphql_endpoint_available, graphql_error_is_retryable,
    graphql_transport_error_in_chain,
};
pub use turn_state::{
    parse_session_shape_response, parse_turn_state_response, session_shape_query, turn_state_query,
    GraphqlSessionShape, GraphqlSubmittedRequest, GraphqlTurnState,
};
pub use validate::{
    escape_graphql_string, validate_collection_identifier, validate_graphql_filter_fragment,
    validate_graphql_name,
};

type Result<T, E = GraphqlError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "native"))]
mod tx_tests;
