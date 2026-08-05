//! Shared GraphQL utility functions used across Gents runtime modules.
//!
//! Two defenses apply, one per grammatical position:
//! - [`escape_graphql_string`] for values interpolated inside **string
//!   literals** — escaping makes any content safe to embed.
//! - [`validate_graphql_name`] / [`validate_collection_identifier`] for
//!   values interpolated as bare **identifiers** (collection names, field
//!   names, mutation input keys). Identifiers sit outside string literals,
//!   so escaping cannot apply; validation against the GraphQL Name grammar
//!   is the only defense.
//!
//! The identifier validators live in `gents-protocol` so the mutation
//! renderer there shares this crate's definition, and are re-exported here.
//!
//! A third position — a raw **object/fragment** spliced in whole, as
//! `EventTrigger.filter` is by the trigger engine's filter probe — is
//! covered by neither. See #1038.

pub use gents_protocol::graphql::{validate_collection_identifier, validate_graphql_name};

pub fn escape_graphql_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
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
