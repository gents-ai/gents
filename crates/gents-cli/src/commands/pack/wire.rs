//! `gents config pack wire`: the command over the plugin wiring writer, and
//! the host tool roots an installed pack's agents read.

use anyhow::Result;
use gents_pack::pack::plugin_wire::{wire_in, WireRequest};
use gents_store::config_client::ConfigAccess;
use serde_json::{json, Value};

use crate::cli::PackWireArgs;

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

/// What a User's install did for each agent the installed pack `coordinate`
/// ships with host tools: the root it was granted reader on, or the root
/// nothing published covers.
pub(crate) async fn grant_pack_root_readers(
    access: &ConfigAccess,
    owner: &str,
    coordinate: &str,
) -> Result<Vec<Value>> {
    use gents_protocol::operations::pack::{PackActor, ReaderAction};
    let readers =
        gents_tools::surface::pack_root_readers(access, owner, coordinate, PackActor::User).await?;
    Ok(readers
        .into_iter()
        .map(|reader| match reader.action {
            ReaderAction::ReportPublishStep => {
                let next = publish_root_command(owner, &reader.root, &reader.agent);
                json!({"agent": reader.agent, "unpublished_root": reader.root, "next": next})
            }
            ReaderAction::Grant | ReaderAction::ReportGrantStep | ReaderAction::None => {
                json!({"agent": reader.agent, "reader_on": reader.root})
            }
        })
        .collect())
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
