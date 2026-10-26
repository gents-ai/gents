use crate::BridgeError;
use gents_desktop_core::client::ClientCore;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageReadRequest {
    pub node_did: String,
    pub refresh: bool,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsageView {
    pub label: String,
    pub provider: String,
    pub backend_ids: Vec<String>,
    pub windows: Vec<UsageWindowView>,
    pub plan: Option<String>,
    pub note: Option<String>,
    pub read_at: Option<String>,
    pub read_error: Option<String>,
    pub read: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindowView {
    pub label: String,
    pub window_minutes: Option<i64>,
    pub used_pct: f64,
    pub resets_at: Option<String>,
    pub source: String,
    pub observed_at: String,
    /// Older than the staleness bound: the last known value.
    pub last_known: bool,
}

pub(crate) async fn account_usage_views(
    access: &gents::ConfigAccess,
    node_did: &str,
    reads: Option<&gents_server::accounts::UsageReads>,
) -> anyhow::Result<Vec<AccountUsageView>> {
    use gents::usage_observation::{account_usage::UsageSource, UsageRead};

    let backends = access
        .transact_readonly("desktop.usage_views", |txn| {
            Box::pin(async move {
                gents::config_client::list_inference_backends_in_txn(txn, node_did).await
            })
        })
        .await?;
    let reads = reads
        .filter(|reads| reads.node_did == node_did)
        .map_or(&[][..], |reads| reads.reads.as_slice());
    let time = |at: chrono::DateTime<chrono::Utc>| at.to_rfc3339();
    let views = reads
        .iter()
        .map(|read| {
            let usage = read.usage.as_ref();
            AccountUsageView {
                label: read.label.clone(),
                provider: read.provider.clone(),
                backend_ids: backends
                    .iter()
                    .filter(|backend| read.matches_backend(backend))
                    .map(|backend| backend.backend_id.clone())
                    .collect(),
                windows: usage
                    .into_iter()
                    .flat_map(|usage| &usage.windows)
                    .map(|window| UsageWindowView {
                        label: window.label.clone(),
                        window_minutes: window.window_minutes,
                        used_pct: window.used_pct,
                        resets_at: window.resets_at.map(time),
                        source: match window.source {
                            UsageSource::Header => "header",
                            UsageSource::Endpoint => "endpoint",
                            UsageSource::Error => "error",
                        }
                        .into(),
                        observed_at: time(window.observed_at),
                        last_known: window.last_known,
                    })
                    .collect(),
                plan: usage.and_then(|usage| usage.plan.clone()),
                note: usage.map_or_else(|| Some("unknown".into()), |usage| usage.note.clone()),
                read_at: usage.and_then(|usage| usage.read_at.map(time)),
                read_error: usage.and_then(|usage| usage.read_error.clone()),
                read: Some(match &read.outcome {
                    UsageRead::Read => "read".into(),
                    UsageRead::SkippedRecent => "skipped_recent".into(),
                    UsageRead::NotReported => "not_reported".into(),
                    UsageRead::Disabled => "disabled".into(),
                    UsageRead::Unavailable(reason) => format!("unavailable: {reason}"),
                }),
            }
        })
        .collect();
    Ok(views)
}

pub async fn read(
    core: &ClientCore,
    request: ProviderUsageReadRequest,
) -> Result<Vec<AccountUsageView>, BridgeError> {
    let node_did = request.node_did.trim();
    let access = core
        .operator_access(node_did)
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let trigger = if request.refresh {
        gents::usage_observation::UsageTrigger::Refresh
    } else {
        gents::usage_observation::UsageTrigger::Open
    };
    let reads = async {
        let signer = core.operator_signer(node_did)?;
        let graphql = core
            .operator_graphql(node_did)
            .ok_or_else(|| anyhow::anyhow!("the node has no operator endpoint"))?;
        gents_server::accounts::request_usage_reads(
            signer.as_ref(),
            &graphql,
            trigger,
            request.provider.as_deref(),
        )
        .await
    }
    .await;
    let reads = match reads {
        Ok(reads) => Some(reads),
        Err(error) if !request.refresh => {
            tracing::warn!(
                target: "gents_desktop_bridge::inference_setup",
                error = %format!("{error:#}"),
                "usage read on open failed; limits unavailable"
            );
            None
        }
        Err(error) => return Err(BridgeError::untyped(error.to_string())),
    };
    account_usage_views(&access, node_did, reads.as_ref())
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}
