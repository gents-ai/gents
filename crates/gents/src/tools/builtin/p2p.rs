use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::defra_node::Node as EmbeddedNode;
use crate::inference::llm::tool::{Tool, ToolDefinition};
use crate::runtime::agent::p2p_reconcile::{
    enrollment_store::EnrollmentProjection,
    overlay_binding::{evaluate_overlay, CurrentAuthorization, OverlayInactive, OverlayRecord},
    templates::{admit_app_collections, AppCollectionAdmissionError, APP_COLLECTIONS_TEMPLATE},
    ActiveEnrollment, EmbeddedRemoteP2pAdmin, EnrollmentOfferIssuer, GraphqlEnrollmentStore,
    RemoteP2pAdmin,
};
use crate::store::config_client::ConfigAccess;
use crate::store::graphql::{
    escape_graphql_string, graphql_string_list_literal, validate_collection_identifier,
};
use crate::NodeIdentity;
use gents_protocol::enrollment::{
    AuthorizationLease, EnrollmentDecisionKind, EnrollmentRenewalPolicy,
};
use gents_protocol::peer_schema::{AppCollectionRefusal, ReplicatedSchema};

#[cfg(test)]
mod tests;

pub const P2P_TOOL_NAME: &str = "p2p";
const OVERLAY_SOURCE: &str = "engineer";

#[derive(Clone)]
pub struct P2pTool {
    node: Arc<EmbeddedNode>,
    identity: Option<Arc<dyn NodeIdentity>>,
    agent: Option<Arc<dyn NodeIdentity>>,
    mutate: bool,
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
    pub request_id: Option<Value>,
    #[serde(default)]
    pub offer_token: Option<Value>,
    #[serde(default)]
    pub details: Option<Value>,
    #[serde(default)]
    pub valid_for_secs: Option<Value>,
    pub lease_seconds: Option<Value>,
    pub auto_renew: Option<Value>,
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
    "enrollment_renewal_policy",
];
const HELP_TOPICS: &[&str] = &["status", "network", "pairings", "enrollment", "scope"];

impl P2pParams {
    fn command(self) -> Result<(Vec<String>, Map<String, Value>)> {
        let mut options = self.options;
        for (key, value) in [
            ("peer_id", self.peer_id),
            ("peer_did", self.peer_did),
            ("collections", self.collections),
            ("request_id", self.request_id),
            ("offer_token", self.offer_token),
            ("details", self.details),
            ("valid_for_secs", self.valid_for_secs),
            ("lease_seconds", self.lease_seconds),
            ("auto_renew", self.auto_renew),
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

/// The pairing overlay `pairings_apply` writes; absent for `pairings_revoke`.
#[derive(Clone)]
struct OverlayWrite {
    desired: Value,
    create_fields: String,
    update_fields: String,
    parent_node_doc_id: String,
}

impl P2pTool {
    pub fn new(
        node: Arc<EmbeddedNode>,
        identity: Option<Arc<dyn NodeIdentity>>,
        agent: Option<Arc<dyn NodeIdentity>>,
        mutate: bool,
    ) -> Self {
        Self {
            node,
            identity,
            agent,
            mutate,
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
        if let Some(context) =
            crate::tools::call::lifecycle::runtime::current_tool_runtime_context()
        {
            ensure!(
                context.node_did.as_deref() == Some(identity.did()),
                "P2P tool node DID differs from the running request"
            );
        }
        Ok(identity)
    }

    fn require_mutation(&self) -> Result<()> {
        ensure!(
            self.mutate,
            "P2P mutations are disabled by the tool grant; read-only actions still work; next call: p2p {{\"action\":\"help\",\"topic\":\"scope\"}}"
        );
        Ok(())
    }

    /// The Agent whose managed identity owns this request's pairing overlay.
    /// The overlay is Agent-owned, so there is no Node identity fallback.
    fn pairing_agent(&self, runtime: &dyn NodeIdentity) -> Result<Arc<dyn NodeIdentity>> {
        let agent = self.agent.clone().context(
            "pairing requires the running agent's own identity, and the Node identity must not stand in for it; run pairings from an agent session; next call: p2p {\"action\":\"help\",\"topic\":\"scope\"}",
        )?;
        ensure!(
            agent.did() != runtime.did(),
            "pairing requires the running agent's own identity, not the Node identity; next call: p2p {{\"action\":\"help\",\"topic\":\"scope\"}}"
        );
        if let Some(context) =
            crate::tools::call::lifecycle::runtime::current_tool_runtime_context()
        {
            ensure!(
                context.agent_did.as_deref() == Some(agent.did()),
                "P2P tool agent differs from the running request"
            );
        }
        Ok(agent)
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
        let access =
            ConfigAccess::local_as(self.node.clone(), identity::Did::new(self.actor()?.did())?);
        let result = access
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
                "_docID peer_id node_did actor_did collections replicator_addresses template source enrollment_member_did enrollment_sequence enrollment_network_id",
            )
            .await?;
        ensure!(rows.len() <= 1, "ambiguous pairing document");
        Ok(rows.into_iter().next())
    }

    fn owned_overlay(row: &Value, agent: &dyn NodeIdentity) -> Result<()> {
        ensure!(
            row["actor_did"].as_str() == Some(agent.did())
                && row["source"].as_str() == Some(OVERLAY_SOURCE),
            "pairing overlay is managed by another owner; do not change its policy; next call: p2p {{\"action\":\"pairings_list\"}}"
        );
        Ok(())
    }

    /// Writes as the Agent that owns the overlay, so DefraDB's document ACP and
    /// the reconciler's overlay-author check see that Agent, not the Node.
    async fn mutate_overlay(
        &self,
        agent: Arc<dyn NodeIdentity>,
        peer: &str,
        write: Option<OverlayWrite>,
    ) -> Result<bool> {
        let peer = peer.to_owned();
        let access = ConfigAccess::local_managed_agent(
            self.node.clone(),
            identity::Did::new(agent.did())?,
            agent.as_ref(),
        )?;
        let agent_did = agent.did().to_owned();
        access
            .transact_write("tool.p2p.overlay", move |txn| {
                let peer = peer.clone();
                let write = write.clone();
                let agent_did = agent_did.clone();
                Box::pin(async move {
                    let response = txn
                        .execute(&format!(
                            "{{DataPlanePairingDesired(filter: {{peer_id: {{_eq: {}}}}}, limit: 2) {{_docID peer_id node_did actor_did collections replicator_addresses template source enrollment_member_did enrollment_sequence enrollment_network_id}}}}",
                            quoted(&peer)
                        ))
                        .await?;
                    let rows = response["data"]["DataPlanePairingDesired"]
                        .as_array()
                        .context("missing pairing documents")?;
                    ensure!(rows.len() <= 1, "ambiguous pairing document");
                    let before = rows.first();
                    if let Some(row) = before {
                        ensure!(
                            row["actor_did"].as_str() == Some(agent_did.as_str()),
                            "pairing overlay is managed by another owner; do not change its policy"
                        );
                    }
                    let unchanged = match (&write, before) {
                        (None, None) => true,
                        (Some(write), Some(before)) => write
                            .desired
                            .as_object()
                            .context("invalid desired pairing")?
                            .iter()
                            .all(|(key, value)| before.get(key) == Some(value)),
                        _ => false,
                    };
                    if unchanged {
                        return Ok::<_, anyhow::Error>(true);
                    }
                    match (write, before) {
                        (Some(write), Some(row)) => {
                            txn.execute(&format!(
                                "mutation {{update_DataPlanePairingDesired(filter: {{_docID: {{_eq: {}}}}}, input: {}) {{_docID}}}}",
                                quoted(row["_docID"].as_str().context("overlay identity missing")?),
                                write.update_fields
                            ))
                            .await?;
                        }
                        (Some(write), None) => {
                            let created = txn
                                .execute(&format!(
                                    "mutation {{create_DataPlanePairingDesired(input: {}) {{_docID}}}}",
                                    write.create_fields
                                ))
                                .await?;
                            let doc = gents_protocol::graphql::extract_mutation_doc_id(
                                &created,
                                "DataPlanePairingDesired",
                            )?;
                            // Only a parented overlay reaches the runtime's reconciler.
                            txn.add_resource_relationship(
                                "DataPlanePairingDesired",
                                &doc,
                                "parent",
                                "nodes",
                                &write.parent_node_doc_id,
                            )
                            .await?;
                        }
                        (None, Some(row)) => {
                            txn.execute(&format!(
                                "mutation {{delete_DataPlanePairingDesired(filter: {{_docID: {{_eq: {}}}}}) {{_docID}}}}",
                                quoted(row["_docID"].as_str().context("overlay identity missing")?)
                            ))
                            .await?;
                        }
                        (None, None) => unreachable!("unchanged returns early"),
                    }
                    Ok::<_, anyhow::Error>(false)
                })
            })
            .await
    }

    async fn local_node_doc_id(&self, runtime: &dyn NodeIdentity) -> Result<String> {
        let rows = self
            .rows(
                "Node",
                Some(&format!("{{node_did: {{_eq: {}}}}}", quoted(runtime.did()))),
                "_docID",
            )
            .await?;
        rows.first()
            .and_then(|row| row["_docID"].as_str())
            .map(str::to_owned)
            .context(
                "this node has no visible Node document; next call: p2p {\"action\":\"status\"}",
            )
    }

    /// Whether one stored overlay contributes, judged by the overlay owner
    /// against the peer's current enrollment in this node's network.
    async fn overlay_status(
        &self,
        runtime: &dyn NodeIdentity,
        projection: &EnrollmentProjection,
        row: &Value,
    ) -> std::result::Result<(), OverlayInactive> {
        let now = chrono::Utc::now();
        let current = row["peer_id"].as_str().and_then(|peer| {
            projection
                .active
                .iter()
                .find(|entry| {
                    entry.request.candidate_peer == peer
                        && gents_protocol::enrollment::authorization_lease_is_fresh_at(
                            &entry.revision.authorization_expires_at,
                            now,
                        )
                })
                .map(|entry| CurrentAuthorization {
                    network_id: entry.revision.network_id.clone(),
                    member_did: entry.request.candidate_did.clone(),
                    sequence: entry.revision.sequence,
                })
        });
        let record = OverlayRecord::from_row(row)
            .ok_or_else(|| OverlayInactive::Unverifiable("malformed overlay".into()))?;
        let reader = ConfigAccess::local_as(
            self.node.clone(),
            identity::Did::new(runtime.did())
                .map_err(|error| OverlayInactive::Unverifiable(error.to_string()))?,
        );
        evaluate_overlay(
            &reader,
            runtime.did(),
            Some(runtime),
            &record,
            current.as_ref(),
        )
        .await
    }

    /// Whether this request's Agent can read the overlay document itself under
    /// native document ACP.
    async fn agent_reads_overlay(&self, doc_id: &str) -> bool {
        let Some(agent) = self.agent.as_ref() else {
            return false;
        };
        let Ok(did) = identity::Did::new(agent.did()) else {
            return false;
        };
        let Ok(access) = ConfigAccess::local_managed_agent(self.node.clone(), did, agent.as_ref())
        else {
            return false;
        };
        access
            .execute(&format!(
                "{{DataPlanePairingDesired(filter: {{_docID: {{_eq: {}}}}}) {{_docID}}}}",
                quoted(doc_id)
            ))
            .await
            .is_ok_and(|rows| {
                rows["data"]["DataPlanePairingDesired"]
                    .as_array()
                    .is_some_and(|rows| !rows.is_empty())
            })
    }

    /// Reads and admits `collections` for a pairing with one owner decision.
    async fn checked_collections(
        &self,
        options: &Map<String, Value>,
    ) -> Result<(BTreeSet<String>, ReplicatedSchema)> {
        let collections: BTreeSet<String> = serde_json::from_value(
            options
                .get("collections")
                .cloned()
                .context("collections is required for pairings_preview and pairings_apply; next call: p2p {\"action\":\"help\",\"topic\":\"pairings\"}")?,
        )
        .context("collections must be a list of application collection names; next call: p2p {\"action\":\"help\",\"topic\":\"pairings\"}")?;
        ensure!(!collections.is_empty(), "collections must name application collections; next call: p2p {{\"action\":\"help\",\"topic\":\"pairings\"}}");
        ensure!(
            collections.len() <= 16,
            "at most 16 application collections per pairing; next call: p2p {{\"action\":\"help\",\"topic\":\"pairings\"}}"
        );
        for name in &collections {
            validate_collection_identifier(name)?;
        }
        match admit_app_collections(self.node.clone(), &collections).await {
            Ok(schema) => Ok((collections, schema)),
            Err(AppCollectionAdmissionError::Refused {
                collection,
                refusal: AppCollectionRefusal::Managed,
            }) => Err(anyhow!("{collection} is a Gents protocol, runtime or credential collection. Those are never replicated as application data, and no tool grant or document ACP changes that; next call: p2p {{\"action\":\"help\",\"topic\":\"scope\"}}")),
            Err(AppCollectionAdmissionError::Refused {
                collection,
                refusal: AppCollectionRefusal::PolicyProtected,
            }) => Err(anyhow!("{collection} has a document access policy, so it cannot be paired: each replica is owned by its signer on the peer and the policy's grants do not replicate, so only that signer could read it there. Pair a collection without @policy, or keep this one on its home node and read it through authenticated query; next call: p2p {{\"action\":\"help\",\"topic\":\"pairings\"}}")),
            Err(AppCollectionAdmissionError::Refused {
                collection,
                refusal: AppCollectionRefusal::View,
            }) => Err(anyhow!("{collection} is a view: it is derived from other collections and cannot carry its own document policy, so it is never paired. Pair the source collection if it has no policy; next call: p2p {{\"action\":\"help\",\"topic\":\"pairings\"}}")),
            Err(AppCollectionAdmissionError::NotRegistered { collection }) => Err(anyhow!("collection {collection} is not registered on this node; create it with the schema tool here and on the peer with the same collection definition; next call: p2p {{\"action\":\"help\",\"topic\":\"pairings\"}}")),
            Err(error) => Err(error.into()),
        }
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
                outcome["grant"] = json!({"mutations":self.mutate});
            }
            return Ok(reply(outcome, None, Value::Null));
        }
        let actor = self.actor()?;
        let admin = EmbeddedRemoteP2pAdmin::new(self.node.clone());
        let options = &options;
        let allowed: &[&str] = match words.as_slice() {
            ["status"] | ["pairings", "list"] => &["details"],
            ["network", "list"] | ["enrollment", "pending" | "history"] => &[],
            ["enrollment", "offer"] => &["valid_for_secs"],
            ["pairings", "get"] => &["peer_id", "details"],
            ["network", "get"] => &["peer_id"],
            ["pairings", "preview" | "apply"] => &["peer_id", "peer_did", "collections"],
            ["pairings", "revoke"] => &["peer_id"],
            ["enrollment", "approve"] => &["request_id", "lease_seconds"],
            ["enrollment", "renewal_policy"] => &["auto_renew", "lease_seconds"],
            ["enrollment", "deny" | "revoke"] => &["request_id"],
            ["enrollment", "request"] => &["offer_token"],
            _ => {
                let resource = words.first().copied().filter(|word| {
                    matches!(*word, "status" | "network" | "pairings" | "enrollment")
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
                    crate::runtime::p2p::observability::observe_embedded_sync_status(&self.node)
                        .await?;
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
                    defra_p2p_adapter::TransportPeerId::new(peer.to_owned())
                        .map_err(anyhow::Error::msg)
                        .context("invalid transport peer ID; next call: p2p {\"action\":\"network_list\"}")?;
                }
                let filter = peer.map(|peer| format!("{{peer_id: {{_eq: {}}}}}", quoted(peer)));
                let desired = self
                    .rows(
                        "DataPlanePairingDesired",
                        filter.as_deref(),
                        "_docID peer_id node_did actor_did collections source template enrollment_member_did enrollment_sequence enrollment_network_id",
                    )
                    .await?;
                let applied = self
                    .rows(
                        "PeerPairingApplied",
                        filter.as_deref(),
                        "peer_id replicator_addresses",
                    )
                    .await?;
                let projection = GraphqlEnrollmentStore::new(self.node.clone(), actor.clone())
                    .load_projection()
                    .await?;
                // An Agent sees why its own pairings are inactive. Another
                // actor's pairing is listed only if native ACP lets this Agent
                // read it, and then only as active or inactive: the reason
                // would disclose that actor's authority.
                let own = self.agent.as_ref().map(|agent| agent.did().to_owned());
                let mut visible = Vec::with_capacity(desired.len());
                for mut row in desired {
                    let mine = own.is_some() && row["actor_did"].as_str() == own.as_deref();
                    if !mine
                        && !self
                            .agent_reads_overlay(row["_docID"].as_str().unwrap_or_default())
                            .await
                    {
                        continue;
                    }
                    let status = self.overlay_status(actor.as_ref(), &projection, &row).await;
                    row["status"] = json!(if status.is_ok() { "active" } else { "inactive" });
                    if let (true, Err(reason)) = (mine, status) {
                        row["inactive_reason"] = json!(reason.code());
                        row["inactive_detail"] = json!(reason.to_string());
                    }
                    visible.push(row);
                }
                let desired = visible;
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
                let mut details_call =
                    json!({"action":format!("pairings_{}", words[1]),"details":true});
                if let Some(peer) = peer {
                    details_call["peer_id"] = json!(peer);
                }
                Ok(reply(
                    json!({"desired":desired,"applied":applied}),
                    None,
                    json!({"enrolled_peers":enrolled,"connected_peers":bounded_observation(connected),"replicators":replicators,"limit":50,"details_call":details_call}),
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
                self.require_mutation()?;
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
                let p2p = crate::store::identity::node_p2p(&self.node)
                    .context("P2P is not running on this node")?;
                let validity = options.get("valid_for_secs").map_or(
                    Ok(crate::runtime::agent::p2p_reconcile::enrollment_offer::DEFAULT_OFFER_VALIDITY),
                    |value| {
                        value
                            .as_i64()
                            .map(chrono::Duration::seconds)
                            .context("valid_for_secs must be a whole number of seconds")
                    },
                )?;
                let did = actor.did().to_owned();
                let offer =
                    EnrollmentOfferIssuer::new(actor, p2p, network_id, did, "client".into())
                        .mint_valid_for(validity)
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
                let now = chrono::Utc::now();
                let mut history: Vec<_> = projection.pending.iter().map(|e| json!({"request_id":e.request.request_id,"state":"pending","peer_id":e.request.candidate_peer,"peer_did":e.request.candidate_did,"requested_at":e.request.issued_at,"request_expires_at":e.request.expires_at}))
                    .chain(projection.active.iter().map(|e| {
                        let lease = AuthorizationLease::at(&e.revision.authorization_expires_at, now);
                        json!({"request_id":e.request.request_id,"state":"approved","peer_id":e.request.candidate_peer,"peer_did":e.request.candidate_did,"requested_at":e.request.issued_at,"decided_at":e.decision.decided_at,"decided_by":e.decision.signer_did,"authorization_sequence":e.revision.sequence,"authorization_expires_at":e.revision.authorization_expires_at,"lease_state":lease.state,"lease_remaining_days":lease.remaining_days(),"renewal_due":lease.needs_renewal()})
                    }))
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
                self.require_mutation()?;
                let request =
                    crate::runtime::agent::p2p_reconcile::enrollment_candidate::request_enrollment(
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
            ["enrollment", "renewal_policy"] => {
                self.require_mutation()?;
                let enabled = options
                    .get("auto_renew")
                    .and_then(Value::as_bool)
                    .context("auto_renew must be true or false")?;
                let lease_seconds = options
                    .get("lease_seconds")
                    .map(|value| {
                        let lease = value
                            .as_u64()
                            .context("lease_seconds must be a positive whole number of seconds")?;
                        validate_approval_lease(lease)?;
                        Ok::<_, anyhow::Error>(lease)
                    })
                    .transpose()?;
                GraphqlEnrollmentStore::new(self.node.clone(), actor)
                    .set_renewal_policy(EnrollmentRenewalPolicy {
                        enabled,
                        lease_seconds,
                    })
                    .await?;
                Ok(reply(
                    json!({"auto_renew":enabled,"lease_seconds":lease_seconds}),
                    None,
                    Value::Null,
                ))
            }
            ["enrollment", operation @ ("approve" | "deny" | "revoke")] => {
                self.require_mutation()?;
                let store = GraphqlEnrollmentStore::new(self.node.clone(), actor);
                let id = text("request_id")?;
                let result = match *operation {
                    "approve" => {
                        let lease = options.get("lease_seconds").map_or(
                            Ok(gents_protocol::enrollment::DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS),
                            |value| {
                                let value = value.as_u64().context("lease_seconds must be a positive whole number of seconds")?;
                                validate_approval_lease(value)?;
                                Ok::<_, anyhow::Error>(value)
                            },
                        )?;
                        store.decide_request_with_lease(id, EnrollmentDecisionKind::Approved, std::time::Duration::from_secs(lease)).await
                    },
                    "deny" => store.decide_request(id, EnrollmentDecisionKind::Denied).await,
                    _ => store.revoke_request(id).await,
                }
                .map_err(|error| {
                    let reused = gents_protocol::error_chain::find_source::<
                        crate::runtime::agent::p2p_reconcile::EnrollmentDecisionRefusal,
                    >(error.as_ref())
                    .is_some_and(|refusal| {
                        refusal.code()
                            == gents_protocol::enrollment::EnrollmentDecisionRefusalCode::ChallengeReused
                    });
                    if reused {
                        error.context("an offer token is single use: two nodes requested with one offer, so both requests are refused. Each joining node needs its own fresh offer; mint one per node and have each re-request; next call: p2p {\"action\":\"enrollment_offer\"}")
                    } else {
                        error
                    }
                })?;
                Ok(reply(
                    json!({"request_id":result.request_id,"state":result.state,"delivery_pending":result.delivery_pending,"decision_doc_id":result.decision_doc_id,"revision_doc_id":result.revision_doc_id}),
                    Some(json!({"action":"enrollment_history"})),
                    Value::Null,
                ))
            }
            ["pairings", operation @ ("preview" | "apply")] => {
                let peer = text("peer_id")?;
                let active = self.enrollment(peer, Some(text("peer_did")?)).await?;
                let (collections, local_schema) = self.checked_collections(options).await?;
                let agent = self.pairing_agent(actor.as_ref())?;
                let before = self.overlay(peer).await?;
                if let Some(row) = &before {
                    Self::owned_overlay(row, agent.as_ref())?;
                }
                let member = active.request.candidate_did.clone();
                let sequence = active.revision.sequence;
                let network = active.revision.network_id.clone();
                let desired = json!({"peer_id":peer,"node_did":actor.did(),"actor_did":agent.did(),"collections":collections,"replicator_addresses":[active.request.candidate_ticket],"template":APP_COLLECTIONS_TEMPLATE,"source":OVERLAY_SOURCE,"enrollment_member_did":member,"enrollment_sequence":sequence,"enrollment_network_id":network});
                if *operation == "preview" {
                    let exposure: Vec<_> = collections
                        .iter()
                        .map(|name| format!("{name} has no document access policy: every document in it, including ones written before this pairing, will be readable by every actor on the receiving node"))
                        .collect();
                    return Ok(reply(
                        json!({"proposed":desired,"receiver_exposure":exposure,"current":before,"local_schema":local_schema,"peer_schema_compatibility":"not_checked"}),
                        None,
                        json!({"enrollment_request_id":active.request.request_id}),
                    ));
                }
                self.require_mutation()?;
                let parent_node_doc_id = self.local_node_doc_id(actor.as_ref()).await?;
                let addresses =
                    graphql_string_list_literal([active.request.candidate_ticket.as_str()]);
                let names = graphql_string_list_literal(collections.iter().map(String::as_str));
                let write = OverlayWrite {
                    desired: desired.clone(),
                    create_fields: format!(
                        "{{peer_id: {}, node_did: {}, actor_did: {}, collections: {names}, replicator_addresses: {addresses}, template: {}, source: {}, enrollment_member_did: {}, enrollment_sequence: {sequence}, enrollment_network_id: {}}}",
                        quoted(peer),
                        quoted(actor.did()),
                        quoted(agent.did()),
                        quoted(APP_COLLECTIONS_TEMPLATE),
                        quoted(OVERLAY_SOURCE),
                        quoted(&member),
                        quoted(&network)
                    ),
                    update_fields: format!(
                        "{{collections: {names}, replicator_addresses: {addresses}, enrollment_member_did: {}, enrollment_sequence: {sequence}, enrollment_network_id: {}}}",
                        quoted(&member),
                        quoted(&network)
                    ),
                    parent_node_doc_id,
                };
                let unchanged = self.mutate_overlay(agent, peer, Some(write)).await?;
                Ok(reply(
                    json!({"desired":desired,"unchanged":unchanged,"applied":"observe_existing_reconciler","local_schema":local_schema,"peer_schema_compatibility":"not_checked"}),
                    Some(json!({"action":"pairings_get","peer_id":peer})),
                    Value::Null,
                ))
            }
            ["pairings", "revoke"] => {
                self.require_mutation()?;
                let peer = text("peer_id")?;
                let agent = self.pairing_agent(actor.as_ref())?;
                // Another Agent's overlay is invisible inside this Agent's
                // transaction, so ownership is read here as the runtime.
                if let Some(row) = self.overlay(peer).await? {
                    Self::owned_overlay(&row, agent.as_ref())?;
                }
                let unchanged = self.mutate_overlay(agent, peer, None).await?;
                Ok(reply(
                    json!({"managed_overlay_removed":peer,"unchanged":unchanged}),
                    Some(json!({"action":"pairings_get","peer_id":peer})),
                    Value::Null,
                ))
            }
            _ => unreachable!(),
        }
    }
}

fn validate_approval_lease(seconds: u64) -> Result<()> {
    ensure!(
        (1..=gents_protocol::enrollment::MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS)
            .contains(&seconds),
        "lease_seconds must be between 1 and {}",
        gents_protocol::enrollment::MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS
    );
    Ok(())
}

fn peer_reference_matches(reference: &str, peer: &str) -> bool {
    reference == peer
        || p2p::iroh::parse_public_peer_addr(reference).is_ok_and(|(id, _)| id.as_str() == peer)
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
        None => "Actions: status; network_list, network_get; pairings_list, pairings_get, pairings_preview, pairings_apply, pairings_revoke; enrollment_offer, enrollment_request, enrollment_pending, enrollment_approve, enrollment_deny, enrollment_revoke, enrollment_history, enrollment_renewal_policy. Action help with topic status, network, pairings, enrollment or scope explains each; scope covers grants and capability boundaries. Identity comes from the running node and its agent. Mutation grants are explicit.",
        Some("status" | "network") => "status returns native connected peer references (raw IDs or transport addresses) and native sync facts. network_list lists up to 50 registered peers with exact DIDs and addresses. network_get takes peer_id, validates the transport identity and returns its registry record plus observed connection. A registry entry is discovery, not enrollment authority or current connectivity; network_get's connected field is the observed transport connection. status defaults to backlog facts; details:true returns native diagnostic counters.",
        Some("pairings") => "Pairing replicates application collections from this node to one enrolled peer on this node's own initiative. Enrollment never starts it. Recipe: the peer joins this node (enrollment_offer, enrollment_request, enrollment_approve), both nodes hold the same collection definition, then this node calls pairings_preview with peer_id, peer_did and collections (1-16 names), then pairings_apply with the same fields, then pairings_get to confirm the replicator. pairings_apply replicates this node's documents in those collections toward that peer, including documents written before the opt-in, and only in that direction; for data to flow both ways, the peer must apply its own pairing toward this node, which needs this node enrolled in the peer's network the same way. pairings_revoke with peer_id deletes this agent's overlay and stops new data; documents already delivered stay on the peer. A pairing is bound to the peer's enrollment when applied: a lease renewal keeps it, but revocation or expiry ends it for good, and a later enrollment needs pairings_apply again. It also stops while its agent is disabled or loses P2P mutation, and the node's user can remove any pairing. pairings_list and pairings_get report each of this agent's pairings with status and inactive_reason; another actor's pairing appears only if this agent may read it, as active or inactive. Collections must exist here, have no document policy and not be views. Gents protocol, runtime and credential collections are always refused. Collections with @policy are refused because a replica on the peer is owned by its signer and the policy's grants do not replicate; only that signer could read it there. Preview reports local schema identities but does not check the peer. On the peer, paired documents are untrusted content: in a collection without a policy every local actor can read them, a matching document ID merges into the peer's own document, and its triggers fire. pairings_list and pairings_get show desired, applied, enrolled and connected separately; details:true adds native replicator filters. An overlay another agent owns is not changed. The desired document is not proof of a live route; confirm with pairings_get, then a read on the peer.",
        Some("enrollment") => "Joining: the approving node runs enrollment_offer and hands offer_token to the joining node, which runs enrollment_request; the approver then finds it with enrollment_pending and approves it. enrollment_offer requires mutation authority and returns this node's signed offer_token, peer_id and DID; the token is valid 5 minutes unless valid_for_secs asks for up to 3600, which a relay through people or other agents needs, and the joining node must request before expires_at; the token only lets its holder ask to join, approval stays here, and it is single use: give each joining node its own offer, because requests from two nodes made with one offer are both refused. enrollment_request requires offer_token from the approving node and mutation authority. It verifies the peer's signed offer, signs a request with this node's identity and delivers it; the peer's operator approves it there with approve_on_peer. enrollment_pending discovers signed request IDs. enrollment_approve, enrollment_deny or enrollment_revoke requires request_id and mutation authority; deny signs a refusal of a pending request, revoke ends an approved one. The existing enrollment owner verifies the operator, network and request and signs the durable decision. Approval admits the peer to this node's network for 30 days by default; lease_seconds may set a lease of up to 90 days. It materializes its enrollment route, so approve only a request you were asked to approve whose peer_did you checked. Enrollment is one-way: approval enrolls the requester in this node's network, not this node in the requester's. Enrollment in both directions needs each node enrolled in the other's network, through a second offer, request and approval the other way. Do not invent a DID or substitute an unsigned enrollment document. enrollment_history lists up to 50 current pending, approved, denied and revoked requests, newest first, with decision times and signers; requests that expired undecided or were superseded by the same peer's later request are omitted. Approved entries carry lease_state, lease_remaining_days and renewal_due, which turns true inside the last 7 days of the lease. Automatic renewal is off by default. enrollment_renewal_policy sets auto_renew and an optional maximum lease_seconds for this network; disabling it immediately stops automatic approvals. Renewal is re-enrollment before expiry: the same four calls with a fresh offer, request and approval for the same peer; the approval takes the peer's next authorization sequence and supersedes the old generation in place, so the peer's enrollment route stays. After expiry the peer's routes and remote requests stop until it re-enrolls. Approval replicates no application data; pairings is a separate opt-in, and a pairing ends with the enrollment it was applied under: re-enrolling restores only the discovery route.",
        Some("scope") => "P2P carries discovery and enrollment documents, plus application collections this node has explicitly paired toward an enrolled peer. Enrollment alone starts no application sync. Gents protocol, runtime and credential collections are never paired, whatever tool grants or document ACP say. Collections with a document policy are refused because only their signer could read them on the peer. Pairing writes an overlay owned by this agent's own DID, so DefraDB ACP decides who may change it; the Node identity is never used in its place. Everything else stays on its home node; read it through query. Enrollment is a separate signed authority, and mutation authority gates offers, requests, decisions and pairings.",
        Some(resource) => bail!("unknown P2P help topic {resource:?}; topics: {HELP_TOPICS:?}; next call: p2p {{\"action\":\"help\",\"topic\":\"scope\"}}"),
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
            description: "Inspect this node's P2P peers, enroll nodes and opt application collections into replication toward an enrolled peer. Pick an action and send only its fields: status [details]; network_list; network_get peer_id; pairings_list [details]; pairings_get peer_id [details]; pairings_preview and pairings_apply peer_id peer_did collections; pairings_revoke peer_id; enrollment_offer [valid_for_secs]; enrollment_request offer_token; enrollment_pending; enrollment_approve request_id [lease_seconds]; enrollment_deny and enrollment_revoke request_id; enrollment_renewal_policy auto_renew [lease_seconds]; enrollment_history. A node joins in four calls: the approver's enrollment_offer, the joining node's enrollment_request, then the approver's enrollment_pending and enrollment_approve; the reverse direction needs the same four calls the other way. Action help with a topic explains rules. Enrollment and DefraDB enforce access.".into(),
            parameters: json!({
                "type": "object",
                "required": ["action"],
                "additionalProperties": false,
                "properties": {
                    "action": {"type": "string", "enum": ACTIONS, "description": "The P2P action; each lists its fields in the tool description."},
                    "topic": {"type": "string", "enum": HELP_TOPICS, "description": "help only: the topic to explain."},
                    "peer_id": text("Remote node's transport peer ID (hex), from enrollment_pending, pairings_list enrolled_peers or network_list. network_get, pairings_get/preview/apply/revoke."),
                    "peer_did": text("Remote node's DID from its signed enrollment (pairings_list enrolled_peers). pairings_preview/apply."),
                    "collections": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 16, "description": "Application collection names to replicate to the peer, e.g. [\"DeploymentNote\"]; each must exist here and have no document policy. pairings_preview/apply."},
                    "request_id": text("Enrollment request ID (enroll-...) from enrollment_pending or the joining node's enrollment_request. enrollment_approve/deny/revoke."),
                    "offer_token": text("The approving node's token from its enrollment_offer, valid until its expires_at (5 minutes by default). enrollment_request, run on the joining node."),
                    "valid_for_secs": {"type": "integer", "minimum": 1, "maximum": 3600, "default": 300, "description": "enrollment_offer only: how long the offer, and a request made from it, stays valid. Raise it when the token is relayed by hand."},
                    "lease_seconds": {"type": "integer", "minimum": 1, "maximum": gents_protocol::enrollment::MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS, "description": "enrollment_approve or enrollment_renewal_policy: authorization lease in seconds, at most 90 days. Omit on policy to retain the current lease duration."},
                    "auto_renew": {"type": "boolean", "description": "enrollment_renewal_policy only: enable or disable automatic renewal for this node's network."},
                    "details": {"type": "boolean", "description": "status, pairings_list, pairings_get: include native diagnostic counters, replicator collections and filters."}
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

fn bounded_observation<T>(mut items: Vec<T>) -> Value
where
    T: Serialize,
{
    let observed_count = items.len();
    items.truncate(50);
    json!({"items":items,"observed_count":observed_count,"limit":50})
}
