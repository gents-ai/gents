use std::sync::Arc;

use anyhow::Context;
use serde::Deserialize;
use serde_json::Value;

use super::claim::Successor;
use super::effect::{CoreOp, Effect};
use super::registry::{CoreOperation, OperationContext, Registry};
use super::supersede::{Ancestry, ParentFacts, PendingFacts, Selector};
use gents_protocol::document_query::{fields::agent_request as fields, Filter, Query, Selection};
use gents_protocol::request_admission::SIGNED_REQUEST_FIELDS;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigApplyTxn;

/// Lean `Hooks.supersedeMatching`.
pub const SUPERSEDE_PENDING: &str = "request.pending.supersede";

/// The effect asking core to supersede every pending Goal continuation of the
/// session that `selector` does not keep.
pub fn supersede_pending(selector: &Selector) -> Effect {
    Effect::Core(CoreOp::new(SUPERSEDE_PENDING, serde_json::json!(selector)))
}

/// Reads the session's pending Goal continuations at the snapshot and the
/// parent of each one the selector does not keep, and supersedes them in row
/// order through the claim gate's pending-supersede path, each with the reason
/// its ancestry classification names (Lean `Hooks.supersedeMatching`). A row
/// no longer pending when its turn comes is left as it is.
struct SupersedePending;

async fn session_rows(
    txn: &ConfigApplyTxn<'_>,
    filter: Filter<fields::Doc>,
) -> anyhow::Result<Vec<AgentRequestRow>> {
    let rendered = Query::select(Selection::owned(SIGNED_REQUEST_FIELDS))
        .filter(filter)
        .render();
    let response = txn.run_local_response(&rendered).await?;
    Ok(gents_store::graphql::rows(&response, "AgentRequest")?)
}

fn documented(row: &AgentRequestRow) -> anyhow::Result<&str> {
    row.doc_id
        .as_deref()
        .context("pending Goal continuation has no document ID")
}

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
        let rows = session_rows(
            txn,
            Filter::eq(fields::NODE_DID, &selector.node_did)
                .and(Filter::eq(fields::SESSION_ID, &selector.session_id))
                .and(Filter::eq(
                    fields::LIFECYCLE_STATE,
                    RequestLifecycleState::Pending.as_str(),
                )),
        )
        .await?;
        let facts = rows
            .iter()
            .map(|row| Ok(PendingFacts::of(row, documented(row)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut parents: Vec<ParentFacts> = Vec::new();
        for parent in facts
            .iter()
            .filter(|row| selector.matches(row))
            .filter_map(|row| row.parent.as_deref())
        {
            if parents.iter().any(|read| read.doc == parent) {
                continue;
            }
            if let [row] = session_rows(txn, Filter::eq(fields::DOC_ID, parent))
                .await?
                .as_slice()
            {
                parents.push(ParentFacts::of(
                    row,
                    parent,
                    &selector.node_did,
                    &selector.session_id,
                ));
            }
        }
        let successor = Successor {
            request_id: selector.successor.clone(),
            request_doc_id: selector.successor_doc.clone(),
        };
        for retirement in selector.stale(&facts, &parents) {
            if let Ancestry::Broken(link) = retirement.ancestry {
                tracing::warn!(
                    request_id = %retirement.request,
                    request_doc_id = %retirement.doc,
                    goal_id = ?facts
                        .iter()
                        .find(|row| row.doc == retirement.doc)
                        .and_then(|row| row.trigger.as_deref()),
                    assignment_goal_id = %selector.goal_id,
                    link = link.describe(),
                    "superseding a pending Goal continuation whose Goal ancestry is broken"
                );
            }
            crate::request::lifecycle::supersede_pending_in_txn(
                txn,
                &selector.node_did,
                &retirement.doc,
                &retirement.request,
                &successor,
                &retirement.reason,
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
