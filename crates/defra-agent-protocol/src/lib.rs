//! Shared substrate for any DefraDB peer participating in a `defra-agent`
//! control plane: GraphQL schema strings, client turn-observation protocol,
//! and serde row mirrors for every replicated collection.

pub mod client_protocol;
pub mod graphql;
pub mod message;
pub mod row;
pub mod schemas;
pub mod transcript;
