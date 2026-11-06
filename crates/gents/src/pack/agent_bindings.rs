use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use gents_protocol::agent_identity::AgentIdentityBinding;
use serde_json::Value;

use super::{InstalledVia, PackInstallOptions, PackManifest};
use crate::documents::PackConfig;
use crate::store::config_client::ConfigAccess;

pub(crate) struct AgentProvisioner<'a> {
    pub node: &'a std::sync::Arc<defra_node::Node>,
    pub runtime: &'a dyn crate::store::identity::NodeIdentity,
    pub actor: Option<&'a dyn crate::store::identity::NodeIdentity>,
    pub selected_agent_did: Option<&'a str>,
    pub node_binding: Option<&'a crate::store::config_client::ConfigurationNodeBinding>,
}

pub(super) struct PackBindingInputs<'a, 'b> {
    pub available: &'a BTreeMap<String, PackAgentBindings>,
    pub provisioner: Option<&'a AgentProvisioner<'b>>,
    pub hosted: Option<&'a PackAgentBindings>,
}

pub type PackAgentBindings = BTreeMap<String, AgentIdentityBinding>;
pub const AGENT_SLOT_REFERENCE_PREFIX: &str = "gents:agent-slot:";

pub fn agent_slot_reference(slot: &str) -> String {
    format!("{AGENT_SLOT_REFERENCE_PREFIX}{slot}")
}

#[derive(Debug, Clone)]
pub struct BoundPackConfig {
    pub installation_doc_id: Option<String>,
    pub config: PackConfig,
    pub agent_bindings: PackAgentBindings,
}

impl BoundPackConfig {
    pub fn bind_inference(
        &self,
        manifest: &PackManifest,
        requested: &super::PackInferenceBindings,
    ) -> Result<PackConfig> {
        let manifest = manifest_with_agent_bindings(manifest, &self.agent_bindings);
        super::bind_pack_install_config(&manifest, &self.config, requested)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AgentBindingPlan {
    pub selected: PackAgentBindings,
    pub recorded: PackAgentBindings,
}

pub(super) fn validate_bindings(bindings: &PackAgentBindings) -> Result<()> {
    let mut dids = BTreeSet::new();
    let mut nonces = BTreeSet::new();
    for (slot, binding) in bindings {
        anyhow::ensure!(
            !slot.is_empty() && !binding.agent_did.is_empty() && !binding.creation_nonce.is_empty(),
            "agent binding fields must not be empty"
        );
        anyhow::ensure!(
            dids.insert(&binding.agent_did) && nonces.insert(&binding.creation_nonce),
            "agent slots must have distinct DIDs and creation nonces"
        );
    }
    Ok(())
}

pub(crate) fn plan_agent_bindings(
    recorded: &PackAgentBindings,
    requested: &[String],
    provisioned: &PackAgentBindings,
) -> Result<AgentBindingPlan> {
    validate_bindings(recorded)?;
    validate_bindings(provisioned)?;
    let mut selected = PackAgentBindings::new();
    let mut next = recorded.clone();
    for slot in requested {
        anyhow::ensure!(
            !selected.contains_key(slot),
            "duplicate agent slot {slot:?}"
        );
        let binding = recorded
            .get(slot)
            .or_else(|| provisioned.get(slot))
            .with_context(|| format!("agent slot {slot:?} needs runtime identity provisioning"))?
            .clone();
        selected.insert(slot.clone(), binding.clone());
        next.insert(slot.clone(), binding);
    }
    validate_bindings(&next)?;
    Ok(AgentBindingPlan {
        selected,
        recorded: next,
    })
}

pub(super) fn manifest_with_agent_bindings(
    manifest: &PackManifest,
    bindings: &PackAgentBindings,
) -> PackManifest {
    let mut bound = manifest.clone();
    for slot in &mut bound.metadata.inference_slots {
        for agent in &mut slot.agents {
            if let Some(binding) = bindings.get(agent) {
                *agent = binding.agent_did.clone();
            }
        }
    }
    bound
}

pub(super) fn authored_slots(value: &Value) -> Result<Vec<String>> {
    let mut slots = Vec::new();
    let Some(agents) = value.get("agents").filter(|v| !v.is_null()) else {
        return Ok(slots);
    };
    for agent in agents
        .as_array()
        .context("agents must be an array or null")?
    {
        let object = agent
            .as_object()
            .context("authored agent must be an object")?;
        anyhow::ensure!(!object.contains_key("agent_did") && !object.contains_key("creation_nonce"), "pack agents declare agent_slot; the runtime provisions agent_did and creation_nonce during installation");
        let slot = object
            .get("agent_slot")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty() && !s.contains(['#', ':', '/']))
            .context("pack agent requires a nonempty agent_slot")?;
        anyhow::ensure!(
            !slots.iter().any(|prior| prior == slot),
            "duplicate agent slot {slot:?}"
        );
        slots.push(slot.to_owned());
    }
    Ok(slots)
}

pub(crate) fn rewrite_references(value: &mut Value, bindings: &PackAgentBindings) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (field, value) in object {
                if matches!(
                    field.as_str(),
                    "agent_did" | "default_agent_did" | "writer_agent_did" | "integrator_agent_did"
                ) {
                    if let Some(slot) = value
                        .as_str()
                        .and_then(|v| v.strip_prefix(AGENT_SLOT_REFERENCE_PREFIX))
                    {
                        *value = bindings
                            .get(slot)
                            .with_context(|| format!("unknown agent slot reference {slot:?}"))?
                            .agent_did
                            .clone()
                            .into();
                    }
                } else if value
                    .as_str()
                    .is_some_and(|v| v.starts_with(AGENT_SLOT_REFERENCE_PREFIX))
                {
                    anyhow::bail!("agent slot reference is not permitted in {field:?}");
                }
                rewrite_references(value, bindings)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                rewrite_references(value, bindings)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn bind_authored_agents(value: &mut Value, bindings: &PackAgentBindings) -> Result<()> {
    let slots = authored_slots(value)?;
    if let Some(agents) = value.get_mut("agents").and_then(Value::as_array_mut) {
        for (agent, slot) in agents.iter_mut().zip(slots) {
            let binding = bindings
                .get(&slot)
                .with_context(|| format!("agent slot {slot:?} is unbound"))?;
            let object = agent
                .as_object_mut()
                .context("authored agent must be an object")?;
            object.remove("agent_slot");
            object.insert("agent_did".into(), binding.agent_did.clone().into());
            object.insert(
                "creation_nonce".into(),
                binding.creation_nonce.clone().into(),
            );
        }
    }
    rewrite_references(value, bindings)
}

pub async fn load_pack_install_config(
    access: &ConfigAccess,
    manifest: &PackManifest,
    options: &PackInstallOptions,
    read_asset: &(dyn Fn(&str) -> Result<Vec<u8>> + Sync),
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
    installed_via: InstalledVia,
) -> Result<BoundPackConfig> {
    load_pack_install_config_with_provisioner(
        access,
        manifest,
        options,
        read_asset,
        environment,
        installed_via,
        None,
    )
    .await
}

pub(crate) async fn load_pack_install_config_with_provisioner(
    access: &ConfigAccess,
    manifest: &PackManifest,
    options: &PackInstallOptions,
    read_asset: &(dyn Fn(&str) -> Result<Vec<u8>> + Sync),
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
    installed_via: InstalledVia,
    provisioner: Option<&AgentProvisioner<'_>>,
) -> Result<BoundPackConfig> {
    let mut available = BTreeMap::new();
    for coordinate in declared_dependencies(manifest) {
        let (_, bindings) =
            super::installation::read_agent_bindings(access, &options.node_did, &coordinate)
                .await?;
        if !bindings.is_empty() {
            available.insert(coordinate, bindings);
        }
    }
    load_pack_install_config_with_dependencies(
        access,
        manifest,
        options,
        read_asset,
        environment,
        installed_via,
        PackBindingInputs {
            available: &available,
            provisioner,
            hosted: None,
        },
    )
    .await
}

pub(super) async fn load_pack_install_config_with_dependencies(
    access: &ConfigAccess,
    manifest: &PackManifest,
    options: &PackInstallOptions,
    read_asset: &(dyn Fn(&str) -> Result<Vec<u8>> + Sync),
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
    installed_via: InstalledVia,
    bindings: PackBindingInputs<'_, '_>,
) -> Result<BoundPackConfig> {
    let PackBindingInputs {
        available,
        provisioner,
        hosted,
    } = bindings;
    super::validate_pack_manifest(manifest)?;
    let mut value: Value = match manifest.config.as_deref() {
        Some(path) => serde_json::from_slice(&read_asset(path)?)?,
        None if matches!(
            manifest.metadata.kind,
            super::PackKind::Plugins | super::PackKind::Assets
        ) =>
        {
            serde_json::json!({"node": {}})
        }
        None => anyhow::bail!("pack has no configuration"),
    };
    let slots = authored_slots(&value)?;
    let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
    let (installation_doc_id, recorded) =
        super::installation::read_agent_bindings(access, &options.node_did, &coordinate).await?;
    let mut provisioned = PackAgentBindings::new();
    for slot in &slots {
        if recorded.contains_key(slot) {
            continue;
        }
        let binding = if let Some(hosted) = hosted {
            hosted
                .get(slot)
                .with_context(|| format!("hosted pack is missing agent slot {slot:?}"))?
                .clone()
        } else if installed_via == InstalledVia::SelfConfig {
            let provisioner = provisioner.context(
                "pack agent creation requires the selected Agent signer and runtime identity",
            )?;
            crate::store::identity::provision_agent_identity_for_agent(
                provisioner.node,
                provisioner.runtime,
                provisioner
                    .actor
                    .context("pack installation requires the selected Agent signer")?,
                provisioner
                    .selected_agent_did
                    .context("pack installation requires the selected Agent DID")?,
            )
            .await?
        } else {
            match access {
                ConfigAccess::Local(local) => {
                    anyhow::ensure!(
                        local.node().node_identity_did() == Some(options.node_did.as_str()),
                        "agent provisioning requires the selected node's runtime"
                    );
                    let root = provisioner
                        .context("local pack agent creation requires the held runtime identity")?
                        .runtime;
                    anyhow::ensure!(
                        root.did() == options.node_did,
                        "runtime identity does not match the selected Node"
                    );
                    let caller = local
                        .identity()
                        .context("local pack agent creation requires an authenticated caller")?;
                    crate::store::identity::provision_agent_identity_authorized(
                        local.node(),
                        root,
                        caller,
                    )
                    .await?
                }
                ConfigAccess::Graphql(endpoint) => {
                    crate::store::identity::provision_agent_identity_remote(
                        endpoint,
                        &options.node_did,
                    )
                    .await?
                }
            }
        };
        provisioned.insert(slot.clone(), binding);
    }
    let plan = plan_agent_bindings(&recorded, &slots, &provisioned)?;
    if let Some(hosted) = hosted {
        anyhow::ensure!(
            *hosted == plan.recorded,
            "hosted agent bindings differ from the installation plan"
        );
    }
    let declared = declared_dependencies(manifest);
    validate_slot_references(&value, &slots, &declared, None)?;
    let mut references = plan.selected.clone();
    for dependency in &declared {
        if let Some(bindings) = available.get(dependency) {
            for slot in bindings.keys() {
                let binding = resolve_dependency_binding(&declared, available, dependency, slot)?;
                references.insert(format!("{dependency}#{slot}"), binding.clone());
            }
        }
    }
    bind_authored_agents(&mut value, &references)?;
    let bound_manifest = manifest_with_agent_bindings(manifest, &plan.selected);
    let config = super::loader::load_pack_config_value(
        &bound_manifest,
        options,
        value,
        read_asset,
        environment,
    )?;
    Ok(BoundPackConfig {
        installation_doc_id,
        config,
        agent_bindings: plan.recorded,
    })
}

/// An offline view keeps agent templates as JSON; only the remaining document
/// kinds are decoded. Provisioning is required before an Agent can be decoded.
#[derive(Clone, Debug)]
pub struct AuthoredPackConfig {
    pub value: Value,
    pub documents: PackConfig,
}

impl AuthoredPackConfig {
    pub fn agent(&self, slot: &str) -> Option<&Value> {
        self.value
            .get("agents")?
            .as_array()?
            .iter()
            .find(|agent| agent.get("agent_slot").and_then(Value::as_str) == Some(slot))
    }

    pub fn document_digests(&self) -> Result<BTreeMap<String, String>> {
        let mut digests = super::pack_document_digests(&self.documents)?
            .into_iter()
            .map(|((collection, id), digest)| (format!("{collection}/{id}"), digest))
            .collect::<BTreeMap<_, _>>();
        if let Some(agents) = self.value.get("agents").and_then(Value::as_array) {
            for agent in agents {
                let slot = agent["agent_slot"]
                    .as_str()
                    .context("authored agent has no slot")?;
                digests.insert(
                    format!("AgentSlot/{slot}"),
                    super::pack_artifact_document_digest(agent)?,
                );
            }
        }
        Ok(digests)
    }
}

pub fn load_pack_authoring_config(
    manifest: &PackManifest,
    options: &PackInstallOptions,
    read_asset: &dyn Fn(&str) -> Result<Vec<u8>>,
    environment: &dyn Fn(&str) -> Option<String>,
) -> Result<AuthoredPackConfig> {
    super::validate_pack_manifest(manifest)?;
    let mut value: Value = match manifest.config.as_deref() {
        Some(path) => serde_json::from_slice(&read_asset(path)?)?,
        None if matches!(
            manifest.metadata.kind,
            super::PackKind::Plugins | super::PackKind::Assets
        ) =>
        {
            serde_json::json!({"node": {}})
        }
        None => anyhow::bail!("pack has no configuration"),
    };
    let original = value.clone();
    super::loader::interpolate_values(&mut value, &|name| {
        if name == "GENTS_PACK_NODE_DID" {
            Some(options.node_did.clone())
        } else {
            environment(name)
        }
    })?;
    let slots = authored_slots(&value)?;
    let declared = manifest
        .metadata
        .inference_slots
        .iter()
        .flat_map(|slot| {
            slot.agents
                .iter()
                .map(move |agent| (agent.as_str(), slot.name.as_str()))
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        declared.len() == slots.len(),
        "every authored agent slot must appear in exactly one inference slot"
    );
    let schema = query::parse_sdl(gents_protocol::schemas::AGENT)?.remove(0);
    let agents = value
        .get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (agent, slot) in agents.iter().zip(&slots) {
        let object = agent.as_object().context("agent must be an object")?;
        let assigned = declared
            .iter()
            .filter(|(agent, _)| *agent == slot)
            .collect::<Vec<_>>();
        anyhow::ensure!(
            assigned.len() == 1,
            "agent slot {slot:?} must appear in exactly one inference slot"
        );
        anyhow::ensure!(
            agent["inference_profile_id"].as_str()
                == Some(super::inference_slot_reference(assigned[0].1).as_str()),
            "agent slot {slot:?} must use its declared inference slot"
        );
        for (field, value) in object {
            if field == "agent_slot" {
                continue;
            }
            anyhow::ensure!(
                !matches!(field.as_str(), "updated_at" | "_docID"),
                "agent template cannot author {field}"
            );
            let definition = schema
                .fields
                .iter()
                .find(|definition| definition.name == *field)
                .with_context(|| format!("unknown agent template field {field:?}"))?;
            if value.is_null() {
                continue;
            }
            let valid = match &definition.kind {
                schema::FieldKind::Scalar(
                    schema::ScalarKind::Bool | schema::ScalarKind::NillableBool,
                ) => value.is_boolean(),
                schema::FieldKind::ScalarArray(_) => value
                    .as_array()
                    .is_some_and(|values| values.iter().all(Value::is_string)),
                _ => value.is_string(),
            };
            anyhow::ensure!(valid, "invalid value for agent template field {field:?}");
        }
        if let Some(owner) = object.get("node_did") {
            anyhow::ensure!(
                owner.as_str() == Some(options.node_did.as_str()),
                "agent template owner does not match installation scope"
            );
        }
    }
    validate_slot_references(&value, &slots, &declared_dependencies(manifest), None)?;
    let mut non_agents = original;
    non_agents
        .as_object_mut()
        .context("pack must be an object")?
        .remove("agents");
    let mut non_agent_manifest = manifest.clone();
    for inference in &mut non_agent_manifest.metadata.inference_slots {
        inference.agents.clear();
    }
    let documents = super::loader::load_pack_config_value(
        &non_agent_manifest,
        options,
        non_agents,
        read_asset,
        environment,
    )?;
    crate::store::config_client::DesiredStateApplyPlan::from_pack_config(&documents)?;
    let mut hydrated = serde_json::to_value(&documents)?;
    if !agents.is_empty() {
        hydrated["agents"] = agents.into();
    }
    Ok(AuthoredPackConfig {
        value: hydrated,
        documents,
    })
}

fn validate_slot_references(
    value: &Value,
    slots: &[String],
    declared: &[String],
    field: Option<&str>,
) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (field, value) in object {
                validate_slot_references(value, slots, declared, Some(field))?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_slot_references(value, slots, declared, field)?;
            }
        }
        Value::String(value) => {
            if let Some(slot) = value.strip_prefix(AGENT_SLOT_REFERENCE_PREFIX) {
                anyhow::ensure!(
                    matches!(
                        field,
                        Some(
                            "agent_did"
                                | "default_agent_did"
                                | "writer_agent_did"
                                | "integrator_agent_did"
                        )
                    ),
                    "agent slot references are only valid in agent DID fields"
                );
                if let Some((dependency, target)) = slot.split_once('#') {
                    anyhow::ensure!(!target.is_empty() && !target.contains('#') && declared.iter().any(|coordinate| coordinate == dependency), "agent slot reference {slot:?} must name a declared direct dependency and slot");
                } else {
                    anyhow::ensure!(
                        slots.iter().any(|candidate| candidate == slot),
                        "unknown agent slot {slot:?}"
                    );
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn declared_dependencies(manifest: &PackManifest) -> Vec<String> {
    manifest
        .metadata
        .dependencies
        .iter()
        .map(|coordinate| {
            let (namespace, name) = super::registry::split_pack_coordinate(coordinate);
            format!("{namespace}/{name}")
        })
        .collect()
}

pub(super) fn resolve_dependency_binding<'a>(
    declared: &[String],
    available: &'a BTreeMap<String, PackAgentBindings>,
    dependency: &str,
    slot: &str,
) -> Result<&'a AgentIdentityBinding> {
    anyhow::ensure!(
        declared.iter().any(|coordinate| coordinate == dependency),
        "agent reference names undeclared dependency {dependency:?}"
    );
    let bindings = available.get(dependency).with_context(|| {
        format!("dependency {dependency:?} has no prepared or installed agent bindings")
    })?;
    validate_bindings(bindings)?;
    bindings
        .get(slot)
        .with_context(|| format!("dependency {dependency:?} has no agent slot {slot:?}"))
}

#[cfg(test)]
pub(in crate::pack) mod tests;
