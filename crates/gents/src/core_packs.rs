//! Leaves a core pack on the Engineer as a Lean `Pack.CorePacks.Wiring`
//! says, installing it first from the release's archive: the one owner
//! behind `gents init`, `gents config pack starter --enable` and an eval
//! trial's core packs.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents_pack::pack::core_packs::{self, CorePack, Origin, Wiring};
use gents_pack::pack::plugin_wire::WireSigners;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;
use serde_json::{json, Value};

/// The core pack archives a build carries, as `(coordinate, .pack bytes)`:
/// `gents_core_packs::archives()` for the runtime itself.
pub type Archives<'a> = &'a [(&'a str, &'a [u8])];

/// Who consents to what a core pack grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// The consent Lean `approvalRequired` names for an archive the release
    /// carries: installing Gents, or setting up an eval trial's node.
    Init,
    /// The user named the pack to enable.
    Explicit,
}

/// Loads the node's runtime identity, which holds the Engineer's agent key.
/// Only installing a pack calls it: re-applying an installed one never reads
/// the key.
pub type RuntimeIdentity<'a> = &'a (dyn Fn() -> Result<Arc<dyn NodeIdentity>> + Send + Sync);

/// The node a core pack is left on, held open by the caller: its store
/// `access` as the node's init User, the `home` whose pack and plugin store
/// that node reads, the Engineer, and what the layers above supply.
pub struct CorePackTarget<'a> {
    pub access: &'a ConfigAccess,
    pub owner: &'a str,
    pub home: &'a Path,
    pub engineer: &'a str,
    pub runtime: RuntimeIdentity<'a>,
    /// Who signs the wire of the pack's plugins and skills.
    pub signers: WireSigners,
    pub registry: Option<&'a str>,
    /// The call that enables the pack, named when consent is missing.
    pub enable_with: String,
}

/// The archive of `pack` that `archives` carries.
pub fn release_archive<'a>(archives: Archives<'a>, pack: CorePack) -> Option<&'a [u8]> {
    archives
        .iter()
        .find(|(coordinate, _)| *coordinate == pack.coordinate())
        .map(|(_, bytes)| *bytes)
}

pub fn not_carried(pack: CorePack) -> String {
    format!(
        "this build carries no {} archive; a build made with `make core-packs` does",
        pack.coordinate()
    )
}

/// The archive one core pack is left from: the home's installed copy, or
/// the release's, imported into the home's pack store. Resolved from the
/// home's files alone, before the caller opens the node's store.
pub struct CorePackSource {
    pack: CorePack,
    installed: bool,
    origin: Origin,
    source: gents_pack::pack::resolved_pack::ResolvedPack,
    requested: Value,
}

pub async fn resolve(
    home: &Path,
    pack: CorePack,
    archives: Archives<'_>,
    registry: Option<&str>,
) -> Result<CorePackSource> {
    let coordinate = pack.coordinate();
    let installed = gents_pack::pack::read_home_install(home, coordinate)?;
    let (digest, origin) = match &installed {
        Some(record) => (record.digest.clone(), Origin::Other),
        None => {
            let bytes = release_archive(archives, pack)
                .with_context(|| not_carried(pack))?
                .to_vec();
            let store = home.to_owned();
            let stored = tokio::task::spawn_blocking(move || {
                gents_pack::pack::store::PackStore::new(&store).import(bytes.as_slice(), None)
            })
            .await??;
            anyhow::ensure!(
                stored.header.coordinate == coordinate,
                "the release's archive for {coordinate} is {}",
                stored.header.coordinate
            );
            (stored.header.digest, Origin::Release)
        }
    };
    let source =
        gents_pack::pack::resolved_pack::resolve_pack_source(&digest, registry, home).await?;
    let requested = gents_pack::pack::plugin_wire::requested_authority(source.manifest(), &[])?;
    Ok(CorePackSource {
        pack,
        installed: installed.is_some(),
        origin,
        source,
        requested,
    })
}

/// Leaves the resolved pack on `target`'s Engineer as `wiring` says,
/// installing it as the Engineer first when the home lacks it.
pub async fn apply(
    target: &CorePackTarget<'_>,
    resolved: &CorePackSource,
    wiring: Wiring,
    consent: Consent,
) -> Result<Value> {
    let CorePackTarget {
        access,
        owner,
        home,
        engineer,
        ..
    } = *target;
    let CorePackSource {
        pack,
        installed,
        origin,
        ref source,
        ref requested,
    } = *resolved;
    let coordinate = pack.coordinate();
    let root_agents = gents_pack::pack::listed_root_agents(source, owner)?;
    let grants =
        requested.as_array().is_some_and(|asks| !asks.is_empty()) || !root_agents.is_empty();
    anyhow::ensure!(
        consent == Consent::Explicit || !core_packs::approval_required(origin, grants),
        "{coordinate} is installed from outside this release and grants what init does not consent to. Enable it with: {}",
        target.enable_with
    );
    if !installed {
        let profile = agent_identity_row(access, owner, engineer)
            .await?
            .agent
            .inference_profile_id;
        let slots: Vec<String> = source
            .manifest()
            .metadata
            .inference_slots
            .iter()
            .filter(|slot| !slot.optional)
            .map(|slot| format!("{}={profile}", slot.name))
            .collect();
        install_as_engineer(
            engineer_config(target).await?.as_ref(),
            &format!("{coordinate}@{}", source.manifest().version),
            source.digest(),
            &slots,
        )
        .await?;
    }
    // The Engineer installed the pack, so its rootless Tools took the
    // Engineer's root and its record says which; the grant goes to the
    // agents its archive lists there, and no others.
    let shown: Vec<(String, String)> = root_agents
        .iter()
        .map(|agent| (agent.coordinate.clone(), agent.agent.clone()))
        .collect();
    // DefraDB admits a WorkspaceRoot reader only for an enabled Agent of
    // the root's Node, so an enable turns the agents on before their
    // grant and back off when it fails.
    if wiring.agents_active {
        set_pack_agents_active(access, owner, coordinate, true)
            .await
            .with_context(|| {
                format!("{coordinate} is installed, but its agents were not turned on")
            })?;
    }
    let supplied = gents_pack::pack::installer_tool_root(access, owner, Some(engineer)).await?;
    let readers = gents_tools::surface::grant_pack_root_readers(
        access,
        owner,
        &[coordinate.to_owned()],
        &shown,
        supplied.as_deref(),
        wiring.root_readers,
    )
    .await
    .with_context(|| {
        format!("{coordinate} is installed, but its agents were not granted their tool root")
    })?;
    if wiring.root_readers {
        if let Some(failed) = readers.iter().find(|reader| reader["failed"].is_string()) {
            if wiring.agents_active {
                set_pack_agents_active(access, owner, coordinate, false).await?;
            }
            anyhow::bail!(
                "{}. Enable {coordinate} again, or grant it with: {}",
                failed["failed"].as_str().unwrap_or_default(),
                failed["next"].as_str().unwrap_or_default()
            );
        }
    }
    let wired = if wiring.wired && !source.manifest().metadata.plugins.is_empty() {
        gents_pack::pack::plugin_wire::wire_pack_batch(
            access,
            owner,
            home,
            gents_pack::pack::plugin_wire::SentPack {
                archive: source,
                approval: None,
            },
            &[engineer.to_owned()],
            gents_pack::pack::plugin_wire::WireBatchOptions {
                tool_calls_consent: wiring.tool_calls,
                preview: false,
            },
            target.signers.clone(),
        )
        .await
        .with_context(|| {
            format!("{coordinate} is installed, but it was not wired into the Engineer")
        })?
    } else {
        Vec::new()
    };
    let dynamic_enabled = if wiring.consent_dynamic {
        gents_pack::pack::plugin_wire::enable_pack_dynamic(
            access,
            owner,
            home,
            engineer,
            coordinate,
            source.digest(),
            target.signers.held_user.clone(),
        )
        .await
        .with_context(|| {
            format!(
                "{coordinate} is installed and wired, but its dynamic scripts were not enabled; enabling it again is safe: {}",
                target.enable_with
            )
        })?
    } else {
        Vec::new()
    };
    let skills = if wiring.wired {
        gents_pack::pack::plugin_wire::attach_pack_skills(
            access,
            owner,
            home,
            engineer,
            coordinate,
            source.digest(),
            target.signers.held_user.clone(),
        )
        .await
        .with_context(|| {
            format!("{coordinate} is installed, but its skills were not attached to the Engineer")
        })?
    } else {
        Vec::new()
    };
    let agents = set_pack_agents_active(access, owner, coordinate, wiring.agents_active)
        .await
        .with_context(|| {
            format!("{coordinate} is installed, but its agents were not left as enabling says")
        })?;
    Ok(json!({
        "pack": coordinate,
        "version": source.manifest().version,
        "digest": source.digest(),
        "wiring": wiring,
        "granted_permissions": requested,
        "wired": wired,
        "dynamic_enabled": dynamic_enabled,
        "skills": skills,
        "host_tool_roots": readers,
        "agents": agents,
    }))
}

/// The agents the installed pack `coordinate` ships, each with whether it is
/// enabled.
pub async fn pack_agents(
    access: &ConfigAccess,
    owner: &str,
    coordinate: &str,
) -> Result<Vec<(String, bool)>> {
    let mut agents = Vec::new();
    for (collection, agent_did) in
        gents_pack::pack::installed_pack_documents(access, owner, coordinate).await?
    {
        if collection != gents_store::collection::Collection::Agent {
            continue;
        }
        let enabled = access
            .read_view(
                &gents_documents::views::agents::AgentList::new(owner)
                    .agent_did(&agent_did)
                    .limit(1),
            )
            .await?
            .first()
            .with_context(|| format!("{coordinate} lists agent {agent_did}, which is missing"))?
            .agent
            .enabled;
        agents.push((agent_did, enabled));
    }
    Ok(agents)
}

/// Enables or disables every agent the installed pack `coordinate` ships, as
/// Lean `Wiring.agentsActive` says, through the agent operation every front
/// end uses. Returns `[{agent, enabled}]`.
pub async fn set_pack_agents_active(
    access: &ConfigAccess,
    owner: &str,
    coordinate: &str,
    active: bool,
) -> Result<Vec<Value>> {
    let mut agents = Vec::new();
    for (agent_did, enabled) in pack_agents(access, owner, coordinate).await? {
        if enabled != active {
            crate::operations::agent::edit(
                access,
                owner,
                &agent_did,
                &gents_protocol::operations::agent::AgentEdit {
                    enabled: Some(active),
                    ..Default::default()
                },
            )
            .await?;
        }
        agents.push(json!({"agent": agent_did, "enabled": active}));
    }
    Ok(agents)
}

/// Installs `package` at `digest` through the Engineer's `config` tool, so the pack's
/// documents are the Engineer's: it reads them, references them and removes
/// the pack again. The consent to what the plugins ask for is the caller's;
/// the Engineer's pack authority is what grants it (Lean
/// `installerProvenance`), so the install records `operator`. Each required
/// inference slot is bound to the Engineer's own profile, deferred while its
/// backend is disabled (Lean `Pack.CorePacks.slotBinding`).
async fn install_as_engineer(
    config: &dyn gents_loop::tool::ToolDyn,
    package: &str,
    digest: &str,
    slots: &[String],
) -> Result<Value> {
    let mut argv = vec!["pack", "install", package, "--digest", digest];
    for slot in slots {
        argv.extend(["--inference-slot", slot.as_str()]);
    }
    let receipt = config
        .call(
            json!({ "argv": argv }).to_string(),
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    serde_json::from_str(&receipt).context("the Engineer's install receipt is not JSON")
}

async fn agent_identity_row(
    access: &ConfigAccess,
    owner: &str,
    agent_did: &str,
) -> Result<gents_documents::views::agents::AgentRow> {
    access
        .read_view(
            &gents_documents::views::agents::AgentList::new(owner)
                .agent_did(agent_did)
                .limit(1),
        )
        .await?
        .into_iter()
        .next()
        .with_context(|| format!("agent {agent_did:?} missing for {owner:?}"))
}

/// The Agent document's identity binding, read as the node that owns it.
pub async fn agent_identity_binding(
    access: &ConfigAccess,
    owner: &str,
    agent_did: &str,
) -> Result<gents_protocol::agent_identity::AgentIdentityBinding> {
    Ok(agent_identity_row(access, owner, agent_did)
        .await?
        .agent
        .identity_binding())
}

/// The config tool as the runtime gives it to the Engineer, with the grants
/// its own Tools hold. `target.access` holds the home's node.
pub async fn engineer_config(
    target: &CorePackTarget<'_>,
) -> Result<Box<dyn gents_loop::tool::ToolDyn>> {
    use gents_tools::builtin::self_config;
    let CorePackTarget {
        access,
        owner,
        home,
        engineer,
        ..
    } = *target;
    let node = access
        .local_node()
        .context("a runtime is serving this home; ask its Engineer to install the pack")?
        .clone();
    let binding = agent_identity_binding(access, owner, engineer).await?;
    let runtime = (target.runtime)()?;
    let agent = gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding)?;
    let held = access
        .transact_readonly("cli.init.engineer_grants", |txn| {
            Box::pin(async move {
                gents_documents::load_agent_context_and_tools_in_txn(txn, owner, engineer).await
            })
        })
        .await?
        .1
        .and_then(|tools| tools.self_config)
        .context("the Engineer's Tools hold no self_config grant")?;
    self_config::build_runtime_self_config_tools(
        Arc::default(),
        node,
        agent,
        runtime,
        &gents_tools::surface::SelfConfigToolConfig {
            enabled: held.enable_self_config.unwrap_or(false),
            agent_did: engineer.to_owned(),
            categories: held
                .self_config_categories
                .unwrap_or_default()
                .into_iter()
                .collect(),
            no_lockout: held.self_config_no_lockout.unwrap_or(false),
            preview: held.self_config_preview.unwrap_or(false),
            enable_pack_install: held.enable_pack_install.unwrap_or(false),
            enable_pack_authority: held.enable_pack_authority.unwrap_or(false),
            registry_url: Some(gents_pack::pack::registry::resolve_registry_url_for_home(
                target.registry,
                Some(target.home),
            )),
            ..Default::default()
        },
        Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::new(
            Some(home.to_owned()),
        )),
        Arc::new(gents_automation::triggers::task_run::TaskRuns),
    )
    .await?
    .into_iter()
    .find(|tool| tool.name() == gents_protocol::tool_names::CONFIG_TOOL_NAME)
    .context("the Engineer's Tools do not grant the config tool")
}
