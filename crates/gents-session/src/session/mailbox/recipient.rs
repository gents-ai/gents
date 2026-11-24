use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use gents_protocol::request_admission::{AgentRequestAdmissionKind, RuntimeInternalSourceKind};
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;

/// Lean `Identity.MailboxAccess.Admission`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Admission {
    Client,
    AutomatedTrigger,
    LocalControl,
    Other,
}

impl Admission {
    fn of(row: &AgentRequestRow) -> Self {
        let kind = row
            .admission_kind
            .as_deref()
            .and_then(|kind| AgentRequestAdmissionKind::try_from(kind).ok());
        let source = row
            .runtime_source_kind
            .as_deref()
            .and_then(|source| RuntimeInternalSourceKind::try_from(source).ok());
        match (kind, source) {
            (Some(AgentRequestAdmissionKind::Client), _) => Self::Client,
            (
                Some(AgentRequestAdmissionKind::RuntimeInternal),
                Some(RuntimeInternalSourceKind::AutomatedTrigger),
            ) => Self::AutomatedTrigger,
            (
                Some(AgentRequestAdmissionKind::RuntimeInternal),
                Some(RuntimeInternalSourceKind::LocalControl),
            ) => Self::LocalControl,
            _ => Self::Other,
        }
    }
}

/// Lean `Identity.MailboxAccess.Hop`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hop {
    pub(crate) admission: Admission,
    pub(crate) signer: String,
}

/// Lean `Identity.MailboxAccess.LineageRoot`.
enum LineageRoot<'a> {
    Signed(&'a str),
    AutomatedTrigger,
    Other,
}

/// Lean `Identity.MailboxAccess.lineageRoot`: the first admission past the
/// filing request's continuations.
fn lineage_root(lineage: &[Hop]) -> LineageRoot<'_> {
    match lineage
        .iter()
        .find(|hop| hop.admission != Admission::LocalControl)
    {
        Some(Hop {
            admission: Admission::Client,
            signer,
        }) => LineageRoot::Signed(signer),
        Some(Hop {
            admission: Admission::AutomatedTrigger,
            ..
        }) => LineageRoot::AutomatedTrigger,
        _ => LineageRoot::Other,
    }
}

/// Lean `Identity.MailboxAccess.filingRecipient`: the filing request's
/// admitted signer; for a continuation, the signer of its lineage's root; for
/// a run a trigger started, the Node's owner; else no one. `lineage` holds the
/// filing request's admission, then its parents'.
pub(crate) fn filing_recipient(
    claimed_signer: Option<&str>,
    lineage: &[Hop],
    node_owner: Option<&str>,
) -> Option<String> {
    let continues = lineage
        .first()
        .is_some_and(|hop| hop.admission == Admission::LocalControl);
    match (claimed_signer, lineage_root(lineage)) {
        (Some(signer), _) => Some(signer.to_owned()),
        (None, LineageRoot::AutomatedTrigger) => node_owner.map(str::to_owned),
        (None, LineageRoot::Signed(signer)) if continues => Some(signer.to_owned()),
        (None, _) => None,
    }
}

/// The recipient of an item the claimed `request` files.
pub(crate) async fn observed_recipient(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    request: &gents_loop::tool::ToolRequestContext,
) -> Result<Option<String>> {
    if let Some(admitted) = &request.admitted_client {
        anyhow::ensure!(
            admitted.request_doc_id == request.request_doc_id,
            "mailbox request provenance does not match the claimed request"
        );
        return Ok(filing_recipient(Some(&admitted.signer_did), &[], None));
    }
    let lineage = lineage(access, &request.request_doc_id).await?;
    let owner = match lineage_root(&lineage) {
        LineageRoot::AutomatedTrigger => node_owner(node, &request.node_did).await?,
        LineageRoot::Signed(_) | LineageRoot::Other => None,
    };
    Ok(filing_recipient(None, &lineage, owner.as_deref()))
}

/// The filing request's admission, then each parent's while the request
/// before it is a continuation: through its stored parent link, and only when
/// its signed source request is that parent; a parent it does not name reads
/// as `Other`.
async fn lineage(access: &ConfigAccess, request_doc_id: &str) -> Result<Vec<Hop>> {
    use gents_protocol::document_query::{
        fields::agent_request as fields, Filter, Query, Selection,
    };
    let mut lineage = Vec::new();
    let mut seen = rapidhash::RapidHashSet::default();
    let mut next = Some((request_doc_id.to_owned(), None::<Option<String>>));
    while let Some((doc_id, named_source)) = next.take() {
        if !seen.insert(doc_id.clone()) {
            break;
        }
        let query = Query::select(
            Selection::doc_id()
                .field(fields::REQUEST_ID)
                .field(fields::ADMISSION_KIND)
                .field(fields::ADMISSION_SIGNER_DID)
                .field(fields::RUNTIME_SOURCE_KIND)
                .field(fields::RUNTIME_SOURCE_REQUEST_ID)
                .field(fields::CAUSED_BY_PARENT_REQUEST_DOC_ID),
        )
        .filter(Filter::eq(fields::DOC_ID, &doc_id))
        .limit(1)
        .render();
        let response = access.read(&query).await?;
        let Some(row) = response.pointer("/data/AgentRequest/0") else {
            break;
        };
        let row: AgentRequestRow =
            serde_json::from_value(row.clone()).context("decode filing request lineage")?;
        let named = named_source.is_none_or(|source| source.as_deref() == Some(&row.request_id));
        let admission = match named {
            true => Admission::of(&row),
            false => Admission::Other,
        };
        lineage.push(Hop {
            admission,
            signer: row.admission_signer_did.clone().unwrap_or_default(),
        });
        if admission == Admission::LocalControl {
            next = row
                .caused_by_parent_request_doc_id
                .map(|parent| (parent, Some(row.runtime_source_request_id.clone())));
        }
    }
    Ok(lineage)
}

/// The Node document's native owner, as the runtime reading its own Node
/// observes it.
async fn node_owner(node: &Arc<EmbeddedNode>, node_did: &str) -> Result<Option<String>> {
    let runtime = identity::Did::new(node_did)?;
    ConfigAccess::transact_local_readonly(node, Some(runtime), "mailbox.node_owner", |txn| {
        Box::pin(async move {
            let Some((doc_id, _)) = gents_documents::read_desired_state_record_in_txn(
                txn,
                gents_store::collection::Collection::Node,
                node_did,
                node_did,
            )
            .await?
            else {
                return Ok::<_, anyhow::Error>(None);
            };
            let owners = txn
                .document_owners("Node", std::slice::from_ref(&doc_id))
                .await?;
            Ok(owners
                .into_iter()
                .find(|observed| observed.doc_id == doc_id && observed.visible)
                .and_then(|observed| observed.owner)
                .map(|owner| owner.as_str().to_owned()))
        })
    })
    .await
}

#[cfg(test)]
mod conformance {
    use super::{filing_recipient, Admission, Hop};

    #[test]
    fn filing_recipient_matches_lean() {
        gents_lean_vocab::families::mailbox_access_filing::replay::<Admission>(|input| {
            let lineage: Vec<Hop> = input
                .lineage
                .iter()
                .map(|hop| Hop {
                    admission: hop.admission,
                    signer: hop.signer.clone(),
                })
                .collect();
            filing_recipient(
                input.claimed_signer.as_deref(),
                &lineage,
                input.node_owner.as_deref(),
            )
        });
    }
}
