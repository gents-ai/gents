use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::cli::args::{SchemaApplyArgs, SchemaCommand};
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_config_access};

#[derive(Debug, thiserror::Error)]
pub(crate) enum SchemaError {
    #[error("no schema inputs found under {}; expected *.graphql, *.gql, *.patch.json, or *.json-patch", root.display())]
    NoInputs { root: PathBuf },
    #[error("pack root {} has a schemas/ directory but no *.graphql, *.gql, *.patch.json, or *.json-patch files", pack_root.display())]
    PackHasNoInputs { pack_root: PathBuf },
    #[error("reading metadata for {}", path.display())]
    ReadMetadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("schema input path is neither file nor directory: {}", path.display())]
    NotFileOrDirectory { path: PathBuf },
    #[error("reading schema directory {}", path.display())]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading schema directory entries from {}", path.display())]
    ReadDirectoryEntries {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading schema SDL {}", path.display())]
    ReadSdl {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading schema patch {}", path.display())]
    ReadPatch {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing schema patch JSON {}", path.display())]
    ParsePatch {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("schema patch must be a JSON Patch array or an object with Patch/patch")]
    PatchShape,
    #[error("schema patch must start with an op containing path")]
    PatchMissingPath,
    #[error("schema patch path has no collection name: {path}")]
    PatchMissingCollection { path: String },
    #[error("schema patch must be a JSON Patch array")]
    PatchNotArray,
    #[error("collection {collection} does not exist")]
    CollectionMissing { collection: String },
    #[error(transparent)]
    Config(#[from] gents::config_client::ConfigClientError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaInputKind {
    Sdl,
    Patch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemaInput {
    path: PathBuf,
    kind: SchemaInputKind,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SchemaApplyFileResult {
    path: String,
    status: &'static str,
    collections: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SchemaApplyPatchResult {
    path: String,
    status: &'static str,
    collection: String,
    applied_fields: Vec<String>,
    skipped_fields: Vec<String>,
    version_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct SchemaApplySummary {
    status: &'static str,
    home: String,
    mode: &'static str,
    graphql: Option<String>,
    root: String,
    schema_files: Vec<SchemaApplyFileResult>,
    patch_files: Vec<SchemaApplyPatchResult>,
}

pub(crate) async fn dispatch(command: SchemaCommand) -> Result<()> {
    match command {
        SchemaCommand::Apply(args) => schema_apply(args).await,
    }
}

pub(crate) async fn schema_apply(args: SchemaApplyArgs) -> Result<()> {
    let root = args.path;
    let explicit_patches = args.patches;
    let inputs = discover_schema_inputs(&root, &explicit_patches)?;
    if inputs.is_empty() {
        return Err(SchemaError::NoInputs { root: root.clone() }.into());
    }

    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let graphql = match &access {
        ConfigAccess::Graphql(endpoint) => Some(endpoint.url().to_owned()),
        ConfigAccess::Local(_) => None,
    };

    let phase = apply_schema_inputs(&access, &root, &inputs).await?;

    let summary = SchemaApplySummary {
        status: "schema_applied",
        home: home_dir.display().to_string(),
        mode: access.mode(),
        graphql,
        root: root.display().to_string(),
        schema_files: phase.schema_files,
        patch_files: phase.patch_files,
    };
    print_json(&serde_json::to_value(summary)?)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PackSchemaPhase {
    pub(crate) status: &'static str,
    pub(crate) root: String,
    pub(crate) schema_files: Vec<SchemaApplyFileResult>,
    pub(crate) patch_files: Vec<SchemaApplyPatchResult>,
}

impl PackSchemaPhase {
    pub(crate) fn changed(&self) -> bool {
        self.schema_files
            .iter()
            .any(|file| file.status != "already_exists")
            || self
                .patch_files
                .iter()
                .any(|patch| patch.status != "already_exists")
    }
}

pub(crate) async fn apply_pack_schemas_if_present(
    access: &ConfigAccess,
    pack_root: &Path,
) -> Result<Option<PackSchemaPhase>> {
    let schemas_dir = pack_root.join("schemas");
    if !schemas_dir.is_dir() {
        return Ok(None);
    }
    let inputs = discover_schema_inputs(&schemas_dir, &[])?;
    if inputs.is_empty() {
        return Err(SchemaError::PackHasNoInputs {
            pack_root: pack_root.to_path_buf(),
        }
        .into());
    }
    Ok(Some(
        apply_schema_inputs(access, &schemas_dir, &inputs).await?,
    ))
}

async fn apply_schema_inputs(
    access: &ConfigAccess,
    root: &Path,
    inputs: &[SchemaInput],
) -> Result<PackSchemaPhase> {
    let mut schema_files = Vec::new();
    for input in inputs
        .iter()
        .filter(|input| input.kind == SchemaInputKind::Sdl)
    {
        schema_files.push(apply_sdl_file(access, &input.path).await?);
    }

    let mut patch_files = Vec::new();
    for input in inputs
        .iter()
        .filter(|input| input.kind == SchemaInputKind::Patch)
    {
        patch_files.push(apply_patch_file(access, &input.path).await?);
    }

    for input in inputs
        .iter()
        .filter(|input| input.kind == SchemaInputKind::Sdl)
    {
        let sdl = fs::read_to_string(&input.path)?;
        gents::config_client::preview_additive_schema_install(access, &sdl)
            .await?
            .require_satisfied()
            .with_context(|| format!("schema series did not satisfy {}", input.path.display()))?;
    }

    Ok(PackSchemaPhase {
        status: "schema_applied",
        root: root.display().to_string(),
        schema_files,
        patch_files,
    })
}

fn discover_schema_inputs(
    root: &Path,
    explicit_patches: &[PathBuf],
) -> Result<Vec<SchemaInput>, SchemaError> {
    let mut discovered = Vec::new();
    collect_schema_inputs(root, &mut discovered)?;
    for path in explicit_patches {
        discovered.push(SchemaInput {
            path: path.clone(),
            kind: SchemaInputKind::Patch,
        });
    }

    let mut by_path = BTreeMap::<PathBuf, SchemaInputKind>::new();
    for input in discovered {
        by_path.entry(input.path).or_insert(input.kind);
    }

    Ok(by_path
        .into_iter()
        .map(|(path, kind)| SchemaInput { path, kind })
        .collect())
}

fn collect_schema_inputs(path: &Path, output: &mut Vec<SchemaInput>) -> Result<(), SchemaError> {
    let metadata = fs::metadata(path).map_err(|source| SchemaError::ReadMetadata {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.is_file() {
        if let Some(kind) = classify_schema_input(path) {
            output.push(SchemaInput {
                path: path.to_path_buf(),
                kind,
            });
        }
        return Ok(());
    }

    if !metadata.is_dir() {
        return Err(SchemaError::NotFileOrDirectory {
            path: path.to_path_buf(),
        });
    }

    let mut entries = fs::read_dir(path)
        .map_err(|source| SchemaError::ReadDirectory {
            path: path.to_path_buf(),
            source,
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|source| SchemaError::ReadDirectoryEntries {
            path: path.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        collect_schema_inputs(&entry.path(), output)?;
    }

    Ok(())
}

fn classify_schema_input(path: &Path) -> Option<SchemaInputKind> {
    let file_name = path.file_name()?.to_string_lossy();
    if file_name.ends_with(".patch.json") || file_name.ends_with(".json-patch") {
        return Some(SchemaInputKind::Patch);
    }

    match path.extension().and_then(|extension| extension.to_str()) {
        Some("graphql" | "gql") => Some(SchemaInputKind::Sdl),
        _ => None,
    }
}

async fn apply_sdl_file(access: &ConfigAccess, path: &Path) -> Result<SchemaApplyFileResult> {
    let sdl = fs::read_to_string(path).map_err(|source| SchemaError::ReadSdl {
        path: path.to_path_buf(),
        source,
    })?;
    let plan = gents::config_client::preview_additive_schema_install(access, &sdl)
        .await
        .with_context(|| format!("preview schema SDL {}", path.display()))?;
    let receipt =
        gents::config_client::apply_additive_schema_install(access, &sdl, &plan.artifact_digest)
            .await
            .with_context(|| format!("publish schema SDL {}", path.display()))?;
    Ok(SchemaApplyFileResult {
        path: path.display().to_string(),
        status: if receipt.requires_publication {
            "applied"
        } else {
            "already_exists"
        },
        collections: receipt.collection_contracts.into_keys().collect(),
    })
}

async fn apply_patch_file(
    access: &ConfigAccess,
    path: &Path,
) -> Result<SchemaApplyPatchResult, SchemaError> {
    let raw = fs::read_to_string(path).map_err(|source| SchemaError::ReadPatch {
        path: path.to_path_buf(),
        source,
    })?;
    let patch = normalize_patch_value(serde_json::from_str::<Value>(&raw).map_err(|source| {
        SchemaError::ParsePatch {
            path: path.to_path_buf(),
            source,
        }
    })?)?;
    let collection = collection_name_from_patch(&patch)?;
    let field_adds = additive_field_ops(&collection, &patch)?;
    let existing_fields = collection_field_names(access, &collection).await?;

    let (patch_to_apply, applied_fields, skipped_fields) =
        filter_existing_field_adds(&patch, &field_adds, &existing_fields)?;
    if patch_to_apply.as_array().is_some_and(Vec::is_empty) {
        return Ok(SchemaApplyPatchResult {
            path: path.display().to_string(),
            status: "already_exists",
            collection: collection.clone(),
            applied_fields,
            skipped_fields,
            version_id: current_collection_version(access, &collection).await?,
        });
    }

    let patched = access
        .patch_collection_schema(&collection, &patch_to_apply)
        .await?;
    let version_id = patched
        .get("VersionID")
        .and_then(Value::as_str)
        .map(str::to_owned);

    Ok(SchemaApplyPatchResult {
        path: path.display().to_string(),
        status: "applied",
        collection,
        applied_fields,
        skipped_fields,
        version_id,
    })
}

async fn collection_field_names(
    access: &ConfigAccess,
    collection: &str,
) -> Result<BTreeSet<String>, SchemaError> {
    let fields = access.collection_fields(collection).await?.ok_or_else(|| {
        SchemaError::CollectionMissing {
            collection: collection.to_owned(),
        }
    })?;
    Ok(fields)
}

async fn current_collection_version(
    access: &ConfigAccess,
    collection: &str,
) -> Result<Option<String>, SchemaError> {
    Ok(access
        .collection_version(collection)
        .await?
        .and_then(|version| {
            version
                .get("VersionID")
                .and_then(Value::as_str)
                .map(str::to_owned)
        }))
}

fn normalize_patch_value(value: Value) -> Result<Value, SchemaError> {
    if let Some(patch) = value.get("Patch").or_else(|| value.get("patch")) {
        return normalize_patch_value(patch.clone());
    }
    if !value.is_array() {
        return Err(SchemaError::PatchShape);
    }
    Ok(value)
}

fn collection_name_from_patch(patch: &Value) -> Result<String, SchemaError> {
    let path = patch
        .as_array()
        .and_then(|ops| ops.first())
        .and_then(|op| op.get("path"))
        .and_then(Value::as_str)
        .ok_or(SchemaError::PatchMissingPath)?;
    let collection = path
        .split('/')
        .find(|segment| !segment.is_empty())
        .ok_or_else(|| SchemaError::PatchMissingCollection {
            path: path.to_owned(),
        })?;
    Ok(collection.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FieldAdd {
    index: usize,
    name: String,
}

fn additive_field_ops(collection: &str, patch: &Value) -> Result<Vec<FieldAdd>, SchemaError> {
    let ops = patch.as_array().ok_or(SchemaError::PatchNotArray)?;
    let mut fields = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        let op_name = op.get("op").and_then(Value::as_str).unwrap_or_default();
        let path = op.get("path").and_then(Value::as_str).unwrap_or_default();
        if op_name != "add" || path != format!("/{collection}/Fields/-") {
            return Ok(Vec::new());
        }
        let Some(name) = op
            .get("value")
            .and_then(|value| value.get("Name"))
            .and_then(Value::as_str)
        else {
            return Ok(Vec::new());
        };
        fields.push(FieldAdd {
            index,
            name: name.to_string(),
        });
    }
    Ok(fields)
}

fn filter_existing_field_adds(
    patch: &Value,
    field_adds: &[FieldAdd],
    existing_fields: &BTreeSet<String>,
) -> Result<(Value, Vec<String>, Vec<String>), SchemaError> {
    if field_adds.is_empty() {
        return Ok((patch.clone(), Vec::new(), Vec::new()));
    }

    let mut applied_fields = Vec::new();
    let mut skipped_fields = Vec::new();
    let mut skip_indexes = BTreeSet::new();
    for field in field_adds {
        if existing_fields.contains(&field.name) {
            skipped_fields.push(field.name.clone());
            skip_indexes.insert(field.index);
        } else {
            applied_fields.push(field.name.clone());
        }
    }

    let ops = patch.as_array().ok_or(SchemaError::PatchNotArray)?;
    let filtered = ops
        .iter()
        .enumerate()
        .filter(|(index, _)| !skip_indexes.contains(index))
        .map(|(_, op)| op.clone())
        .collect::<Vec<_>>();
    Ok((Value::Array(filtered), applied_fields, skipped_fields))
}

#[cfg(test)]
mod pack_schema_tests {
    use super::*;
    use gents_protocol::error_chain::ErrorChain;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn cli_sdl_apply_checks_contracts_and_reports_reinstall_accurately() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("widget.graphql");
        let node = std::sync::Arc::new(
            gents::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        let access = ConfigAccess::local(node.clone());
        fs::write(&path, "type Widget { message: String }").unwrap();
        assert_eq!(
            apply_sdl_file(&access, &path).await.unwrap().status,
            "applied"
        );
        assert_eq!(
            apply_sdl_file(&access, &path).await.unwrap().status,
            "already_exists"
        );
        fs::write(&path, "type Widget { message: Int }").unwrap();
        let error = apply_sdl_file(&access, &path).await.unwrap_err();
        assert!(
            ErrorChain(error.as_ref())
                .to_string()
                .contains("does not match requested schema"),
            "{}",
            ErrorChain(error.as_ref())
        );
        fs::write(
            &path,
            "type Widget { message: String } type OtherWidget { message: String }",
        )
        .unwrap();
        assert!(apply_sdl_file(&access, &path).await.is_err());
        assert!(node.get_collection("OtherWidget").unwrap().is_none());
        fs::write(&path, "type Widget { message: String pending: String }").unwrap();
        let inputs = discover_schema_inputs(dir.path(), &[]).unwrap();
        let error = apply_schema_inputs(&access, dir.path(), &inputs)
            .await
            .unwrap_err();
        assert!(
            ErrorChain(error.as_ref())
                .to_string()
                .contains("missing declared fields"),
            "{}",
            ErrorChain(error.as_ref())
        );
        assert!(!access
            .collection_fields("Widget")
            .await
            .unwrap()
            .unwrap()
            .contains("pending"));
    }
}
