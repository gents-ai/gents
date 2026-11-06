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
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::documents::{
    list_datastore_tool_surfaces, DatastoreToolSurfaceDocument, QueryToolDecl, SurfaceToolDecl,
};
use crate::inference::llm::tool::Tool;
use crate::plugin::ux::UxListing;
use crate::store::config_client::ConfigAccess;
use crate::tools::builtin::defra_query::BoundedQueryTool;
use crate::tools::builtin::defra_query::bounded::BoundedQueryParams;
use crate::tools::builtin::defra_write::{BoundedWriteParams, BoundedWriteTool};

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
    let listing = crate::plugin::ux::find(home, ux)?;
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
    crate::documents::validate_datastore_surface(&document)?;
    Ok(document)
}

fn manifest_allows_tool(listing: &UxListing, surface: &str, tool: &str) -> bool {
    listing.data.iter().any(|declared| {
        declared.surface == surface && declared.tools.iter().any(|name| name == tool)
    })
}

async fn trusted_ux_create_schema(
    home: &Path,
    listing: &UxListing,
    collection: &str,
) -> Result<String> {
    let home = home.to_owned();
    let listing = listing.clone();
    let collection = collection.to_owned();
    let schema = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
        let Some(coordinate) = listing.pack.as_deref() else {
            return Ok(None);
        };
        let Some(record) = crate::pack::read_home_install(&home, coordinate)? else {
            return Ok(None);
        };
        let store = crate::pack::store::PackStore::new(&home);
        if !store.path(&record.digest)?.is_file() {
            return Ok(None);
        }
        let archive = store.open(&record.digest)?;
        let manifest_bytes = archive.asset("manifest.json")?.to_vec();
        let manifest = crate::pack::PackManifest::from_slice(&manifest_bytes)?;
        let mut schemas = std::collections::BTreeMap::new();
        for path in crate::pack::document_pack_schema_paths(&manifest)? {
            schemas.insert(path.to_owned(), archive.asset(path)?.to_vec());
        }
        let digest = crate::pack::digest_declared_assets(&manifest, |path| {
            if path == "manifest.json" {
                Ok(manifest_bytes.as_slice())
            } else if let Some(bytes) = schemas.get(path) {
                Ok(bytes.as_slice())
            } else {
                archive.asset(path)
            }
        })?;
        anyhow::ensure!(
            digest == record.digest,
            "installed pack content does not match its recorded digest"
        );
        anyhow::ensure!(
            format!("{}/{}", manifest.metadata.namespace, manifest.name) == coordinate
                && manifest.version == record.version
                && manifest.metadata.kind == record.kind,
            "installed UX record does not match its trusted pack"
        );
        let declared = manifest.metadata.ux.iter().any(|entry| {
            format!("{coordinate}/{}", entry.name) == listing.id && entry.data == listing.data
        });
        anyhow::ensure!(
            declared,
            "installed UX declaration does not match its trusted pack"
        );
        let mut found = None;
        for path in crate::pack::document_pack_schema_paths(&manifest)? {
            let bytes = schemas
                .get(path)
                .with_context(|| format!("installed schema {path:?} missing"))?;
            let sdl = std::str::from_utf8(bytes)?;
            let schemas = query::parse_sdl(sdl)?;
            if schemas.iter().any(|schema| schema.name == collection) {
                anyhow::ensure!(
                    schemas.len() == 1,
                    "client signing requires exactly one collection in the trusted schema"
                );
                anyhow::ensure!(
                    found.is_none(),
                    "installed pack declares the collection more than once"
                );
                found = Some(sdl.to_owned());
            }
        }
        Ok(found)
    })
    .await
    .context("reading trusted UX pack schema")??;
    schema.ok_or_else(|| {
        crate::store::config_client::ConfigClientError::ClientUnsignedMutationUnsupported.into()
    })
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
        manifest_allows_tool(&listing, &surface, &tool),
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
            let mut tool = BoundedWriteTool::new(access.clone(), decl.clone())
                .declared_by(Some(surface.clone()))
                .with_visible_surface_owner(node_did.clone());
            if matches!(access, ConfigAccess::Graphql(_)) {
                let schema = trusted_ux_create_schema(home, &listing, &decl.collection).await?;
                tool = tool.with_ux_client_schema(schema);
            }
            Tool::call(&tool, BoundedWriteParams(args)).await?
        }
        SurfaceToolDecl::Query(decl) => {
            let tool = BoundedQueryTool::new(access.clone(), decl.clone())
                .declared_by(Some(surface.clone()))
                .with_visible_surface_owner(node_did.clone());
            Tool::call(&tool, BoundedQueryParams(args)).await?
        }
    };
    let output: Value = serde_json::from_str(&output)
        .with_context(|| format!("tool {tool:?} answered something other than JSON"))?;
    tracing::info!(ux = %ux, surface = %surface, tool = %tool, kind, "ux data call");
    Ok(json!({ "kind": kind, "output": output }))
}

async fn watch_declaration(
    home: &Path,
    access: &ConfigAccess,
    request: &UxDataCall,
) -> Result<QueryToolDecl> {
    let listing = listing_with_data(home, &request.ux)?;
    anyhow::ensure!(
        listing
            .data
            .iter()
            .any(|declared| declared.surface == request.surface
                && declared.tools.contains(&request.tool)),
        "query is not declared in the plugin manifest"
    );
    let document = installed_surface(access, &request.node_did, &request.surface).await?;
    let entry = document
        .entries
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .find(|entry| entry.tool_name() == request.tool)
        .ok_or_else(|| anyhow!("surface has no declared query"))?;
    anyhow::ensure!(!has_runtime_fill(entry), "query has runtime-filled fields");
    match entry {
        SurfaceToolDecl::Query(decl) => Ok(decl.clone()),
        SurfaceToolDecl::Create(_) => bail!("only declared queries may be observed"),
    }
}

/// One operator-owned local subscription. It emits query results, never raw
/// document change events. Drop it to close the underlying subscription.
pub struct UxDataWatch {
    home: PathBuf,
    access: ConfigAccess,
    request: UxDataCall,
    declaration: QueryToolDecl,
    changes: events::DocumentChangeSubscription,
    last: Value,
    closed: bool,
}

impl UxDataWatch {
    pub fn initial(&self) -> &Value {
        &self.last
    }

    pub async fn next(&mut self) -> Result<Value> {
        anyhow::ensure!(!self.closed, "pack data observation is closed");
        let result = self.next_open().await;
        if result.is_err() {
            self.closed = true;
        }
        result
    }

    async fn next_open(&mut self) -> Result<Value> {
        loop {
            self.changes
                .recv()
                .await
                .context("pack data observation closed")?;
            anyhow::ensure!(
                watch_declaration(&self.home, &self.access, &self.request).await?
                    == self.declaration,
                "pack data declaration changed; reconnect the observation"
            );
            let current = call(&self.home, &self.access, self.request.clone()).await?;
            anyhow::ensure!(
                watch_declaration(&self.home, &self.access, &self.request).await?
                    == self.declaration,
                "pack data declaration changed; reconnect the observation"
            );
            if current != self.last {
                self.last = current.clone();
                return Ok(current);
            }
        }
    }
}

/// The caller must authenticate the node operator. Subscription precedes the
/// initial read so a concurrent change remains pending for the next read.
pub async fn watch_local(
    home: &Path,
    node: Arc<crate::defra_node::Node>,
    request: UxDataCall,
) -> Result<UxDataWatch> {
    anyhow::ensure!(
        node.node_identity_did() == Some(request.node_did.as_str()),
        "selected node does not own this runtime"
    );
    let access = ConfigAccess::local(node.clone());
    let declaration = watch_declaration(home, &access, &request).await?;
    let changes = crate::subscribe_collections(
        &node,
        [declaration.collection.as_str(), "DatastoreToolSurface"],
    )?;
    let last = call(home, &access, request.clone()).await?;
    anyhow::ensure!(
        watch_declaration(home, &access, &request).await? == declaration,
        "pack data declaration changed; reconnect the observation"
    );
    Ok(UxDataWatch {
        home: home.to_owned(),
        access,
        request,
        declaration,
        changes,
        last,
        closed: false,
    })
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
