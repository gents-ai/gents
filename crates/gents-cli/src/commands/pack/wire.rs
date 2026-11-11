//! `gents config pack wire`: the command over the plugin wiring writer, and
//! the host tool roots an installed pack's agents read.

use anyhow::{Context, Result};
use gents_pack::pack::installed_pack_documents;
use gents_pack::pack::plugin_wire::{wire_in, WireRequest};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use serde_json::{json, Value};

use crate::cli::PackWireArgs;
#[cfg(test)]
use gents_pack::pack::registry::split_pack_coordinate;

pub(crate) async fn wire(args: PackWireArgs) -> Result<()> {
    let (access, owner) = super::resolve_scope_owner(&args.scope).await?;
    let home = gents_service::home_state::resolve_home_dir(args.scope.home.as_deref());
    let request = WireRequest {
        plugin: args.plugin,
        agent: args.agent,
        targets_from: args.targets_from,
        remove: args.remove,
        preview: args.preview,
        dynamic_consent: false,
        tool_calls_consent: false,
    };
    crate::print_json(
        &wire_in(
            &access,
            &owner,
            &home,
            &request,
            gents_service::home_state::load_held_user_identity,
        )
        .await?,
    )
}

fn publish_root_command(node_did: &str, root: &str, agent: &str) -> String {
    format!(
        "gents config workspace-root set --graphql <url> --node-did {node_did} --path {root} --reader-agent {agent}"
    )
}

/// Grants each agent the installed pack `coordinate` ships with host tools
/// reader on the published WorkspaceRoot covering its tool root, as the User
/// holding `access`: a new agent lacks that grant, only a User makes it, and
/// without it the runtime reports the agent unavailable. Returns what each
/// agent was granted, or the root nothing published covers.
pub(crate) async fn grant_pack_root_readers(
    access: &ConfigAccess,
    owner: &str,
    coordinate: &str,
) -> Result<Vec<Value>> {
    use gents_tools::surface::ToolRootReader;
    let mut granted = Vec::new();
    let agents: Vec<String> = installed_pack_documents(access, owner, coordinate)
        .await?
        .into_iter()
        .filter(|(collection, _)| *collection == Collection::Agent)
        .map(|(_, agent)| agent)
        .collect();
    for (agent, reader) in
        gents_tools::surface::agents_tool_root_readers(access, owner, &agents).await?
    {
        granted.push(match reader {
            ToolRootReader::Published(root) => {
                gents_tools::surface::grant_workspace_root_reader(access, &root, &agent)
                    .await
                    .with_context(|| format!("granting {agent} reader on {root}"))?;
                json!({"agent": agent, "reader_on": root})
            }
            ToolRootReader::Unpublished(root) => {
                let next = publish_root_command(owner, &root, &agent);
                json!({"agent": agent, "unpublished_root": root, "next": next})
            }
            ToolRootReader::NotNeeded => continue,
        });
    }
    Ok(granted)
}

#[cfg(test)]
mod next_command_tests {
    use clap::Parser as _;

    #[test]
    fn the_commands_wiring_names_parse() {
        for command in [
            gents_pack::pack::plugin_wire::explain_tools_command("did:key:agent"),
            super::publish_root_command("did:key:node", "/srv/root", "did:key:agent"),
        ] {
            let arguments = command.split(' ').map(|word| match word {
                "<url>" => "http://127.0.0.1:9191/api/v1/graphql",
                word => word,
            });
            crate::cli::Cli::try_parse_from(arguments)
                .unwrap_or_else(|error| panic!("{command}: {error}"));
        }
    }
}
