use super::*;

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    default_behavior_id_for_agent, ensure_agent_principal, load_agent_behavior,
    upsert_agent_behavior, AgentIdentity, BackendProviderKind, DefraAgent, DocumentRuntimeOptions,
    SimpleIdentity, ToolCeiling,
};
use defra_agent_protocol::row::{
    AgentBehaviorRow, InferenceBackendRow, InferenceProfileRow, ScheduledTaskRow, ToolSelectionRow,
};
use defra_agent_protocol::schemas::{ALL_COLLECTION_NAMES, RUNTIME_COLLECTION_NAMES};
use eframe::App as _;
use serde_json::Value;
use tokio::sync::watch;
use tracing_subscriber::{prelude::*, EnvFilter};

use crate::audit;
use crate::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use crate::state::{LogsFilter, OperatorDraft, OperatorSection};
use crate::telemetry::{global_log_layer, global_log_store, DesktopLogCategory, DesktopLogStore};

mod coverage;
include!("tests/support.rs");

mod chat;
mod first_launch;
mod live;
mod peers;
