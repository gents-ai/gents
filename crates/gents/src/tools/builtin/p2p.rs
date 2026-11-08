use std::sync::Arc;

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::defra_node::Node as EmbeddedNode;
use crate::inference::llm::tool::{Tool, ToolDefinition};
use crate::runtime::agent::p2p_reconcile::{
    EmbeddedRemoteP2pAdmin, EnrollmentOfferIssuer, GraphqlEnrollmentStore, RemoteP2pAdmin,
};
use crate::store::config_client::ConfigAccess;
use crate::store::graphql::{escape_graphql_string, validate_collection_identifier};
use crate::NodeIdentity;
use gents_protocol::enrollment::{AuthorizationLease, EnrollmentDecisionKind};

#[cfg(test)]
mod tests;

pub const P2P_TOOL_NAME: &str = "p2p";

#[derive(Clone)]
pub struct P2pTool {
    node: Arc<EmbeddedNode>,
    identity: Option<Arc<dyn NodeIdentity>>,
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
    pub request_id: Option<Value>,
    #[serde(default)]
    pub offer_token: Option<Value>,
    #[serde(default)]
    pub details: Option<Value>,
    #[serde(default)]
    pub valid_for_secs: Option<Value>,
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
    "enrollment_offer",
    "enrollment_request",
    "enrollment_pending",
    "enrollment_approve",
    "enrollment_deny",
    "enrollment_revoke",
    "enrollment_history",
];
const HELP_TOPICS: &[&str] = &["status", "network", "enrollment", "scope"];

impl P2pParams {
    fn command(self) -> Result<(Vec<String>, Map<String, Value>)> {
        let mut options = self.options;
        for (key, value) in [
            ("peer_id", self.peer_id),
            ("request_id", self.request_id),
            ("offer_token", self.offer_token),
            ("details", self.details),
            ("valid_for_secs", self.valid_for_secs),
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

impl P2pTool {
    pub fn new(
        node: Arc<EmbeddedNode>,
        identity: Option<Arc<dyn NodeIdentity>>,
        mutate: bool,
    ) -> Self {
        Self {
            node,
            identity,
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
            ["status"] => &["details"],
            ["network", "list"] | ["enrollment", "pending" | "history"] => &[],
            ["enrollment", "offer"] => &["valid_for_secs"],
            ["network", "get"] => &["peer_id"],
            ["enrollment", "approve" | "deny" | "revoke"] => &["request_id"],
            ["enrollment", "request"] => &["offer_token"],
            _ => {
                let resource = words
                    .first()
                    .copied()
                    .filter(|word| matches!(*word, "status" | "network" | "enrollment"));
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
                ensure!(self.mutate, "P2P mutations are disabled by the tool grant");
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
        None => "Actions: status; network_list, network_get; enrollment_offer, enrollment_request, enrollment_pending, enrollment_approve, enrollment_deny, enrollment_revoke, enrollment_history. Action help with topic status, network, enrollment or scope explains each; scope covers grants and capability boundaries. Identity comes from the running node. Mutation grants are explicit.",
        Some("status" | "network") => "status returns native connected peer references (raw IDs or transport addresses) and native sync facts. network_list lists up to 50 registered peers with exact DIDs and addresses. network_get takes peer_id, validates the transport identity and returns its registry record plus observed connection. A registry entry is discovery, not enrollment authority or current connectivity; network_get's connected field is the observed transport connection. status defaults to backlog facts; details:true returns native diagnostic counters.",
        Some("enrollment") => "Joining: the approving node runs enrollment_offer and hands offer_token to the joining node, which runs enrollment_request; the approver then finds it with enrollment_pending and approves it. enrollment_offer requires mutation authority and returns this node's signed offer_token, peer_id and DID; the token is valid 5 minutes unless valid_for_secs asks for up to 3600, which a relay through people or other agents needs, and the joining node must request before expires_at; the token only lets its holder ask to join, approval stays here, and it is single use: give each joining node its own offer, because requests from two nodes made with one offer are both refused. enrollment_request requires offer_token from the approving node and mutation authority. It verifies the peer's signed offer, signs a request with this node's identity and delivers it; the peer's operator approves it there with approve_on_peer. enrollment_pending discovers signed request IDs. enrollment_approve, enrollment_deny or enrollment_revoke requires request_id and mutation authority; deny signs a refusal of a pending request, revoke ends an approved one. The existing enrollment owner verifies the operator, network and request and signs the durable decision. Approval admits the peer to this node's network for the default authorization lease and materializes its enrollment route, so approve only a request you were asked to approve whose peer_did you checked. Enrollment is one-way: approval enrolls the requester in this node's network, not this node in the requester's. Enrollment in both directions needs each node enrolled in the other's network, through a second offer, request and approval the other way. Do not invent a DID or substitute an unsigned enrollment document. enrollment_history lists up to 50 current pending, approved, denied and revoked requests, newest first, with decision times and signers; requests that expired undecided or were superseded by the same peer's later request are omitted. Approved entries carry lease_state, lease_remaining_days and renewal_due, which turns true inside the last 7 days of the lease. Renewal is re-enrollment before expiry: the same four calls with a fresh offer, request and approval for the same peer; the approval takes the peer's next authorization sequence and supersedes the old generation in place, so the peer's enrollment route stays. After expiry the peer's routes and remote requests stop until it re-enrolls.",
        Some("scope") => "P2P carries discovery and enrollment only. This tool never pairs or syncs application, protocol configuration or credential collections, whatever the tool grants or document ACP say; read authorized documents through query. Enrollment is a separate signed authority, and mutation authority gates offers, requests and decisions.",
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
            description: "Inspect this node's P2P peers and enroll nodes. Pick an action and send only its fields: status [details]; network_list; network_get peer_id; enrollment_offer [valid_for_secs]; enrollment_request offer_token; enrollment_pending; enrollment_approve, enrollment_deny and enrollment_revoke request_id; enrollment_history. A node joins in four calls: the approver's enrollment_offer, the joining node's enrollment_request, then the approver's enrollment_pending and enrollment_approve. The reverse direction needs the same four calls the other way. Action help with a topic explains rules. Enrollment and DefraDB enforce access.".into(),
            parameters: json!({
                "type": "object",
                "required": ["action"],
                "additionalProperties": false,
                "properties": {
                    "action": {"type": "string", "enum": ACTIONS, "description": "The P2P action; each lists its fields in the tool description."},
                    "topic": {"type": "string", "enum": HELP_TOPICS, "description": "help only: the topic to explain."},
                    "peer_id": text("Remote node's transport peer ID (hex), from enrollment_pending or network_list. network_get."),
                    "request_id": text("Enrollment request ID (enroll-...) from enrollment_pending or the joining node's enrollment_request. enrollment_approve/deny/revoke."),
                    "offer_token": text("The approving node's token from its enrollment_offer, valid until its expires_at (5 minutes by default). enrollment_request, run on the joining node."),
                    "valid_for_secs": {"type": "integer", "minimum": 1, "maximum": 3600, "default": 300, "description": "enrollment_offer only: how long the offer, and a request made from it, stays valid. Raise it when the token is relayed by hand."},
                    "details": {"type": "boolean", "description": "status only: include native diagnostic counters."}
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
