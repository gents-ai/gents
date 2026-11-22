use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;
use gents_pack::pack::plugin_wire::{ClientWire, WireCommand, WirePlan, WireRequest};
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use serde_json::{json, Value};

use super::pack_install_client::{PackInstallSession, PackReplaySession};

/// Wires new pins in one transaction of the runtime serving `endpoint`'s
/// node: the client plans each from the plan it made from a pack it sent or
/// approved, and otherwise from its own home, checks each write the runtime
/// proposes against its own plans and signs it with the User key it holds,
/// so the runtime never signs as the User.
pub fn wire_as_client(
    endpoint: GraphqlEndpoint,
    owner: String,
    home: PathBuf,
    wires: Vec<ClientWire>,
) -> BoxFuture<'static, Result<Vec<Value>>> {
    Box::pin(async move {
        let access = ConfigAccess::Graphql(endpoint.clone());
        let requests: Vec<WireRequest> = wires.iter().map(|wire| wire.request.clone()).collect();
        let plan = WirePlan::for_client(&access, &owner, &home, wires).await?;
        let command = WireCommand {
            node_did: owner.clone(),
            requests,
        };
        let session = PackInstallSession::open(
            &endpoint,
            json!({"type": "wire", "command": command}),
            &owner,
            &plan.digests(),
        )
        .await?;
        let replay = Arc::new(PackReplaySession::new(session));
        let access = ConfigAccess::Graphql(endpoint.with_pack_replay(replay.clone())?);
        access
            .transact_write("pack.wire.replay", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    plan.apply(txn, gents_pack::pack::wiring::BatchWrite::Apply)
                        .await
                })
            })
            .await?;
        Ok(serde_json::from_value(replay.result().await?)?)
    })
}

/// The plan a client makes to wire `request.plugin` over a runtime that
/// serves another home (`Configuration.wirePlanSubject`): the record the
/// runtime serves, taken only when it is exactly what a pack in the client's
/// `approvals` for `owner` yields and names `request.plugin`, with those
/// approvals, which vouch for each pack the wire reads
/// (`Configuration.wireOwnApproval`, `Configuration.wireTargetApproval`).
/// Anything else plans nothing and nothing is signed.
pub async fn served_plan(
    endpoint: &GraphqlEndpoint,
    owner: &str,
    request: &WireRequest,
    approvals: &gents_pack::pack::approvals::Approvals,
) -> Result<gents_pack::pack::plugin_wire::ClientPlan> {
    use anyhow::Context as _;
    use gents_pack::pack::wire_subject::{wire_plan_record, WireRecord};
    let approved = approvals.for_node(owner).await?;
    let subjects: Vec<_> = approved
        .values()
        .flat_map(|approval| approval.subjects.iter().cloned())
        .collect();
    let mut url = reqwest::Url::parse(endpoint.url())?;
    let prefix = url
        .path()
        .strip_suffix("/api/v0/graphql")
        .context("wiring over a runtime requires a Gents runtime GraphQL endpoint")?
        .to_owned();
    url.set_path(&format!(
        "{prefix}{}",
        gents_pack::pack::plugin_wire::PLUGIN_RECORD_PATH
    ));
    let client = tokio::task::spawn_blocking(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
    })
    .await??;
    let served: gents_pack::plugin::store::InstalledPlugin =
        crate::http_client::endpoint_get_json(endpoint, |endpoint| {
            endpoint.authorize(
                client
                    .get(url.clone())
                    .query(&[("plugin", request.plugin.as_str())]),
            )
        })
        .await
        .with_context(|| format!("reading {} as the runtime records it", request.plugin))?;
    let record = wire_plan_record(&request.plugin, &subjects, WireRecord::Served(served))
        .with_context(|| {
            format!(
                "{} on {owner} is not what a pack this device approved yields, so it is not wired. Next: gents config pack approve <pack source>",
                request.plugin
            )
        })?;
    Ok(gents_pack::pack::plugin_wire::ClientPlan {
        record,
        approvals: Some(approved.into_values().collect()),
    })
}

/// What a client wires with: the User its home holds for a store it opens
/// itself, and its own key through a runtime serving the node.
pub const SIGNERS: gents_pack::pack::plugin_wire::WireSigners =
    gents_pack::pack::plugin_wire::WireSigners {
        held_user: gents_service::home_state::load_held_user_identity,
        client: wire_as_client,
    };
