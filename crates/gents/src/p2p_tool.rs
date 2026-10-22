use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::agent::p2p_reconcile::{
    templates::admit_app_collections, ActiveEnrollment, EmbeddedRemoteP2pAdmin,
    EnrollmentOfferIssuer, GraphqlEnrollmentStore, RemoteP2pAdmin,
};
use crate::config_client::ConfigAccess;
use crate::defra_node::EmbeddedNode;
use crate::graphql::{escape_graphql_string, validate_collection_identifier};
use crate::llm::tool::{Tool, ToolDefinition};
use crate::tool_surface::EndpointScope;
use crate::NodeIdentity;
use gents_protocol::enrollment::EnrollmentDecisionKind;

#[cfg(test)]
mod tests;

pub const P2P_TOOL_NAME: &str = "p2p";
const OVERLAY_SOURCE: &str = "engineer";

#[derive(Clone)]
pub struct P2pTool {
    node: Arc<EmbeddedNode>,
    identity: Option<Arc<dyn NodeIdentity>>,
    mutate: bool,
    collections: EndpointScope<String, ()>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct P2pParams {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub peer_id: Option<Value>,
    #[serde(default)]
    pub peer_did: Option<Value>,
    #[serde(default)]
    pub collections: Option<Value>,
    #[serde(default)]
    pub collection: Option<Value>,
    #[serde(default)]
    pub doc_ids: Option<Value>,
    #[serde(default)]
    pub request_id: Option<Value>,
    #[serde(default)]
    pub offer_token: Option<Value>,
    #[serde(default)]
    pub details: Option<Value>,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub options: Map<String, Value>,
}

const ACTIONS: &[&str] = &[
    "help",
    "status",
    "network_list",
    "network_get",
    "pairings_list",
    "pairings_get",
    "pairings_preview",
    "pairings_apply",
    "pairings_revoke",
    "enrollment_offer",
    "enrollment_request",
    "enrollment_pending",
    "enrollment_approve",
    "enrollment_deny",
    "enrollment_revoke",
    "enrollment_history",
    "sync_documents",
];
const HELP_TOPICS: &[&str] = &[
    "status",
    "network",
    "pairings",
    "enrollment",
    "sync",
    "scope",
];

impl P2pParams {
    fn command(self) -> Result<(Vec<String>, Map<String, Value>)> {
        let mut options = self.options;
        for (key, value) in [
            ("peer_id", self.peer_id),
            ("peer_did", self.peer_did),
            ("collections", self.collections),
            ("collection", self.collection),
            ("doc_ids", self.doc_ids),
            ("request_id", self.request_id),
            ("offer_token", self.offer_token),
            ("details", self.details),
        ] {
            if let Some(value) = value {
                ensure!(
                    options.insert(key.to_owned(), value).is_none(),
                    "{key} was sent twice, as a field and in options; send it once as a field; next call: p2p {{\"action\":\"help\"}}"
                );
            }
        }
        let Some(action) = self.action else {
            ensure!(
                self.topic.is_none() && !self.argv.is_empty(),
                "action is required, one of {ACTIONS:?}; next call: p2p {{\"action\":\"help\"}}"
            );
            return Ok((self.argv, options));
        };
        ensure!(
            self.argv.is_empty(),
            "send action or the legacy argv, not both; next call: p2p {{\"action\":\"help\"}}"
        );
        ensure!(
            ACTIONS.contains(&action.as_str()),
            "unknown action {action:?}; actions: {ACTIONS:?}; next call: p2p {{\"action\":\"help\"}}"
        );
        if action == "help" {
            return Ok((
                std::iter::once("help".to_owned())
                    .chain(self.topic)
                    .collect(),
                options,
            ));
        }
        ensure!(
            self.topic.is_none(),
            "topic applies only to action help; next call: p2p {{\"action\":\"help\"}}"
        );
        Ok((action.splitn(2, '_').map(str::to_owned).collect(), options))
    }
}

fn action_call(words: &[&str], fields: Value) -> Value {
    let mut call = json!({ "action": words.join("_") });
    if let (Some(call), Some(fields)) = (call.as_object_mut(), fields.as_object()) {
        call.extend(fields.clone());
    }
    call
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
        identity: Option<Arc<dyn NodeIdentity>>,
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

    fn actor(&self) -> Result<Arc<dyn NodeIdentity>> {
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
                context.node_did.as_deref() == Some(identity.did()),
                "P2P tool node DID differs from the running request"
            );
        }
        Ok(identity)
    }

    async fn enrollment(&self, peer: &str, expected_did: Option<&str>) -> Result<ActiveEnrollment> {
        defra_p2p_adapter::TransportPeerId::new(peer.to_owned())
            .map_err(anyhow::Error::msg)
            .context("invalid transport peer ID; next call: p2p {\"action\":\"network_list\"}")?;
        let projection = GraphqlEnrollmentStore::new(self.node.clone(), self.actor()?)
            .load_projection()
            .await?;
        ensure!(
            projection.conflict.is_none(),
            "enrollment authority is conflicted"
        );
        let active = projection.active.into_iter().find(|entry| entry.request.candidate_peer == peer)
            .context("peer has no current authorized enrollment in this node's network. A pairing applied here needs the peer to have joined this node: it ran enrollment_request with this node's offer and this node approved it. Joining the peer's network does not count; pairing both ways needs each node enrolled in the other's network, so mint an offer here for the peer or have the peer apply its pairing toward this node; next call: p2p {\"action\":\"enrollment_pending\"}")?;
        ensure!(expected_did.is_none_or(|did| did == active.request.candidate_did), "peer DID does not match signed enrollment; next call: p2p {{\"action\":\"pairings_list\"}}");
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
        let result = ConfigAccess::local(self.node.clone())
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
                "_docID peer_id node_did collections replicator_addresses template source",
            )
            .await?;
        ensure!(rows.len() <= 1, "ambiguous pairing document");
        Ok(rows.into_iter().next())
    }

    fn owned_overlay(&self, row: &Value, actor: &str) -> Result<()> {
        ensure!(
            row["node_did"].as_str() == Some(actor)
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
        let collections_scope = self.collections.clone();
        ConfigAccess::transact_local_write(&self.node, Some(identity), "tool.p2p.overlay", move |txn| {
            let actor = actor.clone();
            let peer = peer.clone();
            let desired = desired.clone();
            let fields = fields.clone();
            let collections_scope = collections_scope.clone();
            Box::pin(async move {
                let response = txn.execute(&format!("{{DataPlanePairingDesired(filter: {{peer_id: {{_eq: {}}}}}, limit: 2) {{_docID peer_id node_did collections replicator_addresses template source}}}}", quoted(&peer))).await?;
                let rows = response["data"]["DataPlanePairingDesired"].as_array().context("missing pairing documents")?;
                ensure!(rows.len() <= 1, "ambiguous pairing document");
                let before = rows.first();
                if let Some(row) = before {
                    ensure!(row["node_did"].as_str() == Some(actor.as_str()) && row["source"].as_str() == Some(OVERLAY_SOURCE), "pairing overlay is managed by another owner; do not change its policy");
                    let before_collections: Vec<String> = if row["collections"].is_null() {Vec::new()} else {serde_json::from_value(row["collections"].clone()).context("malformed existing collection scope")?};
                    ensure!(collections_scope.permits_all(before_collections.iter()), "existing pairing contains collections outside the P2P grant; next call: p2p {{\"action\":\"pairings_list\"}}");
                    if !before_collections.is_empty() {
                        admit_app_collections(before_collections.into_iter().collect()).context("existing pairing contains protocol collections")?;
                    }
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
                .context("collections is required")?,
        )?;
        ensure!(!collections.is_empty(), "collections must contain application collection names; next call: p2p {{\"action\":\"help\",\"topic\":\"pairings\"}}");
        let collections = admit_app_collections(collections.into_iter().collect())
            .context("protocol collections, including credentials, are never supported by application pairing or sync; changing grants or ACP cannot enable this operation. Keep credentials with their existing operator owner; next call: p2p {\"action\":\"help\",\"topic\":\"scope\"}")?;
        ensure!(
            collections.len() <= 16,
            "at most 16 application collections per pairing"
        );
        for name in &collections {
            validate_collection_identifier(name)?;
            ensure!(self.collections.permits(name), "application collection {name} is outside this tool's P2P collection grant; an authorized configuration owner must grant that application collection before retrying. ACP document access remains separate; next call: p2p {{\"action\":\"help\",\"topic\":\"scope\"}}");
            crate::agent::p2p_reconcile::resolve_embedded_collection_id(&self.node, name)?
                .with_context(|| format!("collection {name} is unavailable on this node"))?;
        }
        Ok(collections)
    }

    async fn call_one(&self, argv: Vec<String>, options: Map<String, Value>) -> Result<Reply> {
        let words: Vec<_> = argv.iter().map(String::as_str).collect();
        if words.first() == Some(&"help") || words.last() == Some(&"--help") {
            let resource = if words.as_slice() == ["--help"] {
                None
            } else if words[0] == "help" {
                words.get(1).copied()
            } else {
                words.first().copied()
            };
            let mut outcome = json!({"help": help(resource)?});
            if resource == Some("scope") {
                outcome["grant"] = json!({"mutations":self.mutate,"application_collections":{"scope":self.collections.kind(),"names":self.collections.keys()}});
            }
            return Ok(reply(outcome, None, Value::Null));
        }
        let actor = self.actor()?;
        let admin = EmbeddedRemoteP2pAdmin::new(self.node.clone());
        let options = &options;
        let allowed: &[&str] = match words.as_slice() {
            ["status"] | ["pairings", "list"] => &["details"],
            ["network", "list"] | ["enrollment", "pending" | "offer" | "history"] => &[],
            ["pairings", "get"] => &["peer_id", "details"],
            ["network", "get"] => &["peer_id"],
            ["pairings", "preview" | "apply"] => &["peer_id", "peer_did", "collections"],
            ["pairings", "revoke"] => &["peer_id"],
            ["enrollment", "approve" | "deny" | "revoke"] => &["request_id"],
            ["enrollment", "request"] => &["offer_token"],
            ["sync", "documents"] => &["peer_id", "collection", "doc_ids"],
            _ => {
                let resource = words.first().copied().filter(|word| {
                    matches!(
                        *word,
                        "status" | "network" | "pairings" | "enrollment" | "sync"
                    )
                });
                let next = resource.map_or(
                    json!({"action":"help"}),
                    |resource| json!({"action":"help","topic":resource}),
                );
                bail!("unknown P2P command; next call: p2p {next}")
            }
        };
        ensure!(
            options.keys().all(|key| allowed.contains(&key.as_str())),
            "unexpected field for action {}; accepted fields: {allowed:?}; next call: p2p {{\"action\":\"help\",\"topic\":\"{}\"}}", words.join("_"), words[0]
        );
        let text = |key: &str| {
            options
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .with_context(|| format!("{key} must be a nonempty string; next call: p2p {{\"action\":\"help\",\"topic\":\"{}\"}}", words[0]))
        };
        let details = options.get("details").map_or(Ok(false), |value| {
            value.as_bool().context("details must be true or false")
        })?;
        match words.as_slice() {
            ["status"] => {
                let peers = bounded_observation(admin.active_peers().await?);
                let addresses = bounded_observation(admin.peer_info().await?);
                let mut sync =
                    crate::p2p_observability::observe_embedded_sync_status(&self.node).await?;
                let observed_backlog_peers = sync.push_backlog.per_peer.len();
                sync.push_backlog.per_peer.truncate(50);
                Ok(reply(
                    json!({"connected_peers":peers,"listen_addresses":addresses}),
                    None,
                    if details {
                        json!({"sync":sync,"observed_backlog_peers":observed_backlog_peers,"peer_limit":50})
                    } else {
                        let value = serde_json::to_value(sync)?;
                        json!({"sync":{"pending_dags":value["pending_dags"],"next_pending_retry_in_ms":value["next_pending_retry_in_ms"],"push_backlog":{"active_jobs":value["push_backlog"]["active_jobs"],"queued_items":value["push_backlog"]["queued_items"],"failed_total":value["push_backlog"]["failed_total"]}},"details_call":{"action":"status","details":true}})
                    },
                ))
            }
            ["network", "list"] => {
                let peers = self
                    .rows(
                        "PeerRegistry",
                        None,
                        "peer_id node_did display_name network_id addresses status updated_at",
                    )
                    .await?;
                Ok(reply(
                    json!({"peers":peers}),
                    None,
                    json!({"connected_peers":bounded_observation(admin.active_peers().await?),"limit":50}),
                ))
            }
            ["network", "get"] => {
                let peer = text("peer_id")?;
                defra_p2p_adapter::TransportPeerId::new(peer.to_owned())
                    .map_err(anyhow::Error::msg)
                    .context(
                        "invalid transport peer ID; next call: p2p {\"action\":\"network_list\"}",
                    )?;
                let peers = self
                    .rows(
                        "PeerRegistry",
                        Some(&format!("{{peer_id: {{_eq: {}}}}}", quoted(peer))),
                        "peer_id node_did display_name network_id addresses status updated_at",
                    )
                    .await?;
                let connected = admin.active_peers().await?.iter().any(|entry| {
                    entry == peer
                        || p2p::iroh::parse_public_peer_addr(entry)
                            .is_ok_and(|(id, _)| id.as_str() == peer)
                });
                Ok(reply(
                    json!({"peer_id":peer,"registered_peers":peers,"connected":connected}),
                    peers.is_empty().then(|| json!({"action":"network_list"})),
                    Value::Null,
                ))
            }
            ["pairings", "list" | "get"] => {
                let peer = (words[1] == "get").then(|| text("peer_id")).transpose()?;
                if let Some(peer) = peer {
                    defra_p2p_adapter::TransportPeerId::new(peer.to_owned()).map_err(anyhow::Error::msg)
                        .context("invalid transport peer ID; next call: p2p {\"action\":\"network_list\"}")?;
                }
                let filter = peer.map(|peer| format!("{{peer_id: {{_eq: {}}}}}", quoted(peer)));
                let desired = self
                    .rows(
                        "DataPlanePairingDesired",
                        filter.as_deref(),
                        "peer_id node_did collections source template",
                    )
                    .await?;
                let applied = self
                    .rows(
                        "PeerPairingApplied",
                        filter.as_deref(),
                        "peer_id collections replicator_addresses",
                    )
                    .await?;
                let projection = GraphqlEnrollmentStore::new(self.node.clone(), actor)
                    .load_projection()
                    .await?;
                let enrolled: Vec<_> = projection.active.iter()
                    .filter(|entry| peer.is_none_or(|peer| entry.request.candidate_peer == peer))
                    .take(50).map(|e| json!({"peer_id":e.request.candidate_peer,"peer_did":e.request.candidate_did,"request_id":e.request.request_id,"authorization_expires_at":e.revision.authorization_expires_at})).collect();
                let connected: Vec<_> = admin
                    .active_peers()
                    .await?
                    .into_iter()
                    .filter(|entry| peer.is_none_or(|peer| peer_reference_matches(entry, peer)))
                    .collect();
                let replicators: Vec<_> = admin
                    .list_replicators()
                    .await?
                    .into_iter()
                    .filter(|entry| {
                        peer.is_none_or(|peer| {
                            entry
                                .address
                                .as_deref()
                                .is_some_and(|address| peer_reference_matches(address, peer))
                                || entry
                                    .id
                                    .as_deref()
                                    .is_some_and(|id| peer_reference_matches(id, peer))
                        })
                    })
                    .collect();
                let replicators = if details {
                    bounded_observation(replicators)
                } else {
                    bounded_observation(replicators.into_iter().map(|entry| {
                        json!({"address":entry.address,"collection_count":entry.collections.len(),"filter_count":entry.filters.as_ref().map(|filters|filters.len())})
                    }).collect())
                };
                let mut detail_options = json!({"details":true});
                if let Some(peer) = peer {
                    detail_options["peer_id"] = json!(peer);
                }
                Ok(reply(
                    json!({"desired":desired,"applied":applied}),
                    None,
                    json!({"enrolled_peers":enrolled,"connected_peers":bounded_observation(connected),"replicators":replicators,"limit":50,"details_call":action_call(&words, detail_options)}),
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
            ["enrollment", "offer"] => {
                ensure!(self.mutate, "P2P mutations are disabled by the tool grant");
                let projection = GraphqlEnrollmentStore::new(self.node.clone(), actor.clone())
                    .load_projection()
                    .await?;
                ensure!(
                    projection.conflict.is_none(),
                    "enrollment authority is conflicted"
                );
                let network_id = projection.network_id.context(
                    "this node has no enrollment Network; the running server creates it at startup",
                )?;
                let p2p = crate::identity::node_p2p(&self.node)
                    .context("P2P is not running on this node")?;
                let did = actor.did().to_owned();
                let offer =
                    EnrollmentOfferIssuer::new(actor, p2p, network_id, did, "client".into())
                        .mint()
                        .await?;
                Ok(reply(
                    json!({"offer_token":offer.token,"expires_at":offer.offer.expires_at,"peer_id":offer.offer.server_peer,"peer_did":offer.offer.admin_did}),
                    Some(json!({"action":"enrollment_pending"})),
                    json!({"joining_node_call":{"action":"enrollment_request","offer_token":"OFFER_TOKEN"}}),
                ))
            }
            ["enrollment", "history"] => {
                let projection = GraphqlEnrollmentStore::new(self.node.clone(), actor)
                    .load_projection()
                    .await?;
                let mut history: Vec<_> = projection.pending.iter().map(|e| json!({"request_id":e.request.request_id,"state":"pending","peer_id":e.request.candidate_peer,"peer_did":e.request.candidate_did,"requested_at":e.request.issued_at,"request_expires_at":e.request.expires_at}))
                    .chain(projection.active.iter().map(|e| json!({"request_id":e.request.request_id,"state":"approved","peer_id":e.request.candidate_peer,"peer_did":e.request.candidate_did,"requested_at":e.request.issued_at,"decided_at":e.decision.decided_at,"decided_by":e.decision.signer_did,"authorization_expires_at":e.revision.authorization_expires_at})))
                    .chain(projection.denied.iter().map(|e| json!({"request_id":e.request.request_id,"state":"denied","peer_id":e.request.candidate_peer,"peer_did":e.request.candidate_did,"requested_at":e.request.issued_at,"decided_at":e.decision.decided_at,"decided_by":e.decision.signer_did})))
                    .chain(projection.revoked.iter().map(|e| json!({"request_id":e.request.request_id,"state":"revoked","peer_id":e.request.candidate_peer,"peer_did":e.request.candidate_did,"requested_at":e.request.issued_at,"decided_at":e.decision.decided_at,"decided_by":e.decision.signer_did,"revoked_at":e.revision.issued_at,"revoked_by":e.revision.signer_did})))
                    .collect();
                let latest = |entry: &Value| {
                    ["revoked_at", "decided_at", "requested_at"]
                        .iter()
                        .find_map(|key| entry[*key].as_str().map(str::to_owned))
                        .unwrap_or_default()
                };
                history.sort_by_key(|entry| std::cmp::Reverse(latest(entry)));
                let observed_count = history.len();
                history.truncate(50);
                Ok(reply(
                    json!({"history":history}),
                    None,
                    json!({"observed_count":observed_count,"limit":50,"request_conflicts":projection.request_conflicts,"conflict":projection.conflict,"omitted":"requests that expired undecided or that a later request from the same peer superseded"}),
                ))
            }
            ["enrollment", "request"] => {
                ensure!(self.mutate, "P2P mutations are disabled by the tool grant");
                let request =
                    crate::agent::p2p_reconcile::enrollment_candidate::request_enrollment(
                        &self.node,
                        actor.as_ref(),
                        text("offer_token")?,
                    )
                    .await?;
                Ok(reply(
                    json!({"request_id":request.request_id,"state":"pending_approval","peer_id":request.server_peer,"peer_did":request.admin_did,"expires_at":request.expires_at}),
                    None,
                    json!({"approve_on_peer":{"action":"enrollment_approve","request_id":request.request_id}}),
                ))
            }
            ["enrollment", operation @ ("approve" | "deny" | "revoke")] => {
                ensure!(self.mutate, "P2P mutations are disabled by the tool grant");
                let store = GraphqlEnrollmentStore::new(self.node.clone(), actor);
                let id = text("request_id")?;
                let result = match *operation {
                    "approve" => store.decide_request(id, EnrollmentDecisionKind::Approved).await,
                    "deny" => store.decide_request(id, EnrollmentDecisionKind::Denied).await,
                    _ => store.revoke_request(id).await,
                }
                .map_err(|error| {
                    if format!("{error:#}").contains("challenge is reused") {
                        error.context("an offer token is single use: two nodes requested with one offer, so both requests are refused. Each joining node needs its own fresh offer; mint one per node and have each re-request; next call: p2p {\"action\":\"enrollment_offer\"}")
                    } else {
                        error
                    }
                })?;
                Ok(reply(
                    json!({"request_id":result.request_id,"state":result.state,"delivery_pending":result.delivery_pending,"decision_doc_id":result.decision_doc_id,"revision_doc_id":result.revision_doc_id}),
                    Some(json!({"action":"pairings_list"})),
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
                let desired = json!({"peer_id":peer,"node_did":actor.did(),"collections":collections,"replicator_addresses":[active.request.candidate_ticket],"template":"app-collections","source":OVERLAY_SOURCE});
                if *operation == "preview" {
                    return Ok(reply(
                        json!({"proposed":desired,"current":before}),
                        None,
                        json!({"enrollment_request_id":active.request.request_id}),
                    ));
                }
                ensure!(self.mutate, "P2P mutations are disabled by the tool grant");
                let fields = format!("{{peer_id: {}, node_did: {}, collections: {}, replicator_addresses: {}, template: \"app-collections\", source: {}}}",
                    quoted(peer), quoted(actor.did()), crate::graphql::graphql_string_list_literal(collections.iter().map(String::as_str)), crate::graphql::graphql_string_list_literal([active.request.candidate_ticket.as_str()]), quoted(OVERLAY_SOURCE));
                let unchanged = self
                    .mutate_overlay(peer, Some(desired.clone()), Some(fields))
                    .await?;
                Ok(reply(
                    json!({"desired":desired,"unchanged":unchanged,"applied":"observe_existing_reconciler"}),
                    Some(json!({"action":"pairings_get","peer_id":peer})),
                    Value::Null,
                ))
            }
            ["pairings", "revoke"] => {
                ensure!(self.mutate, "P2P mutations are disabled by the tool grant");
                let peer = text("peer_id")?;
                let unchanged = self.mutate_overlay(peer, None, None).await?;
                Ok(reply(
                    json!({"managed_overlay_removed":peer,"unchanged":unchanged}),
                    Some(json!({"action":"pairings_get","peer_id":peer})),
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
                        .context("doc_ids is required")?,
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
                validate_collection_identifier(collection)?;
                let doc_ids =
                    crate::graphql::graphql_string_list_literal(ids.iter().map(String::as_str));
                let observed = ConfigAccess::local(self.node.clone())
                    .execute(&format!("{{{collection}(docIDs: {doc_ids}) {{_docID}}}}"))
                    .await?;
                ensure!(
                    observed
                        .get("errors")
                        .is_none_or(|errors| errors.is_null()
                            || errors.as_array().is_some_and(Vec::is_empty)),
                    "P2P document observation failed: {}",
                    observed["errors"]
                );
                let present = observed["data"][collection]
                    .as_array()
                    .context("P2P response omitted documents")?;
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
                    (!missing.is_empty()).then(|| json!({"action":"status"})),
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
        None => "Actions: status; network_list, network_get; pairings_list, pairings_get, pairings_preview, pairings_apply, pairings_revoke; enrollment_offer, enrollment_request, enrollment_pending, enrollment_approve, enrollment_deny, enrollment_revoke, enrollment_history; sync_documents. Action help with topic status, network, pairings, enrollment, sync or scope explains each; scope covers grants and capability boundaries. Identity comes from the running node. Mutation and collection grants are explicit.",
        Some("status" | "network") => "status returns native connected peer references (raw IDs or transport addresses) and native sync facts. network_list lists up to 50 registered peers with exact DIDs and addresses. network_get takes peer_id, validates the transport identity and returns its registry record plus observed connection. A registry entry is discovery, not enrollment authority or current connectivity; network_get's connected field is the observed transport connection. status defaults to backlog facts; details:true returns native diagnostic counters.",
        Some("pairings") => "pairings_get with peer_id inspects one peer; pairings_list shows desired, applied, enrolled and connected observations separately. Defaults show compact replicator counts; details:true includes native collection IDs and filters. pairings_preview and pairings_apply require peer_id, peer_did and collections (1-16 application names). The peer must have current signed enrollment; its DID/address come from that owner. Apply changes only the explicit engineer application overlay; the existing reconciler applies it. pairings_revoke requires peer_id and removes only that overlay. Protocol collections and another owner's overlays are refused. Replacing or revoking requires collection authority over the entire existing overlay. Inspect pairings_get after apply/revoke; submitted desired state is not proof of a live route.",
        Some("enrollment") => "Joining: the approving node runs enrollment_offer and hands offer_token to the joining node, which runs enrollment_request; the approver then finds it with enrollment_pending, approves it and applies pairings. enrollment_offer requires mutation authority and returns this node's signed offer_token (valid 5 minutes), peer_id and DID; the token only lets its holder ask to join, approval stays here, and it is single use: give each joining node its own offer, because requests from two nodes made with one offer are both refused. enrollment_request requires offer_token from the approving node and mutation authority. It verifies the peer's signed offer, signs a request with this node's identity and delivers it; the peer's operator approves it there with approve_on_peer. enrollment_pending discovers signed request IDs. enrollment_approve, enrollment_deny or enrollment_revoke requires request_id and mutation authority; deny signs a refusal of a pending request, revoke ends an approved one. The existing enrollment owner verifies the operator, network and request and signs the durable decision. Approval admits the peer to this node's network for the default authorization lease and materializes its enrollment route, so approve only a request you were asked to approve whose peer_did you checked. Enrollment is one-way: approval enrolls the requester in this node's network, so this node can pair toward it, not the reverse. Pairing in both directions needs each node enrolled in the other's network, through a second offer, request and approval the other way. Do not invent a DID or substitute an unsigned pairing document. enrollment_history lists up to 50 current pending, approved, denied and revoked requests, newest first, with decision times and signers; requests that expired undecided or were superseded by the same peer's later request are omitted.",
        Some("sync") => "sync_documents requires peer_id, collection and doc_ids (1-16 physical document IDs). The peer needs current enrollment; only application collections in the tool grant are supported. Protocol collections, including credentials, are always excluded; neither broader grants nor ACP changes enable them. See help topic scope. The native adapter makes a bounded 10-second sync request, then this node observes actual document IDs. Missing IDs and request errors are explicit. DefraDB chooses providers; this does not certify which peer supplied a document. Use query for authorized document content.",
        Some("scope") => "Pairing overlays and document sync support application collections only. Gents protocol/configuration and credential collections are permanently excluded from these actions, even with broader tool grants or document ACP. Keep credentials with their existing operator owner. For an application collection, an authorized configuration owner may grant the exact P2P collection; native ACP still controls document access. Enrollment is a separate signed authority. Sync accepts 1-16 known document IDs, not collection-wide discovery, and cannot certify the supplying peer.",
        Some(resource) => { let next = if resource == "pairing" { "pairings" } else if resource == "documents" { "sync" } else { "scope" }; bail!("unknown P2P help topic {resource:?}; topics: {HELP_TOPICS:?}; next call: p2p {{\"action\":\"help\",\"topic\":\"{next}\"}}") },
    })
}

impl Tool for P2pTool {
    const NAME: &'static str = P2P_TOOL_NAME;
    type Error = P2pError;
    type Args = P2pParams;
    type Output = String;

    async fn definition(&self, _: String) -> ToolDefinition {
        let text =
            |description: &str| json!({"type":"string","minLength":1,"description":description});
        ToolDefinition {
            name: Self::NAME.into(),
            description: "Inspect this node's P2P peers, enroll nodes, manage authorized pairing overlays and request bounded sync. Pick an action and send only its fields: status [details]; network_list; network_get peer_id; pairings_list [details]; pairings_get peer_id [details]; pairings_preview and pairings_apply peer_id peer_did collections; pairings_revoke peer_id; enrollment_offer; enrollment_request offer_token; enrollment_pending; enrollment_approve, enrollment_deny and enrollment_revoke request_id; enrollment_history; sync_documents peer_id collection doc_ids. A node joins in four calls: the approver's enrollment_offer, the joining node's enrollment_request, then the approver's enrollment_pending and enrollment_approve; the approver's pairings_apply then replicates collections toward the joiner. The reverse direction needs the same four calls the other way. Action help with a topic explains rules. Enrollment and DefraDB enforce access.".into(),
            parameters: json!({
                "type": "object",
                "required": ["action"],
                "additionalProperties": false,
                "properties": {
                    "action": {"type": "string", "enum": ACTIONS, "description": "The P2P action; each lists its fields in the tool description."},
                    "topic": {"type": "string", "enum": HELP_TOPICS, "description": "help only: the topic to explain."},
                    "peer_id": text("Remote node's transport peer ID (hex), from enrollment_pending, pairings_list enrolled_peers or network_list. network_get, pairings_get/preview/apply/revoke, sync_documents."),
                    "peer_did": text("Remote node's DID from its signed enrollment (enrollment_pending peer_did). pairings_preview/apply."),
                    "collections": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 16, "description": "Application collection names to replicate to the peer, e.g. [\"DeploymentNote\"]; they must be in this tool's P2P grant. pairings_preview/apply."},
                    "collection": text("One application collection name. sync_documents."),
                    "doc_ids": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 16, "description": "Document IDs to fetch from the peer. sync_documents."},
                    "request_id": text("Enrollment request ID (enroll-...) from enrollment_pending or the joining node's enrollment_request. enrollment_approve/deny/revoke."),
                    "offer_token": text("The approving node's token from its enrollment_offer, valid 5 minutes. enrollment_request, run on the joining node."),
                    "details": {"type": "boolean", "description": "status, pairings_list, pairings_get: include native diagnostic counters, collection IDs and filters."}
                }
            }),
        }
    }

    async fn call(&self, args: P2pParams) -> std::result::Result<String, P2pError> {
        let (argv, options) = args
            .command()
            .map_err(|error| P2pError(format!("{error:#}")))?;
        if argv.len() > 4 || serde_json::to_vec(&options).map_or(true, |bytes| bytes.len() > 16_384)
        {
            return Err(P2pError(
                "P2P call exceeds bounded parameters; next call: p2p {\"action\":\"help\"}".into(),
            ));
        }
        let result = self.call_one(argv, options).await.map_err(|error| {
            let message = format!("{error:#}");
            P2pError(if message.contains("next call:") {
                message
            } else {
                format!("{message}; next call: p2p {{\"action\":\"help\"}}")
            })
        })?;
        serde_json::to_string(&result).map_err(|error| P2pError(error.to_string()))
    }
}

fn peer_reference_matches(reference: &str, peer: &str) -> bool {
    reference == peer
        || p2p::iroh::parse_public_peer_addr(reference).is_ok_and(|(id, _)| id.as_str() == peer)
}

fn bounded_observation<T: Serialize>(mut items: Vec<T>) -> Value {
    let observed_count = items.len();
    items.truncate(50);
    json!({"items":items,"observed_count":observed_count,"limit":50})
}
