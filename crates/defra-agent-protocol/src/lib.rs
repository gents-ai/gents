//! Shared substrate for any DefraDB peer participating in a `defra-agent`
//! control plane: GraphQL schema strings, client turn-observation protocol,
//! and serde row mirrors for every replicated collection.

pub mod client_protocol;
pub mod graphql;
pub mod message;
pub mod network_token;
pub mod pairing_token;
pub mod row;
pub mod schemas;
pub mod transcript;

pub use pairing_token::{
    decode as decode_invite_token, encode as encode_invite_token,
    signing_payload as invite_token_signing_payload, InviteToken,
    TOKEN_PREFIX as INVITE_TOKEN_PREFIX,
};
