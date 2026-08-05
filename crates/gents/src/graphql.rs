//! Shared GraphQL utility functions used across Gents runtime modules.
//!
//! Two defenses live here, one per grammatical position:
//! - [`escape_graphql_string`] for values interpolated inside **string
//!   literals** — escaping makes any content safe to embed.
//! - [`validate_graphql_name`] / [`validate_collection_identifier`] for
//!   values interpolated as bare **identifiers** (collection names, field
//!   names). Identifiers sit outside string literals, so escaping cannot
//!   apply; validation against the GraphQL Name grammar is the only defense.
//!
//! A third position — a raw **object/fragment** spliced in whole, as
//! `EventTrigger.filter` is by the trigger engine's filter probe — is
//! covered by neither, and has no defense here yet. See #1038.

use anyhow::{bail, Result};

pub fn escape_graphql_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Validate `name` against the GraphQL `Name` grammar:
/// `[_A-Za-z][_0-9A-Za-z]*` (ASCII only). Anything interpolated into a
/// GraphQL query in identifier position MUST pass this check first —
/// escaping does not exist for identifiers.
pub fn validate_graphql_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        Some(c) => {
            bail!("invalid identifier {name:?}: must start with a letter or underscore, got {c:?}")
        }
        None => bail!("invalid identifier: empty string"),
    }
    if let Some(c) = name
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() && *c != '_')
    {
        bail!("invalid identifier {name:?}: only ASCII letters, digits, and underscore are allowed, got {c:?}");
    }
    Ok(())
}

/// Validate a value used as a **collection name** in identifier position
/// (e.g. `EventTrigger.source_collection`). On top of the Name grammar this
/// rejects the `__` prefix, which the GraphQL spec reserves for
/// introspection — a "collection" of `__Type` or `__schema` would aim the
/// runtime's queries at the introspection surface instead of a document
/// collection.
pub fn validate_collection_identifier(name: &str) -> Result<()> {
    validate_graphql_name(name)?;
    if name.starts_with("__") {
        bail!(
            "invalid collection name {name:?}: the __ prefix is reserved for GraphQL introspection"
        );
    }
    Ok(())
}

pub fn response_has_documents(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(items) => !items.is_empty(),
        serde_json::Value::Object(map) => map.contains_key("_docID"),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
