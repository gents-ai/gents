use super::{graphql_error_is_retryable, GraphqlError};
use crate::database_error::{GraphqlFailure, TXN_CONFLICT_CODE};
use crate::output::rows::CanonicalRowError;

/// `AccessClassification.Erasure`.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Erasure {
    Typed,
    Anyhow,
    Boxed,
}

fn retryable_as<E>(erasure: Erasure, error: E) -> bool
where
    E: std::error::Error + Send + Sync + 'static,
{
    match erasure {
        Erasure::Typed => graphql_error_is_retryable(&error),
        Erasure::Anyhow | Erasure::Boxed => graphql_error_is_retryable(
            (Box::new(error) as Box<dyn std::error::Error + Send + Sync>).as_ref(),
        ),
    }
}

/// `AccessClassification.protocolRetryWrapped`.
#[test]
fn every_protocol_holder_of_a_graphql_error_keeps_its_retryability() {
    gents_lean_vocab::families::access_classification_protocol_retry_wrapped::replay::<
        Erasure,
        String,
    >(|input| {
        let code = if input.retryable {
            TXN_CONFLICT_CODE
        } else {
            "INTERNAL"
        };
        let failure = GraphqlFailure::new(
            "write",
            serde_json::json!([{"message": "opaque", "extensions": {"code": code}}]),
        );
        match input.wrapper.as_str() {
            "canonical_row_error_graphql" => retryable_as(
                input.erasure,
                CanonicalRowError::Graphql(GraphqlError::Database(failure)),
            ),
            other => panic!(
                "protocol variant {other} has no constructor here; add it with its \
                 `AccessClassification.ProtocolRetryWrapper` constructor"
            ),
        }
    });
}
