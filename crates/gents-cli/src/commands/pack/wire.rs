//! `gents config pack wire`: the command over the plugin wiring writer, and
//! the host tool roots an installed pack's agents read.

use anyhow::Result;
use gents_pack::pack::approvals::{Approvals, PackApproval};
use gents_pack::pack::plugin_wire::{wire_in_planned, WireRequest};
use gents_pack::pack::resolved_pack::ResolvedPack;
use gents_store::config_client::ConfigAccess;

use crate::cli::{PackApproveArgs, PackWireArgs};

pub(crate) async fn wire(args: PackWireArgs) -> Result<()> {
    let (access, owner) = super::resolve_scope_owner(&args.scope).await?;
    let home = gents_service::home_state::resolve_home_dir(args.scope.home.as_deref());
    let request = WireRequest {
        plugin: args.plugin,
        agent: args.agent,
        targets_from: args.targets_from,
        remove: args.remove,
        preview: args.preview,
        tool_calls_consent: false,
    };
    let plan = match &access {
        ConfigAccess::Graphql(endpoint)
            if !request.preview && !request.remove && !serves_own_home(&home, &owner) =>
        {
            Some(
                gents_host::http::pack_wire_client::served_plan(
                    endpoint,
                    &owner,
                    &request,
                    &approvals(&home)?,
                )
                .await?,
            )
        }
        _ => None,
    };
    crate::print_json(
        &wire_in_planned(
            &access,
            &owner,
            &home,
            &request,
            plan,
            gents_host::http::pack_wire_client::SIGNERS,
        )
        .await?,
    )
}

/// `gents config pack approve`: the client resolves the pack itself and
/// records what it yields for the node, installing nothing; it never takes a
/// digest from the runtime.
pub(crate) async fn approve(args: PackApproveArgs) -> Result<()> {
    let (_, owner) = super::resolve_scope_owner(&args.scope).await?;
    let home = gents_service::home_state::resolve_home_dir(args.scope.home.as_deref());
    let pack = gents_pack::pack::resolved_pack::resolve_pack_source(
        &args.package,
        args.registry.as_deref(),
        &home,
    )
    .await?;
    let approval = record_approvals(
        &home,
        &owner,
        [&pack],
        &args.dependency_variables,
        &args.bind_agents,
    )
    .await?;
    crate::print_json(&serde_json::json!({
        "node_did": owner,
        "approved": approval,
    }))
}

pub(crate) async fn record_approvals<'a>(
    home: &std::path::Path,
    owner: &str,
    packs: impl IntoIterator<Item = &'a ResolvedPack>,
    variables: &[String],
    bind_agents: &[String],
) -> Result<Vec<PackApproval>> {
    let approvals = approvals(home)?;
    let supplied = gents_pack::pack::install_flow::supplied_variables(variables, bind_agents)?;
    let mut recorded = Vec::new();
    for pack in packs {
        let approval = PackApproval::of_pack(pack, owner, &supplied)?;
        approvals.record(owner, approval.clone()).await?;
        recorded.push(approval);
    }
    Ok(recorded)
}

/// The approvals the client identity administering `home` recorded.
fn approvals(home: &std::path::Path) -> Result<Approvals> {
    Ok(Approvals::under(
        gents_service::home_state::cli_identity_paths(home)?.root(),
    ))
}

/// Whether the runtime serving `owner` serves `home` itself: the home is
/// that node's, so its plugin store is the client's own record
/// (`Configuration.WireRecord.ownHome`).
fn serves_own_home(home: &std::path::Path, owner: &str) -> bool {
    gents_service::home_state::resolve_node_did(Some(home), None).is_ok_and(|node| node == owner)
}

#[cfg(test)]
mod next_command_tests {
    use clap::Parser as _;

    #[test]
    fn the_commands_wiring_names_parse() {
        for command in [
            gents_pack::pack::plugin_wire::explain_tools_command("did:key:agent"),
            gents_tools::surface::publish_root_command(
                "did:key:node",
                "/srv/root",
                "did:key:agent",
            ),
            gents_tools::surface::grant_reader_command("/srv/root", "did:key:agent"),
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
