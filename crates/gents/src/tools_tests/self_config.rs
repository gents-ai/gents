use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use gents_tools::builtin::self_config::*;
use gents_tools::surface::SelfConfigToolConfig;
use defra_node::Node as EmbeddedNode;
use gents_documents::patch::{SelfConfigPatch, SelfConfigTarget};
use gents_loop::tool::{Tool, ToolDefinition, ToolDyn};
use gents_protocol::operations::StringUpdate;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;

use gents_tools::builtin::self_config::{agent_management, command};

mod agent_creation_native_tests;
mod agent_operation_text_tests;
mod policy_tests;
mod tests;
mod text_tests;
