mod agent_behavior;
mod common;
mod event_trigger;
mod inference_backend;
mod schedule;
mod task;
mod tool_selection;

pub(crate) use agent_behavior::write_agent_behavior_document;
pub(crate) use event_trigger::write_event_trigger_document;
pub(crate) use inference_backend::{
    write_inference_backend_document, InferenceBackendUpsertDocument,
};
pub(crate) use schedule::write_schedule_document;
pub(crate) use task::write_task_document;
pub(crate) use tool_selection::write_tool_selection_document;

use anyhow::Result;
use defra_agent::defra_node::EmbeddedNode;
use serde_json::{json, Value};

pub(crate) enum ConfigAccess {
    Graphql(String),
    Local(EmbeddedNode),
}

impl ConfigAccess {
    pub(crate) fn mode(&self) -> &'static str {
        match self {
            Self::Graphql(_) => "graphql",
            Self::Local(_) => "local",
        }
    }

    pub(crate) async fn execute(&self, query: &str) -> Result<Value> {
        match self {
            Self::Graphql(graphql) => crate::post_graphql(graphql, query).await,
            Self::Local(node) => {
                let response = node.execute(query).await;
                if response.has_errors() {
                    anyhow::bail!("graphql returned errors: {:?}", response.errors);
                }
                Ok(json!({
                    "data": response.data.unwrap_or(Value::Null),
                }))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ExistingDocumentRef {
    pub(crate) doc_id: String,
    pub(crate) deleted: bool,
}
