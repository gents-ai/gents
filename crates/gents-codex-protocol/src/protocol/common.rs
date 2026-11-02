use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;

#[macro_use]
mod macros;
mod client_request;
mod fuzzy_file_search;
mod server;

pub use client_request::{ClientRequest, ClientResponse, ClientResponsePayload};
pub use fuzzy_file_search::*;
pub use server::{ServerNotification, ServerRequest, ServerRequestPayload, ServerResponse};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Display)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    ApiKey,
    Chatgpt,
    /// [UNSTABLE] FOR OPENAI INTERNAL USE ONLY - DO NOT USE.
    /// ChatGPT auth tokens are supplied by an external host app and are only
    /// stored in memory. Token refresh must be handled by the external host app.
    #[serde(rename = "chatgptAuthTokens")]
    #[strum(serialize = "chatgptAuthTokens")]
    ChatgptAuthTokens,
    #[serde(rename = "agentIdentity")]
    #[strum(serialize = "agentIdentity")]
    AgentIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientRequestSerializationScope {
    Global(&'static str),
    GlobalSharedRead(&'static str),
    Thread { thread_id: String },
    ThreadPath { path: PathBuf },
    CommandExecProcess { process_id: String },
    Process { process_handle: String },
    FuzzyFileSearchSession { session_id: String },
    FsWatch { watch_id: String },
    McpOauth { server_name: String },
}

client_notification_definitions! {
    Initialized,
}
