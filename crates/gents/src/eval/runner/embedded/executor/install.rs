mod expired_request;

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::documents::{InferenceSampling, PackConfig};
use crate::eval::runner::embedded::home::EmbeddedHome;
use crate::eval::runner::executor::{InferenceBinding, TrialFixtures, TrialSpec};
use crate::pack::{
    declared_paths, digest_declared_assets, install_pack_documents,
    load_pack_install_config_with_provisioner, BoundPackConfig, DriftPolicy, InstalledVia,
    PackIdentity, PackInferenceBindings, PackInstallOptions, PackManifest,
};
use crate::store::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::store::graphql::validate_collection_identifier;
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
    let evaluator = home.evaluator(&spec.evaluator_did)?;
    let schema_access = ConfigAccess::local_as(home.node.clone(), ::identity::Did::new(&node_did)?);
    if home.node_acp_enabled() {
        grant_trial_operations(home, &spec.evaluator_did).await?;
    }
    let planning_access = ConfigAccess::local_as(
        home.node.clone(),
        ::identity::Did::new(spec.evaluator_did.clone())?,
    );
    let access = home.client_access(&spec.evaluator_did)?;
    let provisioner = crate::pack::AgentProvisioner {
        node: &home.node,
        runtime: home.identity.as_ref(),
        actor: None,
        selected_agent_did: None,
        node_binding: None,
    };
    let config = load_pack_install_config_with_provisioner(
        &planning_access,
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
        Some(&provisioner),
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
    for path in &manifest.schemas {
        let bytes = assets
            .get(path)
            .with_context(|| format!("pack has no schema asset {path:?}"))?;
        let sdl =
            std::str::from_utf8(bytes).with_context(|| format!("decoding pack schema {path:?}"))?;
        install_schema(&schema_access, sdl)
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
    access
        .transact_write("eval.trial.grant_self_configuration", |txn| {
            let node_did = &node_did;
            let agent_did = &agent_did;
            Box::pin(async move {
                crate::store::config_client::grant_selected_self_configuration(
                    txn, node_did, agent_did,
                )
                .await
            })
        })
        .await
        .context("granting the selected trial Agent self-configuration")?;
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
    let root_readers = config
        .agents
        .iter()
        .filter(|agent| {
            agent.enabled
                && identity
                    .agent_bindings
                    .values()
                    .any(|binding| binding.agent_did == agent.agent_did)
        })
        .map(|agent| agent.agent_did.clone())
        .collect::<Vec<_>>();
    install_workspace_root(&access, &node_did, workspace, &root_readers).await?;
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
    install_fixtures(
        &access,
        &fixtures,
        workspace,
        home,
        &identity.agent_bindings,
        evaluator,
        Some((&manifest, &assets)),
    )
    .await?;
    Ok(agent_did)
}

async fn grant_trial_operations(home: &EmbeddedHome, target: &str) -> Result<()> {
    use crate::store::config_client::NodePermission;

    let owner = ConfigAccess::local_as(home.node.clone(), ::identity::Did::new(home.did())?);
    let target = ::identity::Did::new(target)?;
    for permission in [
        NodePermission::CollectionGet,
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

async fn install_workspace_root(
    access: &ConfigAccess,
    node_did: &str,
    workspace: &Path,
    reader_agents: &[String],
) -> Result<()> {
    let root = workspace
        .to_str()
        .with_context(|| format!("{} is not UTF-8", workspace.display()))?;
    crate::tools::surface::set_workspace_root_with_readers(
        access,
        node_did,
        root,
        Some("Eval trial workspace"),
        true,
        reader_agents,
    )
    .await
    .context("publishing the trial workspace root")?;
    Ok(())
}

async fn install_fixtures(
    access: &ConfigAccess,
    fixtures: &TrialFixtures,
    workspace: &Path,
    home: &EmbeddedHome,
    bindings: &crate::pack::PackAgentBindings,
    evaluator: &dyn crate::store::identity::NodeIdentity,
    pack: Option<(&PackManifest, &BTreeMap<String, Vec<u8>>)>,
) -> Result<()> {
    let trial_did = home.did();
    let schema_access = ConfigAccess::local_as(home.node.clone(), ::identity::Did::new(trial_did)?);
    for sdl in &fixtures.schemas {
        install_schema(&schema_access, sdl)
            .await
            .context("installing a fixture schema")?;
    }
    for fixture in &fixtures.documents {
        let document = bind_fixture_owner(&fixture.document, trial_did);
        if fixture.expired_client_request {
            anyhow::ensure!(
                fixture.collection == "AgentRequest"
                    && fixture.session_readers.is_empty()
                    && !fixture.session_agent_owned,
                "expired client request publication requires an AgentRequest fixture"
            );
            let agent_did = document["agent_did"]
                .as_str()
                .context("expired request fixture has no selected agent")?;
            let binding = bindings
                .values()
                .find(|binding| binding.agent_did == agent_did)
                .context("expired request fixture references an unknown agent binding")?;
            let agent =
                crate::store::identity::resolve_agent_identity(home.identity.as_ref(), binding)?;
            expired_request::install(access, &document, home, evaluator, agent.as_ref()).await?;
            continue;
        }
        if fixture.collection == "AgentSession" {
            anyhow::ensure!(
                document["node_did"] == trial_did,
                "fixture session must belong to the trial node"
            );
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
            let binding = bindings
                .values()
                .find(|binding| binding.agent_did == did)
                .context("agent-owned fixture references an unknown agent binding")?;
            let identity =
                crate::store::identity::resolve_agent_identity(home.identity.as_ref(), binding)?;
            Some(ConfigAccess::local_managed_agent(
                home.node.clone(),
                ::identity::Did::new(did)?,
                identity.as_ref(),
            )?)
        } else {
            None
        };
        let creator = agent_access.as_ref().unwrap_or(access);
        if fixture.collection == "AgentSession" {
            install_session_fixture(creator, &document, &fixture.session_readers).await?;
            continue;
        }
        let schema = match pack {
            Some((manifest, assets)) => Some(trusted_fixture_schema(
                manifest,
                assets,
                fixtures,
                &fixture.collection,
            )?),
            None => None,
        };
        create_document(
            creator,
            "eval.trial.fixture_document",
            &fixture.collection,
            &document,
            schema.as_deref(),
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

async fn install_session_fixture(
    access: &ConfigAccess,
    document: &Value,
    readers: &[String],
) -> Result<()> {
    let session: gents_protocol::session::AgentSession = serde_json::from_value(document.clone())?;
    crate::session::validate_agent_session(&session)?;
    let actor = access
        .actor_did()
        .context("fixture session requires an authenticated creator")?;
    anyhow::ensure!(
        session
            .requester_did
            .as_deref()
            .unwrap_or(&session.agent_did)
            == actor,
        "fixture session creator does not match its requester or owned Agent"
    );
    let mut input = document.clone();
    let fields = object(&mut input, "AgentSession")?;
    fields.insert("relationships_pending".into(), json!(false));
    if fields
        .get("tags")
        .is_some_and(|value| value.as_array().is_some_and(Vec::is_empty))
    {
        fields.insert("tags".into(), Value::Null);
    }
    let target = ::identity::Did::new(&session.agent_did)?;
    let readers = readers
        .iter()
        .map(::identity::Did::new)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    access.transact_write("eval.trial.fixture_session", |txn| {
        let (session, input, target, readers) = (&session, &input, &target, &readers);
        Box::pin(async move {
            let (agent_doc, _) = crate::store::config_client::read_desired_state_record_in_txn(
                txn, Collection::Agent, &session.node_did, &session.agent_did,
            ).await?.context("fixture session agent is unavailable")?;
            let response = txn.execute_client_signed_create(
                "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                &json!({"input": input}), gents_protocol::schemas::AGENT_SESSION,
            ).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "AgentSession")?;
            crate::session::stage_session_relationships_in_txn(txn, &doc, &agent_doc, target).await?;
            for reader in readers {
                txn.add_actor_relationship("AgentSession", &doc, "reader", reader).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
    }).await
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
    schema: Option<&str>,
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
                let response = if txn.client_signed_configuration() {
                    let schema = schema
                        .context("client fixture creation requires its trusted authored schema")?;
                    txn.execute_client_signed_create(mutation, variables, schema)
                        .await?
                } else {
                    txn.execute_with_variables(mutation, variables).await?
                };
                Ok::<_, anyhow::Error>(crate::store::graphql::created_doc_id(
                    &response, collection,
                )?)
            })
        })
        .await
        .with_context(|| format!("creating a {collection} document"))
}

pub(super) fn trusted_seed_schema(spec: &TrialSpec, collection: &str) -> Result<String> {
    let (manifest, assets) = read_pack(&spec.pack_dir)?;
    let digest = digest_declared_assets(&manifest, |path| {
        assets
            .get(path)
            .map(Vec::as_slice)
            .with_context(|| format!("pack has no asset {path:?}"))
    })?;
    anyhow::ensure!(
        digest == spec.pack_digest,
        "pack digest changed before seeding the trial"
    );
    trusted_fixture_schema(&manifest, &assets, &spec.fixtures, collection)
}

fn trusted_fixture_schema(
    manifest: &PackManifest,
    assets: &BTreeMap<String, Vec<u8>>,
    fixtures: &TrialFixtures,
    collection: &str,
) -> Result<String> {
    let mut selected = None;
    let pack_schemas = manifest
        .schemas
        .iter()
        .map(|path| {
            let bytes = assets
                .get(path)
                .with_context(|| format!("pack has no schema asset {path:?}"))?;
            std::str::from_utf8(bytes).with_context(|| format!("decoding pack schema {path:?}"))
        })
        .collect::<Result<Vec<_>>>()?;
    for sdl in gents_protocol::schemas::ALL
        .iter()
        .copied()
        .chain(fixtures.schemas.iter().map(String::as_str))
        .chain(pack_schemas)
    {
        let schemas = query::parse_sdl(sdl)?;
        if schemas.iter().any(|schema| schema.name == collection) {
            anyhow::ensure!(
                schemas.len() == 1,
                "fixture schema for {collection} must declare exactly one collection"
            );
            anyhow::ensure!(
                selected.is_none(),
                "fixture schema for {collection} is ambiguous"
            );
            selected = Some(sdl.to_owned());
        }
    }
    selected
        .with_context(|| format!("fixture collection {collection} has no trusted authored schema"))
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
