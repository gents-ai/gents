//! A plugin's catalog: one non-job invocation of its declared `catalog.argv`.

use anyhow::{bail, Context, Result};
use gents_protocol::job::CatalogEntry;
use serde_json::json;

use super::data::DataCaller;
use super::executor::PluginExecutor;
use super::PluginVerdict;
use crate::document_config::PluginToolRef;

/// The most a catalog call's output may encode to; the plugin output cap is larger.
const MAX_CATALOG_BYTES: usize = 256 * 1024;
/// How much of an undecodable output an error shows.
const HEAD_BYTES: usize = 512;

/// The entries `plugin_ref`'s plugin lists, from one call of its declared
/// `catalog.argv`. The call mounts the pack tier only and offers `data_calls`
/// read-only as `caller`; it offers no `job_calls` or `tool_calls`, and no
/// write it asks for is applied.
pub async fn catalog(
    executor: &PluginExecutor,
    plugin_ref: &PluginToolRef,
    caller: &DataCaller,
) -> Result<Vec<CatalogEntry>> {
    let coordinate = &plugin_ref.plugin;
    let record = executor.resolve(coordinate, plugin_ref.digest.as_deref())?;
    let Some(declared) = &record.declaration.catalog else {
        bail!("plugin {coordinate} declares no catalog; declare `catalog` in its plugin manifest");
    };
    let call = executor
        .call_with_data(&record, json!({"argv": declared.argv}), caller.clone())
        .await?;
    let outcome = call.outcome;
    if outcome.verdict != PluginVerdict::Success {
        bail!(
            "plugin {coordinate} did not list its catalog ({:?}): {}",
            outcome.verdict,
            outcome.diagnostics
        );
    }
    let encoded = serde_json::to_string(&outcome.output)?;
    if encoded.len() > MAX_CATALOG_BYTES {
        bail!(
            "plugin {coordinate} listed a catalog of {} bytes; at most {MAX_CATALOG_BYTES} are read",
            encoded.len()
        );
    }
    serde_json::from_value(outcome.output).with_context(|| {
        let head = &encoded[..encoded.floor_char_boundary(HEAD_BYTES)];
        format!("plugin {coordinate} did not list its catalog as [CatalogEntry]; its output began {head}")
    })
}
