use anyhow::{Context, Result};
use gents_documents::PackConfig;
use gents_store::{collection::Collection, config_client::ConfigAccess};

use super::documents::{
    host_tools_without_root, installer_tool_root, tools_taking_installer_root, InstallerRootAgent,
};
use super::InstalledDependencySnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum HostRootSource {
    Retained,
    Installer,
    Unbound,
}

fn host_root_source(installed_host: bool, installer_root: bool) -> HostRootSource {
    if installed_host {
        HostRootSource::Retained
    } else if installer_root {
        HostRootSource::Installer
    } else {
        HostRootSource::Unbound
    }
}

fn host_root_supplied(
    source: HostRootSource,
    previously_supplied: bool,
    matches_installer: bool,
) -> bool {
    matches_installer
        && (source == HostRootSource::Installer
            || (source == HostRootSource::Retained && previously_supplied))
}

/// Lean `Pack.Operations.hostRootSource` and `hostRootSupplied`. Root paths
/// belong to this node; selecting a default does not confer filesystem
/// authority. Runtime WorkspaceRoot admission and the User's reader grant
/// still apply. Only installer-supplied roots enter the automatic grant plan;
/// a retained matching root keeps its recorded provenance for grant retries.
pub(super) async fn default_host_tool_roots(
    access: &ConfigAccess,
    owner: &str,
    installer: Option<&str>,
    config: &mut PackConfig,
    installed: Option<&InstalledDependencySnapshot>,
) -> Result<Vec<String>> {
    let rootless = host_tools_without_root(&config.tools)?;
    if rootless.is_empty() {
        return Ok(Vec::new());
    }
    let installer_root = installer_tool_root(access, owner, installer).await?;
    let mut supplied = Vec::new();
    for index in rootless {
        let tools = &mut config.tools[index];
        let existing = installed.and_then(|snapshot| {
            snapshot
                .documents
                .get(&(Collection::Tools, tools.tools_id.clone()))
        });
        let host = existing
            .and_then(|(_, tools)| tools.get("host"))
            .filter(|host| !host.is_null());
        let source = host_root_source(host.is_some(), installer_root.is_some());
        let root = match source {
            HostRootSource::Retained => host
                .and_then(|host| host["root"].as_str())
                .map(str::to_owned),
            HostRootSource::Installer => installer_root.clone(),
            HostRootSource::Unbound => None,
        };
        let previously_supplied = installed
            .and_then(|snapshot| {
                snapshot
                    .record
                    .documents
                    .get(&super::key("Tools", &tools.tools_id))
            })
            .is_some_and(|document| document.installer_root);
        if host_root_supplied(
            source,
            previously_supplied,
            installer_root.is_some() && root == installer_root,
        ) {
            supplied.push(tools.tools_id.clone());
        }
        if let Some(host) = tools.host.as_mut() {
            host.root = root;
        }
    }
    Ok(supplied)
}

/// The agents `pack` would run under its installer's root on the node
/// `node_did`, read from its archive alone: what a starter pack's status
/// shows before any node plan exists. It is a display, not the grant's
/// source: an approval carries these rows and the grant is their
/// intersection with what the install recorded
/// (`gents_tools::surface::grant_pack_root_readers`). A config that does
/// not load is an error, never "no such agents": what was not read was not
/// shown.
pub fn listed_root_agents(
    pack: &crate::pack::resolved_pack::ResolvedPack,
    node_did: &str,
) -> Result<Vec<InstallerRootAgent>> {
    let coordinate = format!(
        "{}/{}",
        pack.manifest().metadata.namespace,
        pack.manifest().name
    );
    if pack.manifest().config.is_none() {
        return Ok(Vec::new());
    }
    crate::pack::load_pack_authoring_config(
        pack.manifest(),
        &crate::pack::PackInstallOptions {
            node_did: node_did.to_owned(),
        },
        &|path| Ok(pack.asset(path)?.to_vec()),
        &|name| std::env::var(name).ok(),
    )
    .and_then(|authored| {
        let tools = tools_taking_installer_root(&authored.documents.tools)?;
        // An authored agent names a slot, not a DID, so it is read as written.
        let rows = |key: &str| authored.value[key].as_array().into_iter().flatten();
        Ok(rows("agents")
            .filter_map(|agent| {
                let selected = rows("contexts")
                    .find(|context| {
                        context["context_id"].is_string()
                            && context["context_id"] == agent["context_id"]
                    })
                    .and_then(|context| context["tools_id"].as_str())
                    .filter(|selected| tools.iter().any(|id| id == selected))?;
                Some(InstallerRootAgent {
                    coordinate: coordinate.clone(),
                    agent: agent["agent_slot"]
                        .as_str()
                        .or(agent["agent_did"].as_str())?
                        .to_owned(),
                    name: agent["display_name"].as_str().map(ToOwned::to_owned),
                    tools: selected.to_owned(),
                })
            })
            .collect())
    })
    .with_context(|| {
        format!(
            "{coordinate} did not load for its host tool review, so the folder its agents would read is unknown"
        )
    })
}

#[cfg(test)]
mod conformance {
    use super::*;

    #[test]
    fn host_root_sources_match_lean() {
        gents_lean_vocab::families::pack_operations_host_root_source::replay(|i| {
            host_root_source(i.installed_host, i.installer_root)
        });
    }

    #[test]
    fn supplied_root_provenance_matches_lean() {
        gents_lean_vocab::families::pack_operations_host_root_supplied::replay::<HostRootSource>(
            |i| host_root_supplied(i.source, i.previously_supplied, i.matches_installer),
        );
    }
}
