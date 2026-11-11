use std::sync::Arc;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::claim::Successor;
use super::effect::{CoreOp, Effect};
use super::registry::{CoreOperation, OperationContext, Registry};
use gents_protocol::goal_names::GOAL_TRIGGER_KIND;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigApplyTxn;

/// Lean `Hooks.supersedeMatching`.
pub const SUPERSEDE_PENDING: &str = "request.pending.supersede";

/// Lean `Hooks.Selector`: the Goal assignment whose members a session keeps,
/// and the successor and reason core records on every other pending Goal
/// continuation of the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selector {
    pub node_did: String,
    pub session_id: String,
    pub goal_id: String,
    pub root_doc_id: String,
    pub successor: String,
    pub successor_doc: String,
    pub reason: String,
    pub at: String,
}

impl Selector {
    /// Lean `Hooks.Selector.member`, from the row's own signed fields.
    fn member(&self, row: &AgentRequestRow) -> bool {
        let doc = row.doc_id.as_deref();
        doc == Some(self.root_doc_id.as_str())
            || (row.caused_by_trigger_id.as_deref() == Some(self.goal_id.as_str())
                && row
                    .input
                    .as_ref()
                    .and_then(|input| input.goal_assignment.as_ref())
                    .is_some_and(|reference| {
                        reference.goal_id == self.goal_id
                            && reference.root_request_doc_id.as_deref().or(doc)
                                == Some(self.root_doc_id.as_str())
                    }))
    }

    /// Lean `Hooks.Selector.matches`.
    fn matches(&self, row: &AgentRequestRow) -> bool {
        row.lifecycle_state == Some(RequestLifecycleState::Pending)
            && row.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND)
            && !self.member(row)
    }
}

/// The effect asking core to supersede every pending Goal continuation of the
/// session that `selector` does not keep.
pub fn supersede_pending(selector: &Selector) -> Effect {
    Effect::Core(CoreOp::new(SUPERSEDE_PENDING, serde_json::json!(selector)))
}

/// Reads the session's pending Goal continuations at the snapshot and
/// supersedes, in row order, each one the selector does not keep, through the
/// claim gate's pending-supersede path. A row no longer pending when its turn
/// comes is left as it is.
struct SupersedePending;

#[async_trait::async_trait]
impl CoreOperation for SupersedePending {
    fn name(&self) -> &'static str {
        SUPERSEDE_PENDING
    }

    async fn perform(
        &self,
        txn: &ConfigApplyTxn<'_>,
        _context: OperationContext<'_>,
        args: &Value,
    ) -> anyhow::Result<()> {
        let selector =
            Selector::deserialize(args).context("decoding the pending supersede selector")?;
        let rendered = {
            use gents_protocol::document_query::{
                fields::agent_request as fields, Filter, Query, Selection,
            };
            Query::select(Selection::owned(
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
            ))
            .filter(Filter::eq(fields::NODE_DID, &selector.node_did))
            .filter(Filter::eq(fields::SESSION_ID, &selector.session_id))
            .filter(Filter::eq(
                fields::LIFECYCLE_STATE,
                RequestLifecycleState::Pending.as_str(),
            ))
            .render()
        };
        let response = txn.run_local_response(&rendered).await?;
        let rows: Vec<AgentRequestRow> = gents_store::graphql::rows(&response, "AgentRequest")?;
        let successor = Successor {
            request_id: selector.successor.clone(),
            request_doc_id: selector.successor_doc.clone(),
        };
        for row in rows.iter().filter(|row| selector.matches(row)) {
            let doc = row
                .doc_id
                .as_deref()
                .context("pending Goal continuation has no document ID")?;
            crate::request::lifecycle::supersede_pending_in_txn(
                txn,
                &selector.node_did,
                doc,
                &row.request_id,
                &successor,
                &selector.reason,
                &selector.at,
            )
            .await?;
        }
        Ok(())
    }
}

/// Core's own operations a plugin may request.
pub fn register(registry: &mut Registry) {
    registry.operations.push(Arc::new(SupersedePending));
}
