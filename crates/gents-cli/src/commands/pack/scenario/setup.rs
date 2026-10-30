use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::super::cli_process::path_arg;
use super::super::server::spawn_server_with_args_and_env;
use super::manifest::{PackSeed, ScenarioManifest};
use super::validate::tool_package_needs_root;
use gents::graphql::validate_collection_identifier;

pub(super) struct DependencyInstall<'a> {
    pub(super) bin: &'a Path,
    pub(super) home: &'a Path,
    pub(super) registry: Option<&'a str>,
    pub(super) node_did: &'a str,
    pub(super) inference_profile_id: &'a str,
    pub(super) grant_authority: bool,
}

pub(super) async fn install_dependencies(
    ctx: &DependencyInstall<'_>,
    packages: &[String],
    environments: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<()> {
    for package in packages {
        tracing::info!(%package, "installing dependency");
        let mut args = vec![
            "pack".to_owned(),
            "install".to_owned(),
            package.clone(),
            "--home".to_owned(),
            path_arg(ctx.home),
            "--node-did".to_owned(),
            ctx.node_did.to_owned(),
            "--output".to_owned(),
            "json".to_owned(),
        ];
        if let Some(registry) = ctx.registry {
            args.push("--registry".to_owned());
            args.push(registry.to_owned());
        }
        if ctx.grant_authority {
            args.push("--grant-authority".to_owned());
        }
        let dependency_pack =
            super::super::resolve_pack_source(package, ctx.registry, ctx.home).await?;
        for slot in &dependency_pack.manifest().metadata.inference_slots {
            args.push("--inference-slot".to_owned());
            args.push(format!("{}={}", slot.name, ctx.inference_profile_id));
        }
        super::super::cli_process::run_cli_json_with_env(
            ctx.bin,
            &args,
            environments.get(package).cloned().unwrap_or_default(),
        )
        .await
        .with_context(|| format!("installing dependency {package}"))?;
        tracing::info!(%package, "installed dependency");
    }
    Ok(())
}

pub(super) fn pre_store_with_packs(home: &Path, with_pack: &[PathBuf]) -> Result<()> {
    let store = gents::pack::store::PackStore::new(home);
    for path in with_pack {
        if path.is_dir() {
            let (bytes, header) = gents::pack::archive::pack_dir(path)
                .with_context(|| format!("packing {}", path.display()))?;
            store.import(bytes.as_slice(), Some(&header.digest))?;
        } else {
            store
                .import_file(path, None)
                .with_context(|| format!("storing {}", path.display()))?;
        }
    }
    Ok(())
}

pub(super) fn resolve_pack_tool_root(
    pack: &Path,
    declared: Option<&str>,
    markers: &[String],
) -> Result<PathBuf> {
    let raw = declared.map(str::trim).filter(|value| !value.is_empty());
    let path = match raw {
        Some(raw) => {
            let path = PathBuf::from(raw);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .context("resolving init.tool_root against the process cwd")?
                    .join(path)
            }
        }
        None => pack.join("../.."),
    };
    let root = path
        .canonicalize()
        .with_context(|| format!("resolving pack tool root {}", path.display()))?;
    if !root.is_dir() {
        bail!("pack tool root is not a directory: {}", root.display());
    }
    let missing = markers
        .iter()
        .filter(|marker| !root.join(marker).exists())
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "pack tool root {} is missing required marker(s): {}",
            root.display(),
            missing.join(", ")
        );
    }
    Ok(root)
}

pub(super) fn seed_mutation(seed: &PackSeed, job_id: &str, prompt: &str) -> Result<String> {
    validate_collection_identifier(&seed.collection)?;
    gents::graphql::validate_graphql_name(&seed.job_id_field)?;
    gents::graphql::validate_graphql_name(&seed.prompt_field)?;
    for key in seed.fields.keys() {
        gents::graphql::validate_graphql_name(key)?;
    }
    let mut fields: serde_json::Map<String, serde_json::Value> = seed
        .fields
        .iter()
        .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
        .collect();
    fields.insert(seed.job_id_field.clone(), job_id.into());
    fields.insert(seed.prompt_field.clone(), prompt.into());
    let input = gents_protocol::graphql::graphql_input_literal(&serde_json::Value::Object(fields))?;
    Ok(format!(
        "mutation {{ create_{}(input: {input}) {{ _docID }} }}",
        seed.collection
    ))
}

pub(super) fn pack_init_cli_args(
    home: &Path,
    manifest: &ScenarioManifest,
    tool_root: Option<&Path>,
) -> Vec<String> {
    let mut init_args: Vec<String> = vec![
        "init".into(),
        "--home".into(),
        path_arg(home),
        "--dangerously-overwrite".into(),
        "--inference-url".into(),
        manifest.init.inference_url.clone(),
        "--model-name".into(),
        manifest.init.model_name.clone(),
        "--tool-package".into(),
        manifest.init.tool_package.clone(),
    ];
    if let Some(root) = tool_root {
        init_args.push("--tool-root".into());
        init_args.push(path_arg(root));
    }
    if let Some(preset) = manifest.init.backend_preset.as_deref() {
        init_args.push("--backend-preset".into());
        init_args.push(preset.into());
    }
    if let Some(wire) = manifest.init.openai_wire_api.as_deref() {
        init_args.push("--openai-wire-api".into());
        init_args.push(wire.into());
    }
    if let Some(api_key_env_var) = manifest.init.api_key_env_var.as_deref() {
        init_args.push("--api-key-env-var".into());
        init_args.push(api_key_env_var.into());
    }
    if let Some(max_concurrent) = manifest.init.max_concurrent {
        init_args.push("--max-concurrent".into());
        init_args.push(max_concurrent.to_string());
    }
    init_args
}

pub(super) fn default_job_id() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("exp-{secs}")
}
pub(super) fn resolve_manifest_tool_root(
    pack: &Path,
    manifest: &ScenarioManifest,
) -> Result<Option<PathBuf>> {
    if tool_package_needs_root(&manifest.init.tool_package) {
        Ok(Some(resolve_pack_tool_root(
            pack,
            manifest.init.tool_root.as_deref(),
            &manifest.init.tool_root_markers,
        )?))
    } else {
        Ok(None)
    }
}

pub(super) fn resolve_prepare_within(
    pack: &Path,
    manifest: &ScenarioManifest,
) -> Result<Option<PathBuf>> {
    let declared = manifest
        .init
        .tool_root
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match declared {
        Some(_) => Ok(Some(resolve_pack_tool_root(
            pack,
            manifest.init.tool_root.as_deref(),
            &manifest.init.tool_root_markers,
        )?)),
        None => Ok(None),
    }
}

pub(super) fn allow_scenario_folders(
    pack: &Path,
    manifest: &ScenarioManifest,
    home: &Path,
) -> Result<Vec<gents::plugin::allowed::AllowedDir>> {
    if manifest.allowed_folders.is_empty() {
        return Ok(Vec::new());
    }
    let root = resolve_prepare_within(pack, manifest)?
        .context("allowed_folders needs init.tool_root: every allowed folder stays inside it")?;
    manifest
        .allowed_folders
        .iter()
        .map(|folder| {
            let path = root.join(folder.path.trim());
            anyhow::ensure!(
                !path
                    .components()
                    .any(|part| part == std::path::Component::ParentDir),
                "allowed_folders path {:?} must not contain ..",
                folder.path
            );
            // A symlink cannot lead outside: re-check after creation.
            let inside = |path: &Path| path.starts_with(&root);
            let existing = path
                .ancestors()
                .find(|ancestor| ancestor.exists())
                .context("an absolute path has an existing ancestor")?;
            let planned = existing
                .canonicalize()
                .with_context(|| format!("resolving {}", existing.display()))?
                .join(path.strip_prefix(existing).unwrap_or(Path::new("")));
            let outside = |path: &Path| {
                anyhow::anyhow!(
                    "allowed_folders path {} is outside init.tool_root {}",
                    path.display(),
                    root.display()
                )
            };
            if !inside(&planned) {
                return Err(outside(&planned));
            }
            std::fs::create_dir_all(&path)
                .with_context(|| format!("creating allowed folder {}", path.display()))?;
            let canonical = path
                .canonicalize()
                .with_context(|| format!("resolving allowed folder {}", path.display()))?;
            if !inside(&canonical) {
                return Err(outside(&canonical));
            }
            gents::plugin::allowed::add(home, &canonical, folder.access)
        })
        .collect()
}

pub(super) fn spawn_server_with_pack(
    bin: &Path,
    home: &Path,
    port: u16,
    log: &Path,
    pack: &Path,
    tool_root: Option<&Path>,
    tool_root_env_var: Option<&str>,
) -> Result<tokio::process::Child> {
    let root = path_arg(pack);
    let environment = tool_root
        .zip(tool_root_env_var)
        .map(|(path, name)| vec![(name, path.to_string_lossy().into_owned())])
        .unwrap_or_default();
    spawn_server_with_args_and_env(bin, home, port, log, &["--apply-root", &root], &environment)
}

pub(super) fn stage_scenario_pack(
    pack: &Path,
    distribution: &gents::pack::PackManifest,
    node_did: &str,
    inference_profile_id: &str,
    initialized_default_agent_id: &str,
) -> Result<tempfile::TempDir> {
    let (mut authored, mut report) = crate::desired_state::load_manifest_root(pack);
    if authored.is_none() {
        (authored, report) =
            crate::desired_state::load_manifest_root_for_owner(pack, Some(node_did));
    }
    anyhow::ensure!(
        authored.is_some(),
        "invalid scenario pack configuration: {:?}",
        report.errors
    );
    let mut authored = authored.expect("checked scenario pack configuration");
    super::super::super::config::binding::rebind_manifest_to_node(&mut authored, node_did, true)?;
    authored.node.default_agent_id = Some(initialized_default_agent_id.to_owned());
    let bindings = distribution
        .metadata
        .inference_slots
        .iter()
        .map(|slot| (slot.name.clone(), inference_profile_id.to_owned()))
        .collect();
    let bound = gents::pack::bind_pack_install_config(distribution, &authored, &bindings)?;
    let staged = tempfile::tempdir().context("staging bound scenario pack")?;
    std::fs::write(
        staged.path().join("pack_config.json"),
        serde_json::to_vec_pretty(&bound)?,
    )?;
    for schema in &distribution.schemas {
        let destination = staged.path().join(schema);
        std::fs::create_dir_all(destination.parent().context("scenario schema parent")?)?;
        std::fs::copy(pack.join(schema), &destination)
            .with_context(|| format!("staging scenario schema {schema}"))?;
    }
    Ok(staged)
}
