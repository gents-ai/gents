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
use eframe::App as _;
use rig::completion::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    UserContent,
};
use rig::one_or_many::OneOrMany;
use serde_json::Value;
use tokio::sync::watch;
use tracing_subscriber::{prelude::*, EnvFilter};

use crate::audit;
use crate::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use crate::state::{Activity, LogsFilter, OperatorDraft, OperatorSection};
use crate::telemetry::{global_log_layer, global_log_store, DesktopLogCategory, DesktopLogStore};

include!("tests/support/seed.rs");
include!("tests/support/fixture.rs");
include!("tests/support/chat_flow.rs");
include!("tests/support/operator_flow.rs");
include!("tests/support/bootstrap_runtime.rs");
include!("tests/support/network.rs");
include!("tests/support/mock_backend.rs");
include!("tests/support/response_wait.rs");
include!("tests/support/wait.rs");
include!("tests/support/driver.rs");

mod bootstrap;
mod chat;
mod first_launch;
mod live;
mod peers;
