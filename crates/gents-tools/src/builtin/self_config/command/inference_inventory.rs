use super::*;
use gents_inference::views::backends::{read_inventory_in_txn, BackendList, InventoryRow};
use gents_inference::views::profiles::ProfileList;
use gents_inference::views::settings::{ExecutionList, RetryPolicyList, SamplingList};
use gents_session::session::output_render::ordered;

impl ConfigCommandTool {
    pub(super) async fn typed_inference_inventory(
        &self,
        target: SelfConfigTarget,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<String> {
        let (collection, id_field) = match target {
            SelfConfigTarget::InferenceBackend => ("InferenceBackendMetadata", "backend_id"),
            SelfConfigTarget::InferenceProfile => ("InferenceProfile", "profile_id"),
            SelfConfigTarget::InferenceSampling => ("InferenceSampling", "sampling_id"),
            SelfConfigTarget::InferenceExecution => ("InferenceExecution", "execution_id"),
            SelfConfigTarget::InferenceRetryPolicy => ("InferenceRetryPolicy", "retry_policy_id"),
            _ => bail!("expected an inference inventory resource"),
        };
        let (mut selected, total) =
            gents_store::config_client::ConfigAccess::transact_local_readonly(
                &self.node,
                Some(self.core.identity()?),
                "self_config.inference_inventory",
                |txn| {
                    Box::pin(async move {
                        macro_rules! page {
                            ($view:expr, $field:ident) => {{
                                let total = txn.read_view(&$view).await?.len();
                                let mut page = $view.limit((limit + 1) as u64);
                                if let Some(cursor) = cursor {
                                    page = page.after(cursor);
                                }
                                let rows = txn
                                    .read_view(&page)
                                    .await?
                                    .into_iter()
                                    .map(|row| serde_json::to_value(row.$field))
                                    .collect::<std::result::Result<Vec<_>, _>>()?;
                                (rows, total)
                            }};
                        }
                        let selected = match target {
                            SelfConfigTarget::InferenceBackend => {
                                let total = txn
                                    .read_view(&BackendList::new(&self.node_did))
                                    .await?
                                    .len();
                                let mut page =
                                    BackendList::new(&self.node_did).limit((limit + 1) as u64);
                                if let Some(cursor) = cursor {
                                    page = page.after(cursor);
                                }
                                let rows = read_inventory_in_txn(txn, &page)
                                    .await?
                                    .into_iter()
                                    .map(backend_value)
                                    .collect::<Result<Vec<_>>>()?;
                                (rows, total)
                            }
                            SelfConfigTarget::InferenceProfile => {
                                page!(ProfileList::new(&self.node_did), profile)
                            }
                            SelfConfigTarget::InferenceSampling => {
                                page!(SamplingList::new(&self.node_did), settings)
                            }
                            SelfConfigTarget::InferenceExecution => {
                                page!(ExecutionList::new(&self.node_did), settings)
                            }
                            SelfConfigTarget::InferenceRetryPolicy => {
                                page!(RetryPolicyList::new(&self.node_did), settings)
                            }
                            _ => unreachable!(),
                        };
                        Ok::<_, anyhow::Error>(selected)
                    })
                },
            )
            .await?;
        let truncated = selected.len() > limit;
        selected.truncate(limit);
        if target == SelfConfigTarget::InferenceExecution {
            for row in &mut selected {
                let effective = super::read::execution_settings(Some(row))?;
                row.as_object_mut()
                    .context("execution row must be an object")?
                    .insert("effective".into(), effective);
            }
        }
        let next_cursor = truncated
            .then(|| selected.last()?.get(id_field)?.as_str())
            .flatten();
        ordered! {
            "resource": collection,
            "items": selected,
            "page": json!({
                "limit": limit,
                "total": total,
                "returned": selected.len(),
                "truncated": truncated,
                "next_cursor": next_cursor,
            }),
            "note": "Inventory is read-only and does not rebind any agent. Backend credentials are excluded.",
        }.pretty()
    }
}

pub(super) fn backend_value(row: InventoryRow) -> Result<Value> {
    let metadata = row.metadata;
    let observation = gents_inference::backends::registry::scoped_observation_view(
        row.observation.as_ref(),
        &metadata.node_did,
        metadata.provider_kind,
    )?;
    Ok(json!({
        "backend_id": metadata.backend_id,
        "name": metadata.name,
        "provider_kind": metadata.provider_kind,
        "openai_wire_api": metadata.openai_wire_api,
        "endpoint": metadata.endpoint,
        "claude_platform": metadata.claude_platform,
        "connect_timeout_secs": metadata.connect_timeout_secs,
        "discovery_timeout_secs": metadata.discovery_timeout_secs,
        "max_concurrent": metadata.max_concurrent,
        "max_queue_depth": metadata.max_queue_depth,
        "enabled": metadata.enabled,
        "catalog": observation["catalog"],
        "probe_status": observation["probe_status"],
        "last_probe": observation["last_probe"],
    }))
}
