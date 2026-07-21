use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::hook::BackgroundExecutionRegistry;
use crate::interrupt::interrupt_request;
use crate::tool_call_lifecycle::{AwaitMode, CancelCause, CascadeDispatch, ToolCallLifecycle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelBackgroundToolCallOutcome {
    Cancelled { live_execution_cancelled: bool },
    AlreadyTerminal { state: String },
    NotBackground,
    NotFound,
}

pub async fn cancel_background_tool_call(
    node: Arc<EmbeddedNode>,
    background_executions: &BackgroundExecutionRegistry,
    agent_did: &str,
    session_id: &str,
    tool_call_id: &str,
) -> Result<CancelBackgroundToolCallOutcome> {
    let Some(mut lifecycle) =
        ToolCallLifecycle::load(node.clone(), session_id, tool_call_id).await?
    else {
        return Ok(CancelBackgroundToolCallOutcome::NotFound);
    };

    if lifecycle.await_mode() != AwaitMode::Background {
        return Ok(CancelBackgroundToolCallOutcome::NotBackground);
    }
    if lifecycle.is_terminal() {
        return Ok(CancelBackgroundToolCallOutcome::AlreadyTerminal {
            state: lifecycle.state().as_str().to_string(),
        });
    }

    let live_execution_cancelled = background_executions.cancel(tool_call_id).await;
    let dispatch = lifecycle
        .cancel_during_run_with_cascade_dispatch(CancelCause::UserCancelled, agent_did)
        .await?;

    if let Some(CascadeDispatch::Local(intent)) = dispatch {
        interrupt_request(node.as_ref(), &intent.child_request_id).await?;
    }

    if lifecycle.is_cancelled() {
        Ok(CancelBackgroundToolCallOutcome::Cancelled {
            live_execution_cancelled,
        })
    } else {
        Ok(CancelBackgroundToolCallOutcome::AlreadyTerminal {
            state: lifecycle.state().as_str().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ensure_schemas;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn cancel_background_tool_call_terminalizes_row_and_token() {
        let data_path = std::env::temp_dir().join(format!(
            "agent-tool-control-cancel-{}",
            uuid::Uuid::new_v4()
        ));
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .data_path(&data_path)
                .build()
                .await
                .unwrap(),
        );
        ensure_schemas(&node).await.unwrap();

        let registry = BackgroundExecutionRegistry::default();
        let token = CancellationToken::new();
        registry.insert("tool-1".to_string(), token.clone()).await;

        let mut lifecycle = ToolCallLifecycle::new_background_tool(
            node.clone(),
            "request-1".to_string(),
            "session-1".to_string(),
            "did:defra-agent:test".to_string(),
            "tool-1".to_string(),
            1,
            "bash_unrestricted".to_string(),
            "{}".to_string(),
            chrono::Utc::now() + chrono::Duration::minutes(5),
        );
        lifecycle.start_running().await.unwrap();

        let outcome = cancel_background_tool_call(
            node.clone(),
            &registry,
            "did:defra-agent:test",
            "session-1",
            "tool-1",
        )
        .await
        .unwrap();

        assert_eq!(
            outcome,
            CancelBackgroundToolCallOutcome::Cancelled {
                live_execution_cancelled: true
            }
        );
        assert!(token.is_cancelled());

        let row = ToolCallLifecycle::load(node.clone(), "session-1", "tool-1")
            .await
            .unwrap()
            .expect("tool row");
        assert!(row.is_cancelled());

        let _ = std::fs::remove_dir_all(&data_path);
    }
}
