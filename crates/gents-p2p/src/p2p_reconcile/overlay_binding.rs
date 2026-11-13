use std::collections::BTreeSet;

use serde_json::Value;

use gents_store::config_client::{ConfigAccess, NodePermission};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;

use super::templates::{
    admit_app_collections_with, AppCollectionAdmissionError, APP_COLLECTIONS_TEMPLATE,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionFact {
    pub sequence: u64,
    pub active: bool,
    pub issued_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayBinding {
    pub network_id: String,
    pub member_did: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentAuthorization {
    pub network_id: String,
    pub member_did: String,
    pub sequence: u64,
}

pub fn binding_is_current(
    history: &[RevisionFact],
    binding: &OverlayBinding,
    current: &CurrentAuthorization,
) -> bool {
    let current_sequence = current.sequence;
    if binding.network_id != current.network_id
        || binding.member_did != current.member_did
        || binding.sequence > current_sequence
    {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayFacts {
    pub enrollment_fresh: bool,
    pub binding_current: bool,
    pub author_live_agent: bool,
    pub author_holds_grant: bool,
    pub content_owner_written: bool,
    pub template_valid: bool,
    pub collections_admitted: bool,
}

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
    #[error(
        "the pairing was written by someone other than its author; only the author's own \
         writes count, so it must be applied again by the author or removed"
    )]
    ForeignWrite,
    #[error("the pairing names no application-collections template")]
    InvalidTemplate,
    #[error("a paired collection is refused: {0}")]
    CollectionsRefused(String),
    #[error("the pairing could not be checked, so it contributes nothing: {0}")]
    Unverifiable(String),
}

impl OverlayInactive {
    pub fn model_name(&self) -> Option<&'static str> {
        Some(match self {
            Self::EnrollmentNotFresh => "enrollment_not_fresh",
            Self::BindingEnded => "binding_ended",
            Self::AuthorNotAgent => "author_not_agent",
            Self::AuthorLacksGrant => "author_lacks_grant",
            Self::ForeignWrite => "foreign_write",
            Self::InvalidTemplate => "invalid_template",
            Self::CollectionsRefused(_) => "collections_refused",
            Self::Unverifiable(_) => return None,
        })
    }

    pub fn code(&self) -> &'static str {
        self.model_name().unwrap_or("unverifiable")
    }
}

pub fn decide_overlay(facts: OverlayFacts) -> Result<(), OverlayInactive> {
    if !facts.enrollment_fresh {
        Err(OverlayInactive::EnrollmentNotFresh)
    } else if !facts.binding_current {
        Err(OverlayInactive::BindingEnded)
    } else if !facts.author_live_agent {
        Err(OverlayInactive::AuthorNotAgent)
    } else if !facts.author_holds_grant {
        Err(OverlayInactive::AuthorLacksGrant)
    } else if !facts.content_owner_written {
        Err(OverlayInactive::ForeignWrite)
    } else if !facts.template_valid {
        Err(OverlayInactive::InvalidTemplate)
    } else if !facts.collections_admitted {
        Err(OverlayInactive::CollectionsRefused(String::new()))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayRecord {
    pub doc_id: String,
    pub actor_did: String,
    pub template: Option<String>,
    pub collections: BTreeSet<String>,
    pub binding: Option<OverlayBinding>,
}

impl OverlayRecord {
    pub fn from_row(row: &Value) -> Option<Self> {
        let binding = match (
            row["enrollment_network_id"].as_str(),
            row["enrollment_member_did"].as_str(),
            row["enrollment_sequence"].as_u64(),
        ) {
            (Some(network), Some(member), Some(sequence))
                if !network.is_empty() && !member.is_empty() =>
            {
                Some(OverlayBinding {
                    network_id: network.to_owned(),
                    member_did: member.to_owned(),
                    sequence,
                })
            }
            _ => None,
        };
        Some(Self {
            doc_id: row["_docID"].as_str()?.to_owned(),
            actor_did: row["actor_did"].as_str()?.to_owned(),
            template: row["template"].as_str().map(str::to_owned),
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

pub async fn evaluate_overlay(
    reader: &ConfigAccess,
    node_did: &str,
    runtime: Option<&dyn NodeIdentity>,
    overlay: &OverlayRecord,
    current: Option<&CurrentAuthorization>,
) -> Result<(), OverlayInactive> {
    let unverifiable = |error: anyhow::Error| OverlayInactive::Unverifiable(format!("{error:#}"));
    let binding_current = match (current, overlay.binding.as_ref()) {
        (Some(current), Some(binding)) => {
            let history = member_revisions(reader, &current.network_id, &current.member_did)
                .await
                .map_err(unverifiable)?;
            binding_is_current(&history, binding, current)
        }
        _ => false,
    };
    let authority = author_authority(reader, node_did, runtime, &overlay.actor_did)
        .await
        .map_err(unverifiable)?;
    let content_owner_written = written_only_by(reader, &overlay.doc_id, &overlay.actor_did)
        .await
        .map_err(unverifiable)?;
    let admission = if overlay.collections.is_empty() {
        Err("the pairing names no collection".to_owned())
    } else {
        match admit_app_collections_with(reader, &overlay.collections).await {
            Ok(_) => Ok(()),
            Err(
                error @ (AppCollectionAdmissionError::Refused { .. }
                | AppCollectionAdmissionError::NotRegistered { .. }),
            ) => Err(error.to_string()),
            Err(error) => return Err(OverlayInactive::Unverifiable(error.to_string())),
        }
    };
    let facts = OverlayFacts {
        enrollment_fresh: current.is_some(),
        binding_current,
        author_live_agent: authority != AuthorAuthority::NotAgent,
        author_holds_grant: authority == AuthorAuthority::Holds,
        content_owner_written,
        template_valid: overlay.template.as_deref() == Some(APP_COLLECTIONS_TEMPLATE),
        collections_admitted: admission.is_ok(),
    };
    match decide_overlay(facts) {
        Err(OverlayInactive::CollectionsRefused(_)) => Err(OverlayInactive::CollectionsRefused(
            admission.err().unwrap_or_default(),
        )),
        decision => decision,
    }
}

async fn written_only_by(
    reader: &ConfigAccess,
    doc_id: &str,
    author: &str,
) -> anyhow::Result<bool> {
    let signer = gents_store::identity::commit_signer_identity_for_did(author)?;
    let history = reader
        .execute(&format!(
            r#"{{ _commits(docID: "{}") {{ signature {{ identity }} }} }}"#,
            escape_graphql_string(doc_id)
        ))
        .await?;
    let commits = history["data"]["_commits"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("pairing commit history is unreadable"))?;
    anyhow::ensure!(!commits.is_empty(), "pairing commit history is empty");
    Ok(commits
        .iter()
        .all(|commit| commit["signature"]["identity"].as_str() == Some(signer.as_str())))
}

async fn member_revisions(
    reader: &ConfigAccess,
    network_id: &str,
    member_did: &str,
) -> anyhow::Result<Vec<RevisionFact>> {
    let query = format!(
        r#"{{ NetworkAuthorizationRevision(filter: {{network_id: {{_eq: "{}"}}, member_did: {{_eq: "{}"}}}}) {{ sequence kind issued_at authorization_expires_at }} }}"#,
        escape_graphql_string(network_id),
        escape_graphql_string(member_did)
    );
    let response = reader.execute(&query).await?;
    let rows = response["data"]["NetworkAuthorizationRevision"]
        .as_array()
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

async fn author_authority(
    access: &ConfigAccess,
    node_did: &str,
    runtime: Option<&dyn NodeIdentity>,
    author: &str,
) -> anyhow::Result<AuthorAuthority> {
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
    if runtime.is_some_and(|runtime| {
        gents_store::identity::resolve_agent_identity(runtime, &binding).is_err()
    }) {
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
    let mut fields = context.clone();
    if let Some(object) = fields.as_object_mut() {
        object.remove("_docID");
    }
    let Some(tools_id) = selected_tools(node_did, &fields) else {
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
    let source = gents_store::local::tool_permission_projection::tool_source(
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

pub async fn operator_pairing_statuses(access: &ConfigAccess) -> anyhow::Result<Vec<Value>> {
    let mut pairings = access.list_application_pairings().await?;
    let now = chrono::Utc::now().timestamp_millis();
    for row in &mut pairings {
        let status = match OverlayRecord::from_row(row) {
            None => Err(OverlayInactive::Unverifiable("malformed overlay".into())),
            Some(record) => {
                let current = match record.binding.as_ref() {
                    None => None,
                    Some(binding) => {
                        member_revisions(access, &binding.network_id, &binding.member_did)
                            .await?
                            .into_iter()
                            .max_by_key(|revision| revision.sequence)
                            .filter(|revision| revision.active && revision.expires_at > now)
                            .map(|revision| CurrentAuthorization {
                                network_id: binding.network_id.clone(),
                                member_did: binding.member_did.clone(),
                                sequence: revision.sequence,
                            })
                    }
                };
                let node_did = row["node_did"].as_str().unwrap_or_default().to_owned();
                evaluate_overlay(access, &node_did, None, &record, current.as_ref()).await
            }
        };
        row["status"] = Value::from(if status.is_ok() { "active" } else { "inactive" });
        if let Err(reason) = status {
            row["inactive_reason"] = Value::from(reason.code());
            row["inactive_detail"] = Value::from(reason.to_string());
        }
    }
    Ok(pairings)
}

fn selected_tools(node: &str, context: &Value) -> Option<String> {
    gents_documents::references_of(
        gents_store::collection::Collection::AgentContext,
        context,
        node,
    )
    .ok()?
    .into_iter()
    .find(|reference| reference.collection == gents_store::collection::Collection::Tools)
    .map(|reference| reference.id)
    .filter(|id| !id.is_empty())
}

#[cfg(test)]
mod conformance;
