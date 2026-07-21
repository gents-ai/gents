//! DID-parameterized self-configuration core (#654).
//!
//! Transport-agnostic operations behind the `get_my_config` / `configure_*`
//! tools: every write is a transactional read-modify-write on one owned
//! document, merged through the Lean-fenced patch layer
//! (`config_client::patch`), validated wholesale, and executed under the
//! agent DID so DefraDB ACP is the authorization boundary. A future MCP
//! surface wraps this same core with a DID from the incoming call.

use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use identity::Did;
use serde_json::{Map, Value};

use crate::config_client::patch::{
    apply_patch, create_doc_in_txn, diff_docs, ensure_admissible, read_doc_in_txn,
    update_doc_fields_in_txn, FieldDelta, SelfConfigPatch, SelfConfigTarget,
};
use crate::config_client::ConfigApplyTxn;
use crate::document_config::ToolSelectionDocument;

/// How a self-config write lands: config documents are watched by the control
/// reconciler; a committed patch applies at the next generation swap, not to
/// the in-flight turn. Surfaced in tool descriptions and result payloads.
pub const EFFECT_TIMING_NOTE: &str = "Committed changes are picked up by the runtime reconciler \
     (typically within a few seconds) and apply to requests dispatched after \
     the resulting generation swap; the current turn keeps its existing \
     configuration.";

/// Self-configuration executor for one behavior of one agent.
#[derive(Clone)]
pub struct SelfConfigCore {
    node: Arc<EmbeddedNode>,
    agent_did: String,
    behavior_id: String,
    no_lockout: bool,
}

/// Outcome of an applied (or previewed) patch.
#[derive(Debug, serde::Serialize)]
pub struct PatchOutcome {
    pub collection: &'static str,
    pub doc_id: Option<String>,
    pub created: bool,
    pub committed: bool,
    pub changed: Vec<FieldDelta>,
    pub effect: &'static str,
}

/// Behavior anchor loaded fresh per call, so a prior `configure_behavior`
/// re-pointing `tool_selection_id`/`inference_profile_id`/`backend_id` is
/// honored by the next call.
pub(crate) struct BehaviorAnchor {
    pub(crate) doc: Map<String, Value>,
}

impl BehaviorAnchor {
    pub(crate) fn ref_id(&self, field: &str) -> Option<String> {
        self.doc
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }
}

impl SelfConfigCore {
    pub fn new(node: Arc<EmbeddedNode>, agent_did: String, behavior_id: String) -> Result<Self> {
        if agent_did.trim().is_empty() {
            bail!("self-config requires a non-empty agent DID (fail closed)");
        }
        if behavior_id.trim().is_empty() {
            bail!("self-config requires a non-empty behavior id (fail closed)");
        }
        Ok(Self {
            node,
            agent_did,
            behavior_id,
            no_lockout: false,
        })
    }

    pub fn with_no_lockout(mut self, no_lockout: bool) -> Self {
        self.no_lockout = no_lockout;
        self
    }

    pub fn agent_did(&self) -> &str {
        &self.agent_did
    }

    pub fn behavior_id(&self) -> &str {
        &self.behavior_id
    }

    pub(crate) fn identity(&self) -> Result<Did> {
        Did::new(self.agent_did.clone())
            .map_err(|error| anyhow!("agent DID is not ACP-addressable: {error}"))
    }

    /// Begin an identity-scoped transaction.
    pub(crate) async fn begin_txn(&self) -> Result<ConfigApplyTxn<'_>> {
        ConfigApplyTxn::begin_local(&self.node, Some(self.identity()?)).await
    }

    /// Load and ownership-check the behavior anchor inside the transaction.
    pub(crate) async fn load_behavior_anchor(
        &self,
        txn: &ConfigApplyTxn<'_>,
    ) -> Result<BehaviorAnchor> {
        let Some((_doc_id, doc)) =
            read_doc_in_txn(txn, SelfConfigTarget::AgentBehavior, &self.behavior_id).await?
        else {
            bail!(
                "behavior {} not found; self-config is anchored on the running behavior document",
                self.behavior_id
            );
        };
        let owner = doc.get("agent_did").and_then(Value::as_str).unwrap_or("");
        if owner != self.agent_did {
            bail!(
                "behavior {} is owned by {owner:?}, not this agent — self-config is self only",
                self.behavior_id
            );
        }
        Ok(BehaviorAnchor { doc })
    }

    /// The write operation: load owned doc → merge patch → validate → write
    /// exactly the patched fields → commit; abort wholesale on any failure.
    ///
    /// `resolve_unique` maps the behavior anchor to the target document's
    /// unique value (e.g. `tool_selection_id` for the tools category).
    /// `allow_create` permits upsert-create (automation only); `on_create`
    /// injects identity/link fields the patch surface deliberately excludes.
    pub(crate) async fn apply(&self, request: ApplyRequest<'_>) -> Result<PatchOutcome> {
        ensure_admissible(request.target, &request.patch)?;

        let txn = self.begin_txn().await?;
        let outcome = self.apply_in_txn(&txn, &request).await;
        match outcome {
            Ok(outcome) => {
                txn.commit().await.with_context(|| {
                    format!(
                        "committing {} self-config patch",
                        request.target.collection_name()
                    )
                })?;
                Ok(PatchOutcome {
                    committed: true,
                    ..outcome
                })
            }
            Err(error) => {
                if let Err(discard_error) = txn.discard().await {
                    tracing::warn!(
                        collection = request.target.collection_name(),
                        %discard_error,
                        "self-config transaction discard reported an error; \
                         atomicity still guarantees no partial write"
                    );
                }
                Err(error)
            }
        }
    }

    async fn apply_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        request: &ApplyRequest<'_>,
    ) -> Result<PatchOutcome> {
        let anchor = self.load_behavior_anchor(txn).await?;
        let unique_value = (request.resolve_unique)(&anchor)?;

        let stored = read_doc_in_txn(txn, request.target, &unique_value).await?;
        let (doc_id, stored_doc, creating) = match stored {
            Some((doc_id, doc)) => (Some(doc_id), doc, false),
            None if request.allow_create => (None, Map::new(), true),
            None => bail!(
                "{} {unique_value:?} not found",
                request.target.collection_name()
            ),
        };

        let mut merged = apply_patch(request.target, &stored_doc, &request.patch);
        if creating {
            (request.on_create)(&unique_value, &mut merged)?;
        }

        (request.validate)(txn, &anchor, &stored_doc, &merged).await?;

        if self.no_lockout {
            (request.guard)(&anchor, &merged)?;
        }

        let changed = diff_docs(request.target, &stored_doc, &merged);
        let doc_id = match (&doc_id, creating) {
            (Some(doc_id), _) => {
                update_doc_fields_in_txn(txn, request.target, doc_id, &request.patch, &merged)
                    .await?
            }
            (None, _) => create_doc_in_txn(txn, request.target, &merged).await?,
        };

        Ok(PatchOutcome {
            collection: request.target.collection_name(),
            doc_id: Some(doc_id),
            created: creating,
            committed: false,
            changed,
            effect: EFFECT_TIMING_NOTE,
        })
    }

    /// Dry-run preview: merge + validate in memory, return the diff. Nothing
    /// is written; the transaction is read-only and always discarded.
    pub(crate) async fn preview(&self, request: ApplyRequest<'_>) -> Result<PatchOutcome> {
        ensure_admissible(request.target, &request.patch)?;
        let txn = self.begin_txn().await?;
        let result = self.preview_in_txn(&txn, &request).await;
        let _ = txn.discard().await;
        result
    }

    async fn preview_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        request: &ApplyRequest<'_>,
    ) -> Result<PatchOutcome> {
        let anchor = self.load_behavior_anchor(txn).await?;
        let unique_value = (request.resolve_unique)(&anchor)?;
        let stored = read_doc_in_txn(txn, request.target, &unique_value).await?;
        let (stored_doc, creating) = match stored {
            Some((_, doc)) => (doc, false),
            None if request.allow_create => (Map::new(), true),
            None => bail!(
                "{} {unique_value:?} not found",
                request.target.collection_name()
            ),
        };
        let mut merged = apply_patch(request.target, &stored_doc, &request.patch);
        if creating {
            (request.on_create)(&unique_value, &mut merged)?;
        }
        (request.validate)(txn, &anchor, &stored_doc, &merged).await?;
        if self.no_lockout {
            (request.guard)(&anchor, &merged)?;
        }
        Ok(PatchOutcome {
            collection: request.target.collection_name(),
            doc_id: None,
            created: creating,
            committed: false,
            changed: diff_docs(request.target, &stored_doc, &merged),
            effect: "dry-run: nothing was written",
        })
    }
}

/// Per-call plumbing for one category patch. Boxed closures keep the core's
/// write operation single-sourced while each tool supplies target resolution,
/// validation, creation defaults, and its slice of the no-lockout guard.
pub(crate) struct ApplyRequest<'a> {
    pub(crate) target: SelfConfigTarget,
    pub(crate) patch: SelfConfigPatch,
    pub(crate) allow_create: bool,
    pub(crate) resolve_unique: Box<dyn Fn(&BehaviorAnchor) -> Result<String> + Send + Sync + 'a>,
    pub(crate) on_create:
        Box<dyn Fn(&str, &mut Map<String, Value>) -> Result<()> + Send + Sync + 'a>,
    pub(crate) validate: ValidateFn<'a>,
    pub(crate) guard:
        Box<dyn Fn(&BehaviorAnchor, &Map<String, Value>) -> Result<()> + Send + Sync + 'a>,
}

pub(crate) type ValidateFn<'a> = Box<
    dyn for<'b> Fn(
            &'b ConfigApplyTxn<'b>,
            &'b BehaviorAnchor,
            &'b Map<String, Value>,
            &'b Map<String, Value>,
        ) -> futures::future::BoxFuture<'b, Result<()>>
        + Send
        + Sync
        + 'a,
>;

impl<'a> ApplyRequest<'a> {
    pub(crate) fn new(target: SelfConfigTarget, patch: SelfConfigPatch) -> Self {
        Self {
            target,
            patch,
            allow_create: false,
            resolve_unique: Box::new(|_| bail!("resolve_unique not set (internal bug)")),
            on_create: Box::new(|_, _| Ok(())),
            validate: Box::new(|_, _, _, _| Box::pin(async { Ok(()) })),
            guard: Box::new(|_, _| Ok(())),
        }
    }
}

/// Decode a merged document projection into a typed document for structural
/// validation; the error names the offending field/type for the model.
pub(crate) fn decode_merged<T: serde::de::DeserializeOwned>(
    collection: &str,
    merged: &Map<String, Value>,
) -> Result<T> {
    serde_json::from_value(Value::Object(merged.clone()))
        .map_err(|error| anyhow!("merged {collection} document is not valid: {error}"))
}

/// Shared no-lockout slice for ToolSelection patches: the merged selection
/// must keep the self-config gate on (Lean `gateOn`).
pub(crate) fn guard_selection_keeps_gate(merged: &Map<String, Value>) -> Result<()> {
    let gate_on = merged
        .get("enable_self_config")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !gate_on {
        bail!(
            "no-lockout guard: this patch would disable enable_self_config and strip \
             the agent's own reconfigure ability; ask the operator to lift the guard \
             (self_config_no_lockout) if this is intended"
        );
    }
    Ok(())
}

/// Structural + reference validation for a merged ToolSelection.
pub(crate) fn validate_merged_selection(merged: &Map<String, Value>) -> Result<()> {
    let selection: ToolSelectionDocument = decode_merged("ToolSelection", merged)?;
    selection.validate()
}
