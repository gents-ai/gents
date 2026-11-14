use anyhow::Result;
use chrono::Utc;
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents_tools::native::SessionHistorySnapshot as CanonicalSessionHistorySnapshot;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionHistorySnapshot {
    pub generated_at: String,
    #[serde(flatten)]
    pub history: CanonicalSessionHistorySnapshot,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionHistoryParams {
    pub limit: Option<usize>,
}

pub async fn load_session_history_snapshot(
    graphql: &GraphqlEndpoint,
    node_did: &str,
    limit: Option<usize>,
) -> Result<SessionHistorySnapshot> {
    let history = gents_tools::native::load_session_history_snapshot_with_access(
        &ConfigAccess::Graphql(graphql.clone()),
        node_did,
        Some(limit.unwrap_or(10).clamp(1, 50)),
    )
    .await?;
    Ok(SessionHistorySnapshot {
        generated_at: Utc::now().to_rfc3339(),
        history,
    })
}
