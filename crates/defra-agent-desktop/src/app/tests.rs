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
use crate::state::{Activity, ManageDraft, ManageSection};
use crate::telemetry::{global_log_layer, global_log_store, DesktopLogStore};

#[path = "tests/support/live_fixture/mod.rs"]
mod support_live_fixture;
use support_live_fixture::*;

#[path = "tests/support/seed/mod.rs"]
mod support_seed;
use support_seed::*;

include!("tests/support/chat_flow.rs");
include!("tests/support/manage_flow.rs");
include!("tests/support/bootstrap_runtime.rs");
include!("tests/support/network.rs");
include!("tests/support/wait.rs");
include!("tests/support/driver.rs");

#[path = "tests/support/mock_backend/mod.rs"]
mod support_mock_backend;
use support_mock_backend::*;

#[path = "tests/support/response_wait/mod.rs"]
mod support_response_wait;
use support_response_wait::*;

mod bootstrap;
mod chat;
mod first_launch;
mod live;
mod manage;
mod setup;
