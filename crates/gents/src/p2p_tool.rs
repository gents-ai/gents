use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::agent::p2p_reconcile::{
    templates::admit_app_collections, ActiveEnrollment, EmbeddedRemoteP2pAdmin,
    GraphqlEnrollmentStore, RemoteP2pAdmin,
};
use crate::config_client::ConfigAccess;
use crate::defra_node::EmbeddedNode;
use crate::graphql::{escape_graphql_string, validate_collection_identifier};
use crate::llm::tool::{Tool, ToolDefinition};
use crate::tool_surface::EndpointScope;
use crate::AgentIdentity;

#[cfg(test)]
mod tests;

pub const P2P_TOOL_NAME: &str = "p2p";
const OVERLAY_SOURCE: &str = "engineer";

#[derive(Clone)]
pub struct P2pTool {
    node: Arc<EmbeddedNode>,
    identity: Option<Arc<dyn AgentIdentity>>,
    mutate: bool,
    collections: EndpointScope<String, ()>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct P2pParams {
    pub argv: Vec<String>,
    #[serde(default)]
    pub options: Map<String, Value>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct P2pError(String);

#[derive(Serialize)]
struct Reply {
    outcome: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_call: Option<Value>,
    observations: Value,
}

impl P2pTool {
    pub fn new(
        node: Arc<EmbeddedNode>,
        identity: Option<Arc<dyn AgentIdentity>>,
        mutate: bool,
        collections: EndpointScope<String, ()>,
    ) -> Self {
        Self {
            node,
            identity,
            mutate,
            collections,
        }
    }

    fn actor(&self) -> Result<Arc<dyn AgentIdentity>> {
        let identity = self
            .identity
            .clone()
            .context("running node identity is unavailable")?;
        ensure!(
            self.node.node_identity_did() == Some(identity.did()),
            "P2P administration requires the running node DID"
        );
        if let Some(context) = crate::tool_call_lifecycle::runtime::current_tool_runtime_context() {
            ensure!(
                context.agent_did.as_deref() == Some(identity.did()),
                "P2P tool principal differs from the running request"
            );
        }
        Ok(identity)
    }

    async fn enrollment(&self, peer: &str, expected_did: Option<&str>) -> Result<ActiveEnrollment> {
        defra_p2p_adapter::TransportPeerId::new(peer.to_owned())
            .map_err(anyhow::Error::msg)
            .context(
                "invalid transport peer ID; next call: p2p {\"argv\":[\"network\",\"list\"]}",
            )?;
        let projection = GraphqlEnrollmentStore::new(self.node.clone(), self.actor()?)
            .load_projection()
            .await?;
        ensure!(
            projection.conflict.is_none(),
            "enrollment authority is conflicted"
        );
        let active = projection.active.into_iter().find(|entry| entry.request.candidate_peer == peer)
            .context("peer has no current authorized enrollment; next call: p2p {\"argv\":[\"enrollment\",\"pending\"]}")?;
        ensure!(expected_did.is_none_or(|did| did == active.request.candidate_did), "peer DID does not match signed enrollment; next call: p2p {{\"argv\":[\"pairings\",\"list\"]}}");
        Ok(active)
    }

    async fn rows(
        &self,
        collection: &str,
        filter: Option<&str>,
        fields: &str,
    ) -> Result<Vec<Value>> {
        validate_collection_identifier(collection)?;
        let arguments = filter.map_or("limit: 50".to_owned(), |filter| {
            format!("filter: {filter}, limit: 50")
        });
        let result = ConfigAccess::Local(self.node.clone())
            .execute(&format!("{{{collection}({arguments}) {{{fields}}}}}"))
            .await?;
        ensure!(
            result
                .get("errors")
                .is_none_or(|e| e.is_null() || e.as_array().is_some_and(Vec::is_empty)),
            "P2P document observation failed: {}",
            result["errors"]
        );
        result["data"][collection]
            .as_array()
            .cloned()
            .context("P2P response omitted documents")
    }

    async fn overlay(&self, peer: &str) -> Result<Option<Value>> {
        let rows = self
            .rows(
                "DataPlanePairingDesired",
                Some(&format!("{{peer_id: {{_eq: {}}}}}", quoted(peer))),
                "_docID peer_id agent_did collections replicator_addresses template source",
            )
            .await?;
        ensure!(rows.len() <= 1, "ambiguous pairing document");
        Ok(rows.into_iter().next())
    }

    fn owned_overlay(&self, row: &Value, actor: &str) -> Result<()> {
        ensure!(
            row["agent_did"].as_str() == Some(actor)
                && row["source"].as_str() == Some(OVERLAY_SOURCE),
            "pairing overlay is managed by another owner; do not change its policy"
        );
        Ok(())
    }

    async fn mutate_overlay(
        &self,
        peer: &str,
        desired: Option<Value>,
        fields: Option<String>,
    ) -> Result<bool> {
        let actor = self.actor()?.did().to_owned();
        let identity = actor.parse()?;
        let peer = peer.to_owned();
        ConfigAccess::transact_local(&self.node, Some(identity), "tool.p2p.overlay", move |txn| {
            let actor = actor.clone();
            let peer = peer.clone();
            let desired = desired.clone();
            let fields = fields.clone();
            Box::pin(async move {
                let response = txn.execute(&format!("{{DataPlanePairingDesired(filter: {{peer_id: {{_eq: {}}}}}, limit: 2) {{_docID peer_id agent_did collections replicator_addresses template source}}}}", quoted(&peer))).await?;
                let rows = response["data"]["DataPlanePairingDesired"].as_array().context("missing pairing documents")?;
                ensure!(rows.len() <= 1, "ambiguous pairing document");
                let before = rows.first();
                if let Some(row) = before {
                    ensure!(row["agent_did"].as_str() == Some(actor.as_str()) && row["source"].as_str() == Some(OVERLAY_SOURCE), "pairing overlay is managed by another owner; do not change its policy");
                }
                let unchanged = match (&desired, before) {
                    (None, None) => true,
                    (Some(desired), Some(before)) => desired.as_object().context("invalid desired pairing")?.iter().all(|(key, value)| before.get(key) == Some(value)),
                    _ => false,
                };
                if unchanged { return Ok(true); }
                let mutation = match (fields, before) {
                    (Some(fields), Some(row)) => format!("mutation {{update_DataPlanePairingDesired(filter: {{_docID: {{_eq: {}}}}}, input: {fields}) {{_docID}}}}", quoted(row["_docID"].as_str().context("overlay identity missing")?)),
                    (Some(fields), None) => format!("mutation {{create_DataPlanePairingDesired(input: {fields}) {{_docID}}}}"),
                    (None, Some(row)) => format!("mutation {{delete_DataPlanePairingDesired(filter: {{_docID: {{_eq: {}}}}}) {{_docID}}}}", quoted(row["_docID"].as_str().context("overlay identity missing")?)),
                    (None, None) => unreachable!(),
                };
                txn.execute(&mutation).await?;
                Ok(false)
            })
        }).await
    }

    async fn checked_collections(&self, options: &Map<String, Value>) -> Result<BTreeSet<String>> {
        let collections: Vec<String> = serde_json::from_value(
            options
                .get("collections")
                .cloned()
                .context("options.collections is required")?,
        )?;
        let collections = admit_app_collections(collections.into_iter().collect())
            .context("application pairing collections must be nonempty and exclude Gents protocol collections")?;
        ensure!(
            collections.len() <= 16,
            "at most 16 application collections per pairing"
        );
        for name in &collections {
            validate_collection_identifier(name)?;
            ensure!(self.collections.permits(name), "collection {name} is outside the P2P grant; next call: p2p {{\"argv\":[\"help\",\"pairings\"]}}");
            crate::agent::p2p_reconcile::resolve_embedded_collection_id(&self.node, name)?
                .with_context(|| format!("collection {name} is unavailable on this node"))?;
        }
        Ok(collections)
    }

    async fn call_one(&self, args: P2pParams) -> Result<Reply> {
        let words: Vec<_> = args.argv.iter().map(String::as_str).collect();
        if words.first() == Some(&"help") || words.last() == Some(&"--help") {
            let resource = if words.as_slice() == ["--help"] {
                None
            } else if words[0] == "help" {
                words.get(1).copied()
            } else {
                words.first().copied()
            };
            return Ok(reply(json!({"help": help(resource)?}), None, Value::Null));
        }
        let actor = self.actor()?;
        let admin = EmbeddedRemoteP2pAdmin::new(self.node.clone());
        let options = &args.options;
        let allowed: &[&str] = match words.as_slice() {
            ["status"] | ["network", "list"] | ["pairings", "list"] | ["enrollment", "pending"] => {
                &[]
            }
            ["network", "get"] => &["peer_id"],
            ["pairings", "preview" | "apply"] => &["peer_id", "peer_did", "collections"],
            ["pairings", "revoke"] => &["peer_id"],
            ["enrollment", "approve" | "revoke"] => &["request_id"],
            ["sync", "documents"] => &["peer_id", "collection", "doc_ids"],
            _ => bail!("unknown P2P command; next call: p2p {{\"argv\":[\"help\"]}}"),
        };
        ensure!(
            options.keys().all(|key| allowed.contains(&key.as_str())),
            "unexpected P2P option; next call: p2p {{\"argv\":[\"help\"]}}"
        );
        let text = |key: &str| {
            options
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .with_context(|| format!("options.{key} must be a nonempty string"))
        };
        match words.as_slice() {
            ["status"] => {
                let peers = bounded_observation(admin.active_peers().await?);
                let addresses = bounded_observation(admin.peer_info().await?);
                let mut sync =
                    crate::p2p_observability::observe_embedded_sync_status(&self.node).await?;
                let observed_backlog_peers = sync.push_backlog.per_peer.len();
                sync.push_backlog.per_peer.truncate(50);
                Ok(reply(
                    json!({"connected_peer_ids":peers,"listen_addresses":addresses}),
                    None,
                    json!({"sync":sync,"observed_backlog_peers":observed_backlog_peers,"peer_limit":50}),
                ))
            }
            ["network", "list"] => {
                let peers = self
                    .rows(
                        "PeerRegistry",
                        None,
                        "peer_id agent_did display_name network_id addresses status updated_at",
                    )
                    .await?;
                Ok(reply(
                    json!({"peers":peers}),
                    None,
                    json!({"connected_peer_ids":bounded_observation(admin.active_peers().await?),"limit":50}),
                ))
            }
            ["network", "get"] => {
                let peer = text("peer_id")?;
                defra_p2p_adapter::TransportPeerId::new(peer.to_owned()).map_err(anyhow::Error::msg)
                    .context("invalid transport peer ID; next call: p2p {\"argv\":[\"network\",\"list\"]}")?;
                let peers = self
                    .rows(
                        "PeerRegistry",
                        Some(&format!("{{peer_id: {{_eq: {}}}}}", quoted(peer))),
                        "peer_id agent_did display_name network_id addresses status updated_at",
                    )
                    .await?;
                let connected = admin.active_peers().await?.iter().any(|id| id == peer);
                Ok(reply(
                    json!({"peer_id":peer,"registered_peers":peers,"connected":connected}),
                    peers.is_empty().then(|| json!({"argv":["network","list"]})),
                    Value::Null,
                ))
            }
            ["pairings", "list"] => {
                let desired = self
                    .rows(
                        "DataPlanePairingDesired",
                        None,
                        "peer_id agent_did collections source template",
                    )
                    .await?;
                let applied = self
                    .rows(
                        "PeerPairingApplied",
                        None,
                        "peer_id collections replicator_addresses",
                    )
                    .await?;
                let projection = GraphqlEnrollmentStore::new(self.node.clone(), actor)
                    .load_projection()
                    .await?;
                let enrolled: Vec<_> = projection.active.iter().map(|e| json!({"peer_id":e.request.candidate_peer,"peer_did":e.request.candidate_did,"request_id":e.request.request_id,"authorization_expires_at":e.revision.authorization_expires_at})).take(50).collect();
                Ok(reply(
                    json!({"desired":desired,"applied":applied}),
                    None,
                    json!({"enrolled_peers":enrolled,"connected_peer_ids":bounded_observation(admin.active_peers().await?),"replicators":bounded_observation(admin.list_replicators().await?),"limit":50}),
                ))
            }
            ["enrollment", "pending"] => {
                let projection = GraphqlEnrollmentStore::new(self.node.clone(), actor)
                    .load_projection()
                    .await?;
                let pending: Vec<_> = projection.pending.iter().take(50).map(|e| json!({"request_id":e.request.request_id,"peer_id":e.request.candidate_peer,"peer_did":e.request.candidate_did,"network_id":e.request.network_id})).collect();
                Ok(reply(
                    json!({"pending":pending}),
                    None,
                    json!({"conflict":projection.conflict,"limit":50}),
                ))
            }
            ["enrollment", operation @ ("approve" | "revoke")] => {
                ensure!(self.mutate, "P2P mutations are disabled by the tool grant");
                let store = GraphqlEnrollmentStore::new(self.node.clone(), actor);
                let id = text("request_id")?;
                let result = if *operation == "approve" {
                    store
                        .decide_request(
                            id,
                            gents_protocol::enrollment::EnrollmentDecisionKind::Approved,
                        )
                        .await?
                } else {
                    store.revoke_request(id).await?
                };
                Ok(reply(
                    json!({"request_id":result.request_id,"state":result.state,"delivery_pending":result.delivery_pending,"decision_doc_id":result.decision_doc_id,"revision_doc_id":result.revision_doc_id}),
                    Some(json!({"argv":["pairings","list"]})),
                    Value::Null,
                ))
            }
            ["pairings", operation @ ("preview" | "apply")] => {
                let peer = text("peer_id")?;
                let active = self.enrollment(peer, Some(text("peer_did")?)).await?;
                let collections = self.checked_collections(options).await?;
                let before = self.overlay(peer).await?;
                if let Some(row) = &before {
                    self.owned_overlay(row, actor.did())?;
                }
                let desired = json!({"peer_id":peer,"agent_did":actor.did(),"collections":collections,"replicator_addresses":[active.request.candidate_ticket],"template":"app-collections","source":OVERLAY_SOURCE});
                if *operation == "preview" {
                    return Ok(reply(
                        json!({"proposed":desired,"current":before}),
                        None,
                        json!({"enrollment_request_id":active.request.request_id}),
                    ));
                }
                ensure!(self.mutate, "P2P mutations are disabled by the tool grant");
                let fields = format!("{{peer_id: {}, agent_did: {}, collections: {}, replicator_addresses: {}, template: \"app-collections\", source: {}}}",
                    quoted(peer), quoted(actor.did()), crate::graphql::graphql_string_list_literal(collections.iter().map(String::as_str)), crate::graphql::graphql_string_list_literal([active.request.candidate_ticket.as_str()]), quoted(OVERLAY_SOURCE));
                let unchanged = self
                    .mutate_overlay(peer, Some(desired.clone()), Some(fields))
                    .await?;
                Ok(reply(
                    json!({"desired":desired,"unchanged":unchanged,"applied":"observe_existing_reconciler"}),
                    Some(json!({"argv":["pairings","list"]})),
                    Value::Null,
                ))
            }
            ["pairings", "revoke"] => {
                ensure!(self.mutate, "P2P mutations are disabled by the tool grant");
                let peer = text("peer_id")?;
                let unchanged = self.mutate_overlay(peer, None, None).await?;
                Ok(reply(
                    json!({"managed_overlay_removed":peer,"unchanged":unchanged}),
                    Some(json!({"argv":["pairings","list"]})),
                    Value::Null,
                ))
            }
            ["sync", "documents"] => {
                ensure!(self.mutate, "P2P mutations are disabled by the tool grant");
                let peer = text("peer_id")?;
                self.enrollment(peer, None).await?;
                let collection = text("collection")?;
                let scoped = Map::from_iter([("collections".into(), json!([collection]))]);
                self.checked_collections(&scoped).await?;
                let ids: Vec<String> = serde_json::from_value(
                    options
                        .get("doc_ids")
                        .cloned()
                        .context("options.doc_ids is required")?,
                )?;
                ensure!(
                    !ids.is_empty()
                        && ids.len() <= 16
                        && ids.iter().all(|id| !id.trim().is_empty()),
                    "sync requires 1 through 16 nonempty document IDs"
                );
                let result = admin
                    .sync_documents(collection, &ids, Some(Duration::from_secs(10)))
                    .await;
                let filter = format!(
                    "{{_docID: {{_in: {}}}}}",
                    crate::graphql::graphql_string_list_literal(ids.iter().map(String::as_str))
                );
                let present = self.rows(collection, Some(&filter), "_docID").await?;
                let arrived: BTreeSet<_> = present
                    .iter()
                    .filter_map(|r| r["_docID"].as_str())
                    .collect();
                let missing: Vec<_> = ids
                    .iter()
                    .filter(|id| !arrived.contains(id.as_str()))
                    .collect();
                Ok(reply(
                    json!({"observed_document_ids":arrived,"missing_document_ids":missing}),
                    (!missing.is_empty()).then(|| json!({"argv":["status"]})),
                    json!({"peer_id":peer,"collection":collection,"sync_request_error":result.err().map(|e|e.to_string()),"source_peer_not_certified":true}),
                ))
            }
            _ => unreachable!(),
        }
    }
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", escape_graphql_string(value))
}
fn reply(outcome: Value, next_call: Option<Value>, observations: Value) -> Reply {
    Reply {
        outcome,
        next_call,
        observations,
    }
}

fn help(resource: Option<&str>) -> Result<&'static str> {
    Ok(match resource {
        None => "Native P2P commands in argv: status; network list/get; pairings list/preview/apply/revoke; enrollment pending/approve/revoke; sync documents. Read [help,RESOURCE] for parameters. Identity comes from the running node. Mutation and collection grants are explicit.",
        Some("status" | "network") => "[status] returns connected peer IDs and native sync facts. [network,list] lists up to 50 registered peers with exact DIDs and addresses. [network,get] takes options.peer_id, validates the transport identity and returns its registry record plus observed connection. A registry entry is discovery, not enrollment authority.",
        Some("pairings") => "[pairings,list] shows desired, applied, enrolled and connected observations separately. [pairings,preview] and [pairings,apply] require options.peer_id, peer_did and collections (1-16 application names). The peer must have current signed enrollment; its DID/address come from that owner. Apply changes only the explicit engineer application overlay; the existing reconciler applies it. [pairings,revoke] requires peer_id and removes only that overlay. Protocol collections and another owner's overlays are refused. Inspect list after apply/revoke; submitted desired state is not proof of a live route.",
        Some("enrollment") => "[enrollment,pending] discovers signed request IDs. [enrollment,approve] or [enrollment,revoke] requires options.request_id and mutation authority. The existing enrollment owner verifies the operator, network and request and signs the durable decision. Approval uses its bounded default authorization lease. Do not invent a DID or substitute an unsigned pairing document.",
        Some("sync") => "[sync,documents] requires options.peer_id, collection and doc_ids (1-16 physical document IDs). The peer needs current enrollment; the application collection must be allowed by the P2P grant. The native adapter makes a bounded 10-second sync request, then this node observes actual document IDs. Missing IDs and request errors are explicit. DefraDB chooses providers; this does not certify which peer supplied a document. Use query for authorized document content.",
        Some(_) => bail!("unknown P2P help resource; next call: p2p {{\"argv\":[\"help\"]}}"),
    })
}

impl Tool for P2pTool {
    const NAME: &'static str = P2P_TOOL_NAME;
    type Error = P2pError;
    type Args = P2pParams;
    type Output = String;

    async fn definition(&self, _: String) -> ToolDefinition {
        ToolDefinition { name: Self::NAME.into(), description: "Inspect this node's P2P peers, manage authorized pairing overlays and request bounded sync. Commands use argv; read [\"help\"] or [\"help\",RESOURCE] for parameters. Enrollment and DefraDB enforce access.".into(), parameters: json!({"type":"object","required":["argv"],"additionalProperties":false,"properties":{"argv":{"type":"array","minItems":1,"items":{"type":"string"}},"options":{"type":"object"}}}) }
    }

    async fn call(&self, args: P2pParams) -> std::result::Result<String, P2pError> {
        if args.argv.len() > 4
            || serde_json::to_vec(&args.options).map_or(true, |bytes| bytes.len() > 16_384)
        {
            return Err(P2pError(
                "P2P call exceeds bounded parameters; next call: p2p {\"argv\":[\"help\"]}".into(),
            ));
        }
        let result = self.call_one(args).await.map_err(|error| {
            let message = format!("{error:#}");
            P2pError(if message.contains("next call:") {
                message
            } else {
                format!("{message}; next call: p2p {{\"argv\":[\"help\"]}}")
            })
        })?;
        serde_json::to_string(&result).map_err(|error| P2pError(error.to_string()))
    }
}

fn bounded_observation<T: Serialize>(mut items: Vec<T>) -> Value {
    let observed_count = items.len();
    items.truncate(50);
    json!({"items":items,"observed_count":observed_count,"limit":50})
}
