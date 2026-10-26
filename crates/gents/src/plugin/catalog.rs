//! A plugin's catalog: one non-job invocation of its declared `catalog.argv`.

use gents_protocol::job::CatalogEntry;
use serde_json::json;

use super::data::DataCaller;
use super::executor::PluginExecutor;
use super::PluginVerdict;
use crate::document_config::PluginToolRef;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("plugin {coordinate} declares no catalog; declare `catalog` in its plugin manifest")]
    NoCatalog { coordinate: String },
    #[error("plugin {coordinate} did not list its catalog ({verdict:?}): {diagnostics}")]
    NotListed {
        coordinate: String,
        verdict: PluginVerdict,
        diagnostics: String,
    },
    #[error("plugin {coordinate} listed a catalog of {bytes} bytes; at most {MAX_CATALOG_BYTES} are read")]
    TooLarge { coordinate: String, bytes: usize },
    #[error(
        "plugin {coordinate} did not list its catalog as [CatalogEntry]; its output began {head}"
    )]
    Undecodable {
        coordinate: String,
        head: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Run(#[from] super::executor::PluginRunError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

type Result<T, E = CatalogError> = std::result::Result<T, E>;

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
        return Err(CatalogError::NoCatalog {
            coordinate: coordinate.clone(),
        });
    };
    let call = executor
        .call_with_data(&record, json!({"argv": declared.argv}), caller.clone())
        .await?;
    let outcome = call.outcome;
    if outcome.verdict != PluginVerdict::Success {
        return Err(CatalogError::NotListed {
            coordinate: coordinate.clone(),
            verdict: outcome.verdict,
            diagnostics: outcome.diagnostics,
        });
    }
    let encoded = serde_json::to_string(&outcome.output)?;
    if encoded.len() > MAX_CATALOG_BYTES {
        return Err(CatalogError::TooLarge {
            coordinate: coordinate.clone(),
            bytes: encoded.len(),
        });
    }
    serde_json::from_value(outcome.output).map_err(|source| {
        let head = encoded[..encoded.floor_char_boundary(HEAD_BYTES)].to_owned();
        CatalogError::Undecodable {
            coordinate: coordinate.clone(),
            head,
            source,
        }
        .into()
    })
}
