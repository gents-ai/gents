//! Codex app-server JSON-RPC wire vocabulary used by the Gents shim.
//!
//! These types track the workspace's pinned Codex revision. Only transport DTOs
//! live here; upstream server helpers and schema generators are deliberately
//! excluded from the shipped Gents dependency graph.

mod core_types;
mod jsonrpc_lite;
mod protocol;

pub use core_types::{AbsolutePathBuf, GitSha, MessagePhase};
pub use jsonrpc_lite::*;
pub use protocol::common::*;
pub use protocol::v1::ClientInfo;
pub use protocol::v1::GetAuthStatusResponse;
pub use protocol::v1::GetConversationSummaryParams;
pub use protocol::v1::GetConversationSummaryResponse;
pub use protocol::v1::GitDiffToRemoteParams;
pub use protocol::v1::GitDiffToRemoteResponse;
pub use protocol::v1::InitializeCapabilities;
pub use protocol::v1::InitializeParams;
pub use protocol::v1::InitializeResponse;
pub use protocol::v2::*;
