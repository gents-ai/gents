#[derive(Debug, thiserror::Error)]
pub(crate) enum GraphqlError {
    #[error("{field} must not be empty")]
    EmptyField { field: &'static str },
}

pub(super) fn normalize_required<'a>(
    field: &'static str,
    value: &'a str,
) -> Result<&'a str, GraphqlError> {
    normalize_optional_string(Some(value)).ok_or(GraphqlError::EmptyField { field })
}

pub(super) fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

pub(super) use gents_protocol::graphql::escape_graphql_string;
