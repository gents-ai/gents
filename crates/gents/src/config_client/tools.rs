use crate::collection::Collection;
use crate::document_config::{DocumentConfigError, Tools};

use super::desired_state::DesiredStateError;
use super::{
    ConfigAccess, ConfigClientError, DesiredStateApplyDocument, DesiredStateApplyPlan,
    TransactionFailure,
};

#[derive(Debug, thiserror::Error)]
pub enum ToolsWriteError {
    #[error("replaced Tools document missing")]
    Missing,
    #[error(transparent)]
    Invalid(Box<DocumentConfigError>),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    DesiredState(#[from] DesiredStateError),
    #[error(transparent)]
    Access(#[from] ConfigClientError),
}

impl From<DocumentConfigError> for ToolsWriteError {
    fn from(error: DocumentConfigError) -> Self {
        Self::Invalid(Box::new(error))
    }
}

impl TransactionFailure for ToolsWriteError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::DesiredState(error) => error.config_client_error(),
            Self::Invalid(error) => error.config_client_error(),
            _ => None,
        }
    }
}

type Result<T, E = ToolsWriteError> = std::result::Result<T, E>;

/// Replace a complete Tools document; omitted groups revoke their capabilities.
pub async fn write_tools_document(access: &ConfigAccess, tools: &Tools) -> Result<String> {
    tools.validate()?;
    access
        .transact_write("config.tools.write", |txn| {
            Box::pin(async move {
                let value = serde_json::to_value(tools)?;
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: Collection::Tools,
                    add: value.clone(),
                    update: value,
                }])?;
                super::apply_desired_state_plan(txn, &plan).await?;
                super::desired_state::read_record(
                    txn,
                    Collection::Tools,
                    &tools.node_did,
                    &tools.tools_id,
                )
                .await?
                .map(|(doc_id, _)| doc_id)
                .ok_or(ToolsWriteError::Missing)
            })
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use std::sync::Arc;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

    #[tokio::test]
    async fn replacement_revokes_omitted_groups_without_changing_other_owner() -> TestResult {
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::local(node);
        let mut tools: Tools = serde_json::from_value(json!({
            "node_did":"did:key:owner", "tools_id":"same",
            "host":{"files":{"mode":"ReadOnly"}},
            "built_ins":{"enable_goal_tools":true}, "tags":["old"]
        }))?;
        let first_id = write_tools_document(&access, &tools).await?;
        let mut foreign = tools.clone();
        foreign.node_did = "did:key:other".into();
        assert_ne!(write_tools_document(&access, &foreign).await?, first_id);
        tools.host = None;
        tools.built_ins = None;
        tools.tags.clear();
        assert_eq!(write_tools_document(&access, &tools).await?, first_id);
        access
            .transact_write("test.tools.read", |txn| {
                let tools = &tools;
                let foreign = &foreign;
                Box::pin(async move {
                    for (owner, expected) in [("did:key:owner", tools), ("did:key:other", foreign)]
                    {
                        let (_, value) = super::super::desired_state::read_record(
                            txn,
                            Collection::Tools,
                            owner,
                            "same",
                        )
                        .await?
                        .ok_or("missing tools")?;
                        assert_eq!(serde_json::from_value::<Tools>(value)?, *expected);
                    }
                    Ok(())
                })
            })
            .await
    }
}
