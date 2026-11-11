//! Callable entry points for the operations `gents_protocol::operations`
//! declares. Front ends call these; the owners below keep the behaviour, and
//! none of them names this module.

pub mod agent;
pub mod mcp_service;
pub mod request;
pub mod session;
pub mod skill;
pub mod tools;
