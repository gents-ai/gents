use crate::collection::Collection;
use crate::AgentDocument as Agent;

use super::desired_state::DesiredStateError;
#[cfg(test)]
use super::ConfigApplyTxn;
use super::{
    ConfigAccess, ConfigClientError, DesiredStateApplyDocument, DesiredStateApplyPlan,
    TransactionFailure,
};

#[derive(Debug, thiserror::Error)]
pub enum AgentWriteError {
    #[error("replaced Agent missing")]
    Missing,
    #[cfg(test)]
    #[error("decoding canonical Agent")]
    Decode(#[source] serde_json::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    DesiredState(#[from] DesiredStateError),
    #[error(transparent)]
    Access(#[from] ConfigClientError),
}

impl TransactionFailure for AgentWriteError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::DesiredState(error) => error.config_client_error(),
            _ => None,
        }
    }
}

type Result<T, E = AgentWriteError> = std::result::Result<T, E>;

pub async fn write_agent_document(access: &ConfigAccess, behavior: &Agent) -> Result<String> {
    access
        .transact_write("config.agent.write", |txn| {
            Box::pin(async move {
                let value = serde_json::to_value(behavior)?;
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: Collection::Agent,
                    add: value.clone(),
                    update: value,
                }])?;
                super::apply_desired_state_plan(txn, &plan).await?;
                super::desired_state::read_record(
                    txn,
                    Collection::Agent,
                    &behavior.node_did,
                    &behavior.agent_id,
                )
                .await?
                .map(|(doc_id, _)| doc_id)
                .ok_or(AgentWriteError::Missing)
            })
        })
        .await
}

#[cfg(test)]
async fn load_agent_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    agent_id: &str,
) -> Result<Option<Agent>> {
    super::read_desired_state_document_in_txn(txn, Collection::Agent, node_did, agent_id)
        .await?
        .map(serde_json::from_value)
        .transpose()
        .map_err(AgentWriteError::Decode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_client::write_inference_profile_document;
    use crate::document_config::InferenceProfile;

    use serde_json::json;
    use std::sync::Arc;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

    #[tokio::test]
    async fn replacement_is_scoped_clears_context_and_rolls_back_dangling_links() -> TestResult {
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::local(node.clone());
        for owner in ["did:key:owner", "did:key:other"] {
            let backend = json!({"node_did":owner,"backend_id":"local","name":"Local",
                "provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1",
                "auth":{"kind":"unauthenticated"}});
            let context =
                json!({"node_did":owner,"context_id":"context","system_prompt":"literal"});
            let plan = DesiredStateApplyPlan::new(vec![
                DesiredStateApplyDocument {
                    collection: Collection::InferenceBackend,
                    add: backend.clone(),
                    update: backend,
                },
                DesiredStateApplyDocument {
                    collection: Collection::AgentContext,
                    add: context.clone(),
                    update: context,
                },
            ])?;
            access
                .transact_write("test.behavior.seed", |txn| {
                    let plan = &plan;
                    Box::pin(async move {
                        super::super::apply_desired_state_plan(txn, plan)
                            .await
                            .map(|_| ())
                    })
                })
                .await?;
            let profile: InferenceProfile = serde_json::from_value(
                json!({"node_did":owner,"profile_id":"inference","backend_id":"local","model_name":"exact-model"}),
            )?;
            write_inference_profile_document(&access, &profile).await?;
        }
        let mut behavior: Agent = serde_json::from_value(json!({
            "node_did":"did:key:owner","agent_id":"same","inference_profile_id":"inference",
            "context_id":"context","description":"old","tags":["old"]
        }))?;
        let original_id = write_agent_document(&access, &behavior).await?;
        let mut foreign = behavior.clone();
        foreign.node_did = "did:key:other".to_owned();
        let foreign_id = write_agent_document(&access, &foreign).await?;
        assert_ne!(original_id, foreign_id);
        behavior.context_id = None;
        behavior.description = None;
        behavior.tags.clear();
        assert_eq!(write_agent_document(&access, &behavior).await?, original_id);
        access
            .transact_write("test.behavior.read", |txn| {
                let behavior = &behavior;
                let foreign = &foreign;
                Box::pin(async move {
                    let actual = load_agent_in_txn(txn, &behavior.node_did, &behavior.agent_id)
                        .await?
                        .unwrap();
                    assert_eq!(actual, *behavior);
                    let actual_foreign =
                        load_agent_in_txn(txn, &foreign.node_did, &foreign.agent_id)
                            .await?
                            .unwrap();
                    assert_eq!(actual_foreign, *foreign);
                    Ok::<_, AgentWriteError>(())
                })
            })
            .await?;
        let mut invalid = behavior.clone();
        invalid.context_id = Some("missing".to_owned());
        assert!(write_agent_document(&access, &invalid).await.is_err());
        access
            .transact_write("test.behavior.rollback", |txn| {
                let behavior = &behavior;
                Box::pin(async move {
                    assert_eq!(
                        load_agent_in_txn(txn, &behavior.node_did, &behavior.agent_id)
                            .await?
                            .as_ref(),
                        Some(behavior)
                    );
                    Ok::<_, AgentWriteError>(())
                })
            })
            .await?;
        Ok(())
    }
}
