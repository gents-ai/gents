use std::sync::Arc;

use anyhow::Result;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use serde_json::Value;

use crate::workspace::WorkspaceBindingDoc;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
