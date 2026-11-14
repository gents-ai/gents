use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};

use gents_protocol::operations::StringUpdate;
use gents_store::graphql::escape_graphql_string;
use gents_tools::builtin::self_config::*;

use gents_tools::builtin::self_config::agent_management;

mod agent_creation_native_tests;

mod tests;
mod text_tests;

mod fixture;
