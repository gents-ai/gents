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

/// Lean `Identity.MailboxAccess.lineageRoot`: whether a trigger started the
/// run, read past its continuations.
fn started_by_trigger(lineage: &[Admission]) -> bool {
    lineage
        .iter()
        .find(|admission| **admission != Admission::LocalControl)
        == Some(&Admission::AutomatedTrigger)
}

/// Lean `Identity.MailboxAccess.filingRecipient`: the filing request's
/// admitted signer, else the Node's owner for a run a trigger started, else no
/// one. `lineage` holds the filing request's admission, then its parents'.
pub(crate) fn filing_recipient(
    claimed_signer: Option<&str>,
    lineage: &[Admission],
    node_owner: Option<&str>,
) -> Option<String> {
    match claimed_signer {
        Some(signer) => Some(signer.to_owned()),
        None if started_by_trigger(lineage) => node_owner.map(str::to_owned),
        None => None,
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
    let owner = match started_by_trigger(&lineage) {
        true => node_owner(node, &request.node_did).await?,
        false => None,
    };
    Ok(filing_recipient(None, &lineage, owner.as_deref()))
}

async fn lineage(access: &ConfigAccess, request_doc_id: &str) -> Result<Vec<Admission>> {
    use gents_protocol::document_query::{
        fields::agent_request as fields, Filter, Query, Selection,
    };
    let mut lineage = Vec::new();
    let mut seen = rapidhash::RapidHashSet::default();
    let mut next = Some(request_doc_id.to_owned());
    while let Some(doc_id) = next.take() {
        if !seen.insert(doc_id.clone()) {
            break;
        }
        let query = Query::select(
            Selection::doc_id()
                .field(fields::REQUEST_ID)
                .field(fields::ADMISSION_KIND)
                .field(fields::RUNTIME_SOURCE_KIND)
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
        let admission = Admission::of(&row);
        lineage.push(admission);
        if admission == Admission::LocalControl {
            next = row.caused_by_parent_request_doc_id;
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
    use super::{filing_recipient, Admission};

    #[test]
    fn filing_recipient_matches_lean() {
        gents_lean_vocab::families::mailbox_access_filing::replay::<Admission>(|input| {
            filing_recipient(
                input.claimed_signer.as_deref(),
                &input.lineage,
                input.node_owner.as_deref(),
            )
        });
    }
}
