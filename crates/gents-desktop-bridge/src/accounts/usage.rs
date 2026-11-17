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
    /// The provider's own refusal, when one stands. Beside the windows rather
    /// than among them: a refusal states a fact, not a percentage.
    pub refusal: Option<UsageRefusalView>,
}

/// A standing provider refusal, as the provider stated it. Every field past
/// the timestamp is optional because a refusal establishes only what it says:
/// plain "free usage exhausted" gives no cap, no counter and no reset, and the
/// screen shows those as unknown rather than as 0 or 100%.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageRefusalView {
    pub at: String,
    /// The model the refusal is about. A provider refuses per model, so this is
    /// what the screen says is out, not the whole account.
    pub model: Option<String>,
    /// The window the provider named, e.g. "rolling 24-hour".
    pub window: Option<String>,
    pub used: Option<u64>,
    pub cap: Option<u64>,
    /// Absolute only; a relative "refreshes in 24 hours" leaves this null.
    pub resets_at: Option<String>,
    /// What this node itself observed in the window: an estimate, always, and
    /// `consumed_basis` says so.
    pub consumed_by_node: Option<u64>,
    /// Whose count `consumed_by_node` is: `locally_estimated` always, because it
    /// is this node's arithmetic over its own calls.
    pub consumed_basis: String,
    /// A bounded excerpt of the provider's words, never the whole body.
    pub detail: String,
    /// Whose window `window` names: `provider_reported` when the provider
    /// stated one, `unknown` when it named none and this node asserts none (`window`
    /// is then null),
    /// `locally_estimated` for a window this node chose, which is never
    /// reported as the provider's.
    pub window_provenance: String,
    /// How far back this node looked in its own recorded calls. Always this
    /// node's choice: the provider never said its evidence window is this one.
    pub evidence_window_hours: i64,
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
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    reads: Option<&gents_server::accounts::UsageReads>,
) -> anyhow::Result<Vec<AccountUsageView>> {
    use gents_inference::usage_observation::{account_usage::UsageSource, UsageRead};

    let backends = access
        .transact_readonly("desktop.usage_views", |txn| {
            Box::pin(
                async move { gents_documents::list_inference_backends_in_txn(txn, node_did).await },
            )
        })
        .await?;
    let reads = reads
        .filter(|reads| reads.node_did == node_did)
        .map_or(&[][..], |reads| reads.reads.as_slice());
    let time = |at: chrono::DateTime<chrono::Utc>| at.to_rfc3339();
    let refusal_view =
        |refusal: &gents_inference::usage_observation::UsageRefusalView| UsageRefusalView {
            at: refusal.at.clone(),
            model: refusal.model.clone(),
            window: refusal.window.clone(),
            used: refusal.used,
            cap: refusal.cap,
            resets_at: refusal.resets_at.clone(),
            consumed_by_node: refusal.consumed_by_node,
            consumed_basis: match refusal.consumed_basis {
                gents_inference::usage_observation::WindowBasis::ProviderReported => {
                    "provider_reported".into()
                }
                gents_inference::usage_observation::WindowBasis::Unknown => "unknown".into(),
                gents_inference::usage_observation::WindowBasis::LocallyEstimated => {
                    "locally_estimated".into()
                }
            },
            detail: refusal.detail.clone(),
            // Stated by the view, not inferred from `window.is_some()`: a
            // provider that named no window leaves `window` null rather than
            // borrowing the node's look-back as its own.
            window_provenance: match refusal.window_basis {
                gents_inference::usage_observation::WindowBasis::ProviderReported => {
                    "provider_reported".into()
                }
                // The same word Lean and Rust use: `unknown`, never "unstated".
                gents_inference::usage_observation::WindowBasis::Unknown => "unknown".into(),
                gents_inference::usage_observation::WindowBasis::LocallyEstimated => {
                    "locally_estimated".into()
                }
            },
            evidence_window_hours: refusal.evidence_window_hours,
        };
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
                refusal: read.refusal.as_ref().map(refusal_view),
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
        .map_err(BridgeError::classify_transport_error)?;
    let trigger = if request.refresh {
        gents_inference::usage_observation::UsageTrigger::Refresh
    } else {
        gents_inference::usage_observation::UsageTrigger::Open
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
        Err(error) => return Err(BridgeError::classify_transport_error(error)),
    };
    account_usage_views(&access, node_did, reads.as_ref())
        .await
        .map_err(BridgeError::classify_transport_error)
}
