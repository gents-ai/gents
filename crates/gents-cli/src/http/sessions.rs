use anyhow::Result;
use chrono::Utc;
use gents::config_client::{ConfigAccess, GraphqlEndpoint};
use gents::toolset::SessionHistorySnapshot as CanonicalSessionHistorySnapshot;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SessionHistorySnapshot {
    pub(crate) generated_at: String,
    #[serde(flatten)]
    pub(crate) history: CanonicalSessionHistorySnapshot,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct SessionHistoryParams {
    pub(crate) limit: Option<usize>,
}

pub(crate) async fn load_session_history_snapshot(
    graphql: &GraphqlEndpoint,
    node_did: &str,
    limit: Option<usize>,
) -> Result<SessionHistorySnapshot> {
    let history = gents::toolset::load_session_history_snapshot_with_access(
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
