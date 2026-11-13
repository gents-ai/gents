use gents_documents::patch::{SelfConfigPatch, SelfConfigTarget};
use gents_loop::tool::{Tool, ToolDyn};
use gents_protocol::operations::StringUpdate;
use gents_store::graphql::escape_graphql_string;
use gents_tools::builtin::self_config::command;
use gents_tools::builtin::self_config::*;
use serde_json::{json, Value};
use std::collections::BTreeSet;

use gents_protocol::tool_names::CONFIG_TOOL_NAME;
#[path = "self_config/agent_operation_text_tests.rs"]
mod agent_operation_text_tests;
#[path = "self_config/tests.rs"]
mod tests;
#[path = "self_config/text_tests.rs"]
mod text_tests;
