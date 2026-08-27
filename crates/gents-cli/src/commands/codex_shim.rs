use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use gents::defra_node::EmbeddedNode;
use gents_codex_protocol as codex;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinHandle;

mod background;
mod bound_behavior;
mod child_stream;
mod command_projection;
mod compaction_projection;
mod compat;
mod continuation_stream;
mod handlers;
mod history_projection;
mod host_runtime;
mod progress;
mod projection_state;
mod protocol;
mod store;
mod subagent_projection;
mod thread_projection;
mod thread_routes;
mod trace;
mod turn;
mod turn_projection;

const JSONRPC_INVALID_REQUEST: i64 = -32600;
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const JSONRPC_INVALID_PARAMS: i64 = -32602;
const JSONRPC_INTERNAL_ERROR: i64 = -32603;

mod server;
mod state;

#[allow(unused_imports)]
pub(crate) use server::{
    bind_codex_shim, resolve_codex_shim_behavior_id, BoundCodexShim, CodexShimBindArgs,
    CodexShimBindError,
};
#[cfg(test)]
use server::{request_is_authorized, validate_bind_security};
#[allow(unused_imports)]
pub(crate) use state::{CodexSidecar, DEFAULT_MEMORY_MODE};
use state::{ConnectionState, Outbound, ShimState, TurnStreamControl};

#[cfg(test)]
mod tests;
