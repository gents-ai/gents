use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;
use gents_pack::pack::plugin_wire::{WireCommand, WirePlan, WireRequest};
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use serde_json::{json, Value};

use super::pack_install_client::{PackInstallSession, PackReplaySession};

/// Wires a new pin in the transaction of the runtime serving `endpoint`'s
/// node: the client reads the same plan, checks each write the runtime
/// proposes against its own and signs it with the User key it holds, so
/// the runtime never signs as the User.
pub fn wire_as_client(
    endpoint: GraphqlEndpoint,
    owner: String,
    home: PathBuf,
    request: WireRequest,
) -> BoxFuture<'static, Result<Value>> {
    Box::pin(async move {
        let wire = WirePlan::read_unheld(
            &ConfigAccess::Graphql(endpoint.clone()),
            &owner,
            &home,
            &request,
        )
        .await?;
        let command = WireCommand {
            node_did: owner.clone(),
            request: request.clone(),
        };
        let session = PackInstallSession::open(
            &endpoint,
            json!({"type": "wire", "command": command}),
            &owner,
            std::slice::from_ref(&wire.plugin.digest),
        )
        .await?;
        let replay = Arc::new(PackReplaySession::new(session));
        let access = ConfigAccess::Graphql(endpoint.with_pack_replay(replay.clone())?);
        access
            .transact_write("pack.wire.replay", |txn| {
                let plan = &wire.plan;
                Box::pin(async move {
                    plan.apply(txn, &gents_inference::resolved::ProviderProfilePolicy)
                        .await
                })
            })
            .await?;
        replay.result().await
    })
}

/// What a client wires with: the User its home holds for a store it opens
/// itself, and its own key through a runtime serving the node.
pub const SIGNERS: gents_pack::pack::plugin_wire::WireSigners =
    gents_pack::pack::plugin_wire::WireSigners {
        held_user: gents_service::home_state::load_held_user_identity,
        client: wire_as_client,
    };
