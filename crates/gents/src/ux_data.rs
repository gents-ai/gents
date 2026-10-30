//! A UX plugin's data path: the datastore tools its pack's manifest
//! declares (`ux[].data`), run for one of its pages through the operator's
//! access to the agent's node.
//!
//! Three owners narrow one call. The manifest says which (surface, tool)
//! pairs this plugin may name at all; the node's `DatastoreToolSurface`
//! says what the tool is, with the fail-closed checks
//! [`crate::documents::merge_datastore_tool_surfaces`] applies for
//! an agent (missing, disabled, foreign, malformed, duplicate); ACP at the
//! node decides what the principal behind `access` may read or write. A
//! tool with a runtime-filled field is refused: a page has no triggering
//! document to fill it from.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::config_client::ConfigAccess;
use crate::defra_query::bounded::BoundedQueryParams;
use crate::defra_query::BoundedQueryTool;
use crate::defra_write::{BoundedWriteParams, BoundedWriteTool};
use crate::documents::{
    list_datastore_tool_surfaces, DatastoreToolSurfaceDocument, SurfaceToolDecl,
};
use crate::inference::llm::tool::Tool;
use crate::ux_plugin::UxListing;

/// One call from a page, as `desktop_ux_data_call` receives it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UxDataCall {
    /// The runtime listing id of the calling plugin (`<ns>/<pack>/<name>`).
    pub ux: String,
    pub node_did: String,
    pub surface: String,
    pub tool: String,
    /// The tool's own arguments: an object, or absent for none.
    #[serde(default)]
    pub args: Value,
}

/// The `kind` a call or a surface listing reports for an entry.
fn kind_of(entry: &SurfaceToolDecl) -> &'static str {
    match entry {
        SurfaceToolDecl::Create(_) => "create",
        SurfaceToolDecl::Query(_) => "query",
    }
}

fn listing_with_data(home: &Path, ux: &str) -> Result<UxListing> {
    let listing = crate::ux_plugin::find(home, ux)?;
    anyhow::ensure!(
        !listing.data.is_empty(),
        "ux plugin {ux:?} declares no data surfaces"
    );
    Ok(listing)
}

/// The installed surface `surface` of `node_did`, enabled, with each entry
/// well-formed and named once.
async fn installed_surface(
    access: &ConfigAccess,
    node_did: &str,
    surface: &str,
) -> Result<DatastoreToolSurfaceDocument> {
    let document = list_datastore_tool_surfaces(access, node_did)
        .await?
        .into_iter()
        .find(|document| document.surface_id == surface)
        .ok_or_else(|| anyhow!("surface {surface:?} is not installed on node {node_did}"))?;
    anyhow::ensure!(document.enabled, "surface {surface:?} is disabled");
    let mut names = rapidhash::RapidHashSet::default();
    for entry in document.entries.as_deref().unwrap_or(&[]) {
        entry
            .validate()
            .map_err(|error| anyhow!("surface {surface:?} has a malformed entry: {error}"))?;
        anyhow::ensure!(
            names.insert(entry.tool_name().to_owned()),
            "surface {surface:?} declares tool {:?} twice",
            entry.tool_name()
        );
    }
    Ok(document)
}

fn has_runtime_fill(entry: &SurfaceToolDecl) -> bool {
    match entry {
        SurfaceToolDecl::Create(decl) => decl.fields.iter().any(|field| field.fill.is_some()),
        SurfaceToolDecl::Query(decl) => decl.filter_fields.iter().any(|field| field.fill.is_some()),
    }
}

fn args_object(args: Value) -> Result<Map<String, Value>> {
    match args {
        Value::Null => Ok(Map::new()),
        Value::Object(map) => Ok(map),
        other => bail!(
            "args must be a JSON object of the tool's arguments, got {}",
            match other {
                Value::Array(_) => "an array",
                Value::String(_) => "a string",
                Value::Number(_) => "a number",
                Value::Bool(_) => "a boolean",
                Value::Null | Value::Object(_) => unreachable!(),
            }
        ),
    }
}

/// Runs one declared tool of one declared surface for the UX plugin listed
/// as `ux`, answering `{"kind": "query" | "create", "output": <the tool's
/// own result>}`.
pub async fn call(home: &Path, access: &ConfigAccess, request: UxDataCall) -> Result<Value> {
    let UxDataCall {
        ux,
        node_did,
        surface,
        tool,
        args,
    } = request;
    let listing = listing_with_data(home, &ux)?;
    anyhow::ensure!(
        listing
            .data
            .iter()
            .any(|declared| declared.surface == surface && declared.tools.contains(&tool)),
        "ux plugin {ux:?} may not call tool {tool:?} of surface {surface:?}: not declared in its \
         manifest"
    );
    let document = installed_surface(access, &node_did, &surface).await?;
    let entry = document
        .entries
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .find(|entry| entry.tool_name() == tool)
        .ok_or_else(|| anyhow!("surface {surface:?} has no tool {tool:?}"))?;
    anyhow::ensure!(
        !has_runtime_fill(entry),
        "tool {tool:?} has runtime-filled fields and is not callable from a page"
    );
    let args = args_object(args)?;
    let kind = kind_of(entry);
    let output = match entry {
        SurfaceToolDecl::Create(decl) => {
            let tool = BoundedWriteTool::new(access.clone(), decl.clone())
                .declared_by(Some(surface.clone()));
            Tool::call(&tool, BoundedWriteParams(args)).await?
        }
        SurfaceToolDecl::Query(decl) => {
            let tool = BoundedQueryTool::new(access.clone(), decl.clone())
                .declared_by(Some(surface.clone()));
            Tool::call(&tool, BoundedQueryParams(args)).await?
        }
    };
    let output: Value = serde_json::from_str(&output)
        .with_context(|| format!("tool {tool:?} answered something other than JSON"))?;
    tracing::info!(ux = %ux, surface = %surface, tool = %tool, kind, "ux data call");
    Ok(json!({ "kind": kind, "output": output }))
}

/// The declarations behind the plugin's surfaces on `node_did`: for each
/// surface the manifest names, whether it is enabled and the entries the
/// manifest lets the plugin call, each with its `kind`. A declared surface
/// the node does not have is listed disabled, without tools, as `missing`.
pub async fn surfaces(
    home: &Path,
    access: &ConfigAccess,
    ux: &str,
    node_did: &str,
) -> Result<Value> {
    let listing = listing_with_data(home, ux)?;
    let installed = list_datastore_tool_surfaces(access, node_did).await?;
    let mut out = Vec::with_capacity(listing.data.len());
    for declared in &listing.data {
        let Some(document) = installed
            .iter()
            .find(|document| document.surface_id == declared.surface)
        else {
            out.push(json!({
                "surface": declared.surface,
                "enabled": false,
                "missing": true,
                "tools": [],
            }));
            continue;
        };
        let tools = document
            .entries
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter(|entry| declared.tools.iter().any(|tool| tool == entry.tool_name()))
            .map(|entry| {
                let mut value = serde_json::to_value(entry)?;
                value["kind"] = json!(kind_of(entry));
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()?;
        out.push(json!({
            "surface": declared.surface,
            "enabled": document.enabled,
            "tools": tools,
        }));
    }
    Ok(json!({ "surfaces": out }))
}

#[cfg(test)]
mod tests;
