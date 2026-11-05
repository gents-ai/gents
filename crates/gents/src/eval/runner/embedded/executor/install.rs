mod expired_request;

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::documents::{InferenceSampling, PackConfig};
use crate::eval::runner::embedded::home::EmbeddedHome;
use crate::eval::runner::executor::{InferenceBinding, TrialFixtures, TrialSpec};
use crate::pack::{
    declared_paths, digest_declared_assets, install_pack_documents, load_pack_install_config,
    BoundPackConfig, DriftPolicy, InstalledVia, PackIdentity, PackInferenceBindings,
    PackInstallOptions, PackManifest,
};
use crate::store::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::store::graphql::{escape_graphql_string, validate_collection_identifier};
use crate::{Collection, ConfigAccess};

pub(super) async fn install(
    spec: &TrialSpec,
    home: &EmbeddedHome,
    workspace: &Path,
) -> Result<String> {
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
    let evaluator =
        crate::store::identity::RegisteredIdentity::from_registered_did(&spec.evaluator_did, None)
            .context(
                "the evaluator signing identity must be available before installing a trial",
            )?;
    if home.node_acp_enabled() {
        grant_trial_operations(home, &spec.evaluator_did).await?;
    }
    let access = ConfigAccess::local_as(
        home.node.clone(),
        ::identity::Did::new(spec.evaluator_did.clone())?,
    );
    let config = load_pack_install_config(
        &access,
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
        InstalledVia::Operator,
    )
    .await?;
    let mut identity = PackIdentity::new(&manifest, &digest, Vec::new());
    identity.installation_doc_id = config.installation_doc_id.clone();
    identity.agent_bindings = config.agent_bindings.clone();
    let agent_did = config
        .agent_bindings
        .get(&spec.agent_slot)
        .with_context(|| format!("pack has no installed agent slot {:?}", spec.agent_slot))?
        .agent_did
        .clone();
    if home.node_acp_enabled() {
        for binding in identity.agent_bindings.values() {
            grant_trial_operations(home, &binding.agent_did).await?;
        }
    }
    let config = bind_inference_slots(&manifest, &config, &spec.inference)
        .context("binding the pack's inference slots to the frozen profile")?;
    let config = root_host_tools(config, workspace)?;
    drop(evaluator);
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
    // The Agent parent must exist under the evaluator before the pack is installed.
    let mut trial_node = config.node.clone();
    trial_node.default_agent_did = None;
    apply(
        &access,
        "eval.trial.create_node",
        &DesiredStateApplyPlan::new(vec![desired(
            Collection::Node,
            serde_json::to_value(trial_node)?,
        )])?,
    )
    .await
    .context("creating the evaluator-owned trial node")?;
    let node_doc = access
        .transact_readonly("eval.trial.node_owner", |txn| {
            let node_did = &node_did;
            Box::pin(async move {
                crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    Collection::Node,
                    node_did,
                    node_did,
                )
                .await?
                .map(|(doc, _)| doc)
                .context("created trial node is unavailable")
            })
        })
        .await?;
    access
        .add_actor_relationship(
            "Node",
            &node_doc,
            "administrator",
            &::identity::Did::new(&spec.evaluator_did)?,
        )
        .await
        .context("granting the evaluator trial-node administration")?;
    // A reference is only installable once what it names exists.
    apply(
        &access,
        "eval.trial.install_inference",
        &inference_plan(&spec.inference, &spec.trial_id, &node_did)?,
    )
    .await
    .context("installing the trial inference binding")?;
    install_pack_documents(&access, &node_did, &identity, &config, DriftPolicy::Refuse)
        .await
        .context("installing the trial pack")?;
    let mut selected = config.node.clone();
    selected.default_agent_did = Some(agent_did.clone());
    apply(
        &access,
        "eval.trial.select_agent",
        &DesiredStateApplyPlan::new(vec![desired(
            Collection::Node,
            serde_json::to_value(selected)?,
        )])?,
    )
    .await?;
    install_workspace_root(&access, workspace).await?;
    let mut fixtures = spec.fixtures.clone();
    for fixture in &mut fixtures.documents {
        crate::pack::bind_agent_references(&mut fixture.document, &identity.agent_bindings)?;
        fixture.document = bind_fixture_actors(&fixture.document, &node_did, &spec.evaluator_did);
        for reader in &mut fixture.session_readers {
            *reader = identity
                .agent_bindings
                .get(reader)
                .with_context(|| {
                    format!("fixture session reader references unknown agent slot {reader:?}")
                })?
                .agent_did
                .clone();
        }
    }
    if home.node_acp_enabled() {
        let mut owners = std::collections::BTreeSet::new();
        for fixture in &fixtures.documents {
            if fixture.session_agent_owned {
                let agent = fixture.document["agent_did"]
                    .as_str()
                    .context("agent-owned fixture session has no agent")?;
                if owners.insert(agent) {
                    grant_trial_operations(home, agent).await?;
                }
            }
        }
    }
    install_fixtures(&access, &fixtures, workspace, &node_did).await?;
    Ok(agent_did)
}

async fn grant_trial_operations(home: &EmbeddedHome, target: &str) -> Result<()> {
    use crate::store::config_client::NodePermission;

    let owner = ConfigAccess::local_as(home.node.clone(), ::identity::Did::new(home.did())?);
    let target = ::identity::Did::new(target)?;
    for permission in [
        NodePermission::CollectionGet,
        NodePermission::CollectionPatch,
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DocumentDelete,
        NodePermission::DacRelationAdd,
        NodePermission::DacRelationDelete,
    ] {
        owner.grant_node_permission(&target, permission).await?;
    }
    Ok(())
}

fn bind_inference_slots(
    manifest: &PackManifest,
    config: &BoundPackConfig,
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
    config.bind_inference(manifest, &bindings)
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

async fn install_workspace_root(access: &ConfigAccess, workspace: &Path) -> Result<()> {
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
    access
        .write("eval.trial.workspace_root", &mutation)
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
        let mut document = bind_fixture_owner(&fixture.document, trial_did);
        if fixture.expired_client_request {
            anyhow::ensure!(
                fixture.collection == "AgentRequest"
                    && fixture.session_readers.is_empty()
                    && !fixture.session_agent_owned,
                "expired client request publication requires an AgentRequest fixture"
            );
            expired_request::install(access, &document, trial_did).await?;
            continue;
        }
        if fixture.collection == "AgentSession" {
            anyhow::ensure!(
                document["node_did"] == trial_did,
                "fixture session must belong to the trial node"
            );
            object(&mut document, "AgentSession")?
                .insert("relationships_pending".into(), json!(true));
        } else {
            anyhow::ensure!(
                fixture.session_readers.is_empty() && !fixture.session_agent_owned,
                "session ownership and readers require an AgentSession fixture"
            );
        }
        let agent_access = if fixture.session_agent_owned {
            let did = document["agent_did"]
                .as_str()
                .context("agent-owned fixture session has no agent")?;
            let node = access
                .local_node()
                .context("agent-owned fixture requires a local trial")?
                .clone();
            Some(ConfigAccess::local_as(node, ::identity::Did::new(did)?))
        } else {
            None
        };
        let creator = agent_access.as_ref().unwrap_or(access);
        let doc_id = create_document(
            creator,
            "eval.trial.fixture_document",
            &fixture.collection,
            &document,
        )
        .await?;
        if fixture.collection == "AgentSession" {
            let agent_did = document["agent_did"]
                .as_str()
                .context("fixture session is missing its agent")?;
            let agent_doc = access
                .transact_readonly("eval.trial.fixture_agent", |txn| {
                    Box::pin(async move {
                        crate::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            Collection::Agent,
                            trial_did,
                            agent_did,
                        )
                        .await?
                        .map(|(id, _)| id)
                        .context("fixture session agent is unavailable")
                    })
                })
                .await?;
            crate::session::grant_created_session_relationships(
                creator, &doc_id, &agent_doc, agent_did,
            )
            .await?;
            for reader in &fixture.session_readers {
                creator
                    .add_actor_relationship(
                        "AgentSession",
                        &doc_id,
                        "reader",
                        &::identity::Did::new(reader)?,
                    )
                    .await?;
            }
        }
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
    let plan = crate::store::config_client::preview_schema_install(access, sdl).await?;
    crate::store::config_client::apply_schema_install(access, sdl, &plan.artifact_digest).await?;
    Ok(())
}

pub(crate) fn bind_fixture_owner(value: &Value, trial_did: &str) -> Value {
    bind_fixture_token(value, "$trial", trial_did)
}

pub(super) fn bind_fixture_actors(value: &Value, trial_did: &str, evaluator_did: &str) -> Value {
    bind_fixture_token(
        &bind_fixture_owner(value, trial_did),
        "$evaluator",
        evaluator_did,
    )
}

fn bind_fixture_token(value: &Value, token: &str, replacement: &str) -> Value {
    match value {
        Value::String(text) if text == token => Value::String(replacement.into()),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|v| bind_fixture_token(v, token, replacement))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), bind_fixture_token(value, token, replacement)))
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
                crate::store::graphql::created_doc_id(&response, collection)
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
