use std::sync::Arc;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::claim::Successor;
use super::effect::{CoreOp, Effect};
use super::registry::{CoreOperation, Registry};
use gents_store::config_client::ConfigApplyTxn;

/// Lean `Hooks.supersedeAll`.
pub const SUPERSEDE_PENDING: &str = "request.pending.supersede";

/// Lean `Hooks.StaleRequest`: a pending request to supersede, the request that
/// supersedes it and the reason core records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleRequest {
    pub doc_id: String,
    pub request_id: String,
    pub successor: String,
    pub successor_doc: String,
    pub reason: String,
}

#[derive(Serialize, Deserialize)]
struct Supersede {
    node_did: String,
    at: String,
    stale: Vec<StaleRequest>,
}

/// The effect asking core to supersede `stale` in order. Each entry counts
/// toward the requesting plugin's bound.
pub fn supersede_pending(node_did: &str, at: &str, stale: Vec<StaleRequest>) -> Effect {
    Effect::Core(CoreOp::new(
        SUPERSEDE_PENDING,
        serde_json::json!(Supersede {
            node_did: node_did.to_owned(),
            at: at.to_owned(),
            stale,
        }),
    ))
}

/// Supersedes each named pending request in order through the claim gate's
/// pending-supersede path. A request no longer pending is left as it is.
struct SupersedePending;

#[async_trait::async_trait]
impl CoreOperation for SupersedePending {
    fn name(&self) -> &'static str {
        SUPERSEDE_PENDING
    }

    async fn perform(&self, txn: &ConfigApplyTxn<'_>, args: &Value) -> anyhow::Result<()> {
        let args =
            Supersede::deserialize(args).context("decoding the pending supersede request")?;
        for stale in &args.stale {
            crate::request::lifecycle::supersede_pending_in_txn(
                txn,
                &args.node_did,
                &stale.doc_id,
                &stale.request_id,
                &Successor {
                    request_id: stale.successor.clone(),
                    request_doc_id: stale.successor_doc.clone(),
                },
                &stale.reason,
                &args.at,
            )
            .await?;
        }
        Ok(())
    }

    fn extent(&self, args: &Value) -> usize {
        args.get("stale").and_then(Value::as_array).map_or(1, Vec::len)
    }
}

/// Core's own operations a plugin may request.
pub fn register(registry: &mut Registry) {
    registry.operations.push(Arc::new(SupersedePending));
}
