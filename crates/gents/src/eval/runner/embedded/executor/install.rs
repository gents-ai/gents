use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::defra_node::EmbeddedNode;
use crate::documents::{InferenceSampling, PackConfig};
use crate::eval::runner::embedded::home::EmbeddedHome;
use crate::eval::runner::executor::{InferenceBinding, TrialFixtures, TrialSpec};
use crate::graphql::{escape_graphql_string, validate_collection_identifier};
use crate::pack::{
    bind_pack_install_config, declared_paths, digest_declared_assets, load_pack_config,
    PackInferenceBindings, PackInstallOptions, PackManifest,
};
use crate::{Collection, ConfigAccess};

pub(super) async fn install(spec: &TrialSpec, home: &EmbeddedHome, workspace: &Path) -> Result<()> {
    let (manifest, assets) = read_pack(&spec.pack_dir)?;
    let digest = digest_declared_assets(&manifest, |path| {
        assets
            .get(path)
            .map(Vec::as_slice)
            .with_context(|| format!("pack has no asset {path:?}"))
    })?;
    anyhow::ensure!(
        digest == spec.pack_digest,
        "pack digest mismatch: {} materialized as {digest}, but the run froze {}",
        spec.pack_dir.display(),
        spec.pack_digest
    );

    let node_did = home.did().to_string();
    let config = load_pack_config(
        &manifest,
        &PackInstallOptions {
            node_did: node_did.clone(),
        },
        &|path| {
            assets
                .get(path)
                .cloned()
                .with_context(|| format!("pack has no asset {path:?}"))
        },
        &|name| std::env::var(name).ok(),
    )?;
    let config = bind_inference_slots(&manifest, &config, &spec.inference)
        .context("binding the pack's inference slots to the frozen profile")?;
    let mut config = root_host_tools(config, workspace)?;
    config.node.default_agent_id = Some(spec.agent_id.clone());
    let access = ConfigAccess::local(home.node.clone());
    for path in &manifest.schemas {
        let bytes = assets
            .get(path)
            .with_context(|| format!("pack has no schema asset {path:?}"))?;
        let sdl =
            std::str::from_utf8(bytes).with_context(|| format!("decoding pack schema {path:?}"))?;
        install_schema(&access, sdl)
            .await
            .with_context(|| format!("installing pack schema {path:?}"))?;
    }
    // A reference is only installable once what it names exists.
    apply(
        &access,
        "eval.trial.install_inference",
        &inference_plan(&spec.inference, &spec.trial_id, &node_did)?,
    )
    .await
    .context("installing the trial inference binding")?;
    apply(
        &access,
        "eval.trial.install_pack",
        &DesiredStateApplyPlan::from_pack_config(&config)?,
    )
    .await
    .context("installing the trial pack")?;

    install_workspace_root(&home.node, workspace).await?;
    install_fixtures(&access, &spec.fixtures, workspace, &node_did).await
}

fn bind_inference_slots(
    manifest: &PackManifest,
    config: &PackConfig,
    binding: &InferenceBinding,
) -> Result<PackConfig> {
    let profile_id = binding
        .profile
        .get("profile_id")
        .and_then(Value::as_str)
        .context("the trial's InferenceProfile document has no profile_id")?;
    let bindings: PackInferenceBindings = manifest
        .metadata
        .inference_slots
        .iter()
        .filter(|slot| !slot.optional)
        .map(|slot| (slot.name.clone(), profile_id.to_owned()))
        .collect();
    bind_pack_install_config(manifest, config, &bindings)
}

fn root_host_tools(mut config: PackConfig, workspace: &Path) -> Result<PackConfig> {
    let root = workspace
        .to_str()
        .with_context(|| format!("{} is not UTF-8", workspace.display()))?;
    for tools in &mut config.tools {
        if let Some(host) = tools.host.as_mut() {
            host.root.get_or_insert_with(|| root.to_owned());
        }
    }
    Ok(config)
}

pub(super) async fn apply(
    access: &ConfigAccess,
    operation: &'static str,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    access
        .transact_write(operation, |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
}

pub(super) fn read_pack(pack_dir: &Path) -> Result<(PackManifest, BTreeMap<String, Vec<u8>>)> {
    let manifest_path = pack_dir.join("manifest.json");
    let manifest: PackManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?,
    )
    .context("parsing pack manifest")?;
    let mut assets = BTreeMap::new();
    for path in declared_paths(&manifest) {
        let file = pack_dir.join(&path);
        let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
        assets.insert(path, bytes);
    }
    Ok((manifest, assets))
}

fn inference_plan(
    binding: &InferenceBinding,
    trial_id: &str,
    node_did: &str,
) -> Result<DesiredStateApplyPlan> {
    let mut backend = binding.backend.clone();
    own(&mut backend, "InferenceBackend", node_did)?;

    let mut sampling = match binding.sampling.clone() {
        Some(sampling) => sampling,
        None => serde_json::to_value(InferenceSampling {
            node_did: node_did.to_string(),
            sampling_id: format!("eval-{trial_id}"),
            ..Default::default()
        })?,
    };
    own(&mut sampling, "InferenceSampling", node_did)?;
    object(&mut sampling, "InferenceSampling")?.insert("seed".to_string(), json!(binding.seed));
    let sampling_id = sampling
        .get("sampling_id")
        .and_then(Value::as_str)
        .context("the trial's InferenceSampling document has no sampling_id")?
        .to_string();

    let mut profile = binding.profile.clone();
    own(&mut profile, "InferenceProfile", node_did)?;
    object(&mut profile, "InferenceProfile")?.insert("sampling_id".to_string(), json!(sampling_id));

    let mut documents = Vec::new();
    for (collection, value) in [
        (Collection::InferenceRetryPolicy, &binding.retry_policy),
        (Collection::InferenceExecution, &binding.execution),
    ] {
        if let Some(value) = value {
            let mut value = value.clone();
            own(&mut value, collection.graphql_type(), node_did)?;
            documents.push(desired(collection, value));
        }
    }
    documents.extend([
        desired(Collection::InferenceBackend, backend),
        desired(Collection::InferenceSampling, sampling),
        desired(Collection::InferenceProfile, profile),
    ]);
    DesiredStateApplyPlan::new(documents)
}

pub(super) fn desired(collection: Collection, value: Value) -> DesiredStateApplyDocument {
    DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    }
}

fn own(value: &mut Value, named: &str, node_did: &str) -> Result<()> {
    object(value, named)?.insert("node_did".to_string(), json!(node_did));
    Ok(())
}

fn object<'a>(value: &'a mut Value, named: &str) -> Result<&'a mut serde_json::Map<String, Value>> {
    value
        .as_object_mut()
        .with_context(|| format!("the trial's {named} document is not an object"))
}

async fn install_workspace_root(
    node: &std::sync::Arc<EmbeddedNode>,
    workspace: &Path,
) -> Result<()> {
    let root = escape_graphql_string(
        workspace
            .to_str()
            .with_context(|| format!("{} is not UTF-8", workspace.display()))?,
    );
    let updated_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_WorkspaceRoot(input: {{
                root_path: "{root}",
                display_name: "Eval trial workspace",
                enabled: true,
                updated_at: "{updated_at}"
            }}) {{ _docID }}
        }}"#
    );
    ConfigAccess::write_local(node, "eval.trial.workspace_root", &mutation)
        .await
        .context("publishing the trial workspace root")?;
    Ok(())
}

async fn install_fixtures(
    access: &ConfigAccess,
    fixtures: &TrialFixtures,
    workspace: &Path,
    trial_did: &str,
) -> Result<()> {
    for sdl in &fixtures.schemas {
        install_schema(access, sdl)
            .await
            .context("installing a fixture schema")?;
    }
    for fixture in &fixtures.documents {
        create_document(
            access,
            "eval.trial.fixture_document",
            &fixture.collection,
            &bind_fixture_owner(&fixture.document, trial_did),
        )
        .await?;
    }
    for file in &fixtures.files {
        let path = workspace_path(workspace, &file.path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&path, &file.contents)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

pub(super) async fn install_schema(access: &ConfigAccess, sdl: &str) -> Result<()> {
    let plan = crate::config_client::preview_schema_install(access, sdl).await?;
    crate::config_client::apply_schema_install(access, sdl, &plan.artifact_digest).await?;
    Ok(())
}

pub(crate) fn bind_fixture_owner(value: &Value, trial_did: &str) -> Value {
    match value {
        Value::String(text) if text == "$trial" => Value::String(trial_did.into()),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|v| bind_fixture_owner(v, trial_did))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), bind_fixture_owner(value, trial_did)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

pub(super) async fn create_document(
    access: &ConfigAccess,
    label: &'static str,
    collection: &str,
    document: &Value,
) -> Result<String> {
    validate_collection_identifier(collection)?;
    let mutation = format!(
        "mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}"
    );
    let variables = json!({ "input": document });
    access
        .transact_write(label, |txn| {
            let (mutation, variables) = (&mutation, &variables);
            Box::pin(async move {
                let response = txn.execute_with_variables(mutation, variables).await?;
                crate::graphql::created_doc_id(&response, collection)
            })
        })
        .await
        .with_context(|| format!("creating a {collection} document"))
}

fn workspace_path(workspace: &Path, relative: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        Path::new(relative)
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "workspace path {relative:?} must stay inside the trial workspace"
    );
    Ok(workspace.join(relative))
}

#[cfg(test)]
mod tests;
