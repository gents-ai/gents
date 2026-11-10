//! Whether an application-collection pairing overlay may contribute its
//! collections to a route (Lean `PairingReconcile.OverlayBinding`). The pure
//! decision is [`binding_is_current`] and [`decide_overlay`];
//! [`evaluate_overlay`] reads the facts they take as the runtime.

use std::collections::BTreeSet;
use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use serde_json::Value;

use crate::store::config_client::{ConfigAccess, NodePermission};
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;

use super::templates::{admit_app_collections, AppCollectionAdmissionError};

/// One authorization revision of the bound member (Lean `Revision`). Times are
/// milliseconds since the Unix epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionFact {
    pub sequence: u64,
    pub active: bool,
    pub issued_at: i64,
    pub expires_at: i64,
}

/// The enrollment an overlay was applied under (Lean `Binding`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayBinding {
    pub member_did: String,
    pub sequence: u64,
}

/// The peer's current fresh authorization (Lean `Current`), with the scope its
/// revisions are read in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentAuthorization {
    pub network_id: String,
    pub member_did: String,
    pub sequence: u64,
}

/// Lean `bindingCurrent`: the same member has been authorized without
/// interruption from the bound sequence to the current one.
pub fn binding_is_current(
    history: &[RevisionFact],
    binding: &OverlayBinding,
    current_member: &str,
    current_sequence: u64,
) -> bool {
    if binding.member_did != current_member || binding.sequence > current_sequence {
        return false;
    }
    let contiguous = (binding.sequence..=current_sequence)
        .all(|sequence| history.iter().any(|revision| revision.sequence == sequence));
    contiguous
        && history.iter().all(|revision| {
            let in_window =
                binding.sequence <= revision.sequence && revision.sequence <= current_sequence;
            !in_window
                || (revision.active
                    && (revision.sequence == binding.sequence
                        || history.iter().all(|previous| {
                            previous.sequence + 1 != revision.sequence
                                || revision.issued_at < previous.expires_at
                        })))
        })
}

/// The facts one overlay is judged on (Lean `Facts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayFacts {
    pub enrollment_fresh: bool,
    pub binding_current: bool,
    pub author_live_agent: bool,
    pub author_holds_grant: bool,
    pub collections_admitted: bool,
}

/// Why an overlay contributes nothing (Lean `Inactive`), plus an unreadable
/// fact, which fails closed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OverlayInactive {
    #[error("the peer has no fresh enrollment in this node's network")]
    EnrollmentNotFresh,
    #[error(
        "the enrollment this pairing was applied under has ended (revoked, lapsed or replaced); \
         a later enrollment starts with no application sync until the pairing is applied again"
    )]
    BindingEnded,
    #[error("the pairing's author is not an enabled Agent of this node")]
    AuthorNotAgent,
    #[error("the pairing's author does not currently hold the P2P mutation grant")]
    AuthorLacksGrant,
    #[error("a paired collection is refused: {0}")]
    CollectionsRefused(String),
    #[error("the pairing could not be checked, so it contributes nothing: {0}")]
    Unverifiable(String),
}

impl OverlayInactive {
    /// The model's reason name; `None` for an unreadable fact.
    pub fn model_name(&self) -> Option<&'static str> {
        Some(match self {
            Self::EnrollmentNotFresh => "enrollment_not_fresh",
            Self::BindingEnded => "binding_ended",
            Self::AuthorNotAgent => "author_not_agent",
            Self::AuthorLacksGrant => "author_lacks_grant",
            Self::CollectionsRefused(_) => "collections_refused",
            Self::Unverifiable(_) => return None,
        })
    }

    pub fn code(&self) -> &'static str {
        self.model_name().unwrap_or("unverifiable")
    }
}

/// Lean `decideOverlay`: the first failed precondition in model order.
pub fn decide_overlay(facts: OverlayFacts) -> Result<(), OverlayInactive> {
    if !facts.enrollment_fresh {
        Err(OverlayInactive::EnrollmentNotFresh)
    } else if !facts.binding_current {
        Err(OverlayInactive::BindingEnded)
    } else if !facts.author_live_agent {
        Err(OverlayInactive::AuthorNotAgent)
    } else if !facts.author_holds_grant {
        Err(OverlayInactive::AuthorLacksGrant)
    } else if !facts.collections_admitted {
        Err(OverlayInactive::CollectionsRefused(String::new()))
    } else {
        Ok(())
    }
}

/// One stored overlay, as the reconciler and the tool read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayRecord {
    pub actor_did: String,
    pub collections: BTreeSet<String>,
    pub binding: Option<OverlayBinding>,
}

impl OverlayRecord {
    pub fn from_row(row: &Value) -> Option<Self> {
        let binding = match (
            row["enrollment_member_did"].as_str(),
            row["enrollment_sequence"].as_u64(),
        ) {
            (Some(member), Some(sequence)) if !member.is_empty() => Some(OverlayBinding {
                member_did: member.to_owned(),
                sequence,
            }),
            _ => None,
        };
        Some(Self {
            actor_did: row["actor_did"].as_str()?.to_owned(),
            collections: row["collections"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect(),
            binding,
        })
    }
}

/// Reads each fact as the runtime, in model order, and stops at the first
/// that fails. `current` is the peer's fresh authorization, or `None`.
pub async fn evaluate_overlay(
    node: &Arc<EmbeddedNode>,
    runtime: &dyn NodeIdentity,
    overlay: &OverlayRecord,
    current: Option<&CurrentAuthorization>,
) -> Result<(), OverlayInactive> {
    let Some(current) = current else {
        return Err(OverlayInactive::EnrollmentNotFresh);
    };
    let Some(binding) = overlay.binding.as_ref() else {
        return Err(OverlayInactive::BindingEnded);
    };
    let history = member_revisions(node, &current.network_id, &current.member_did)
        .await
        .map_err(|error| OverlayInactive::Unverifiable(format!("{error:#}")))?;
    if !binding_is_current(&history, binding, &current.member_did, current.sequence) {
        return Err(OverlayInactive::BindingEnded);
    }
    match author_authority(node, runtime, &overlay.actor_did).await {
        Ok(AuthorAuthority::Holds) => {}
        Ok(AuthorAuthority::NotAgent) => return Err(OverlayInactive::AuthorNotAgent),
        Ok(AuthorAuthority::LacksGrant) => return Err(OverlayInactive::AuthorLacksGrant),
        Err(error) => return Err(OverlayInactive::Unverifiable(format!("{error:#}"))),
    }
    if overlay.collections.is_empty() {
        return Err(OverlayInactive::CollectionsRefused(
            "the pairing names no collection".into(),
        ));
    }
    match admit_app_collections(node.clone(), &overlay.collections).await {
        Ok(_) => Ok(()),
        Err(
            error @ (AppCollectionAdmissionError::Refused { .. }
            | AppCollectionAdmissionError::NotRegistered { .. }),
        ) => Err(OverlayInactive::CollectionsRefused(error.to_string())),
        Err(error) => Err(OverlayInactive::Unverifiable(error.to_string())),
    }
}

/// Every authorization revision of one member in one network, read the way
/// the enrollment owner reads them.
async fn member_revisions(
    node: &EmbeddedNode,
    network_id: &str,
    member_did: &str,
) -> anyhow::Result<Vec<RevisionFact>> {
    let query = format!(
        r#"{{ NetworkAuthorizationRevision(filter: {{network_id: {{_eq: "{}"}}, member_did: {{_eq: "{}"}}}}) {{ sequence kind issued_at authorization_expires_at }} }}"#,
        escape_graphql_string(network_id),
        escape_graphql_string(member_did)
    );
    let response = crate::store::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "read pairing overlay enrollment history",
    )
    .await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("NetworkAuthorizationRevision"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("enrollment history omitted revisions"))?;
    rows.iter()
        .map(|row| {
            let time = |field: &str| -> anyhow::Result<i64> {
                Ok(chrono::DateTime::parse_from_rfc3339(
                    row[field]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("revision has no {field}"))?,
                )?
                .timestamp_millis())
            };
            Ok(RevisionFact {
                sequence: row["sequence"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("revision has no sequence"))?,
                active: match row["kind"].as_str() {
                    Some("active") => true,
                    Some("revoked") => false,
                    other => anyhow::bail!("unknown revision kind {other:?}"),
                },
                issued_at: time("issued_at")?,
                expires_at: time("authorization_expires_at")?,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthorAuthority {
    NotAgent,
    LacksGrant,
    Holds,
}

/// Whether the overlay's author is an enabled, managed Agent of this node that
/// currently holds the native `delete-document` permission through the
/// `p2p-pairing` source the tool permission projection installs for its
/// selected Tools. Native NAC is asked, so a withdrawn grant, a disabled or
/// deleted Agent, or a reselected Tools document all read as not holding it.
async fn author_authority(
    node: &Arc<EmbeddedNode>,
    runtime: &dyn NodeIdentity,
    author: &str,
) -> anyhow::Result<AuthorAuthority> {
    let node_did = runtime.did();
    let access = ConfigAccess::local_as(node.clone(), identity::Did::new(node_did)?);
    let agents = access
        .execute(&format!(
            r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}, node_did: {{_eq: "{}"}}}}) {{ _docID enabled context_id creation_nonce }} }}"#,
            escape_graphql_string(author),
            escape_graphql_string(node_did)
        ))
        .await?;
    let agents = agents["data"]["Agent"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let [agent] = agents.as_slice() else {
        return Ok(AuthorAuthority::NotAgent);
    };
    if agent["enabled"].as_bool() == Some(false) {
        return Ok(AuthorAuthority::NotAgent);
    }
    let Some(nonce) = agent["creation_nonce"].as_str() else {
        return Ok(AuthorAuthority::NotAgent);
    };
    let binding = gents_protocol::agent_identity::AgentIdentityBinding {
        agent_did: author.to_owned(),
        creation_nonce: nonce.to_owned(),
    };
    if crate::store::identity::resolve_agent_identity(runtime, &binding).is_err() {
        return Ok(AuthorAuthority::NotAgent);
    }
    let Some(context_id) = agent["context_id"].as_str().filter(|id| !id.is_empty()) else {
        return Ok(AuthorAuthority::LacksGrant);
    };
    let contexts = access
        .execute(&format!(
            r#"{{ AgentContext(filter: {{context_id: {{_eq: "{}"}}, node_did: {{_eq: "{}"}}}}) {{ _docID node_did context_id tools_id }} }}"#,
            escape_graphql_string(context_id),
            escape_graphql_string(node_did)
        ))
        .await?;
    let contexts = contexts["data"]["AgentContext"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let [context] = contexts.as_slice() else {
        return Ok(AuthorAuthority::LacksGrant);
    };
    // The projection reads document fields only; the typed Context decoder
    // refuses the `_docID` system field.
    let mut fields = context.clone();
    if let Some(object) = fields.as_object_mut() {
        object.remove("_docID");
    }
    let Some(tools_id) =
        crate::store::local::tool_permission_projection::selected_tools(node_did, &fields)
    else {
        return Ok(AuthorAuthority::LacksGrant);
    };
    let tools = access
        .execute(&format!(
            r#"{{ Tools(filter: {{tools_id: {{_eq: "{}"}}, node_did: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
            escape_graphql_string(&tools_id),
            escape_graphql_string(node_did)
        ))
        .await?;
    let tools = tools["data"]["Tools"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let ([tools], Some(context_doc)) = (tools.as_slice(), context["_docID"].as_str()) else {
        return Ok(AuthorAuthority::LacksGrant);
    };
    let Some(tools_doc) = tools["_docID"].as_str() else {
        return Ok(AuthorAuthority::LacksGrant);
    };
    // Native NAC names a source by the name the projection installed.
    let source = crate::store::local::tool_permission_projection::tool_source(
        node_did,
        tools_doc,
        context_doc,
        author,
        gents_protocol::peer_schema::P2P_PAIRING_SOURCE_PURPOSE,
    );
    let grants = access
        .explain_node_subject(&identity::Did::new(author)?)
        .await?;
    let holds = grants.iter().any(|grant| {
        grant.permission == NodePermission::DocumentDelete
            && matches!(&grant.via, acp::nac::PermissionVia::Source { path } if path.contains(&source))
    });
    Ok(if holds {
        AuthorAuthority::Holds
    } else {
        AuthorAuthority::LacksGrant
    })
}
