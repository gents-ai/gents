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
    apply_patch, diff_docs, ensure_admissible, FieldDelta, SelfConfigPatch, SelfConfigTarget,
};
use crate::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, validate_desired_state_plan,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::document_config::Tools;

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
/// re-pointing `context_id`/`inference_profile_id` is
/// honored by the next call.
pub(crate) struct BehaviorAnchor {
    pub(crate) doc: Map<String, Value>,
    pub(crate) context: Map<String, Value>,
    pub(crate) profile: Map<String, Value>,
    pub(crate) execution: Map<String, Value>,
}

impl BehaviorAnchor {
    pub(crate) fn ref_id(&self, field: &str) -> Option<String> {
        self.doc
            .get(field)
            .or_else(|| self.context.get(field))
            .or_else(|| self.profile.get(field))
            .or_else(|| self.execution.get(field))
            .and_then(Value::as_str)
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

    pub(crate) fn node(&self) -> &EmbeddedNode {
        &self.node
    }

    pub(crate) fn identity(&self) -> Result<Did> {
        Did::new(self.agent_did.clone())
            .map_err(|error| anyhow!("agent DID is not ACP-addressable: {error}"))
    }

    /// Load and ownership-check the behavior anchor inside the transaction.
    pub(crate) async fn load_behavior_anchor(
        &self,
        txn: &ConfigApplyTxn<'_>,
    ) -> Result<BehaviorAnchor> {
        let Some((_doc_id, doc)) = read_owned_doc(
            txn,
            SelfConfigTarget::AgentBehavior,
            &self.agent_did,
            &self.behavior_id,
        )
        .await?
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
        let context_id = doc
            .get("context_id")
            .and_then(Value::as_str)
            .context("behavior context is missing")?;
        let profile_id = doc
            .get("inference_profile_id")
            .and_then(Value::as_str)
            .context("behavior inference profile is missing")?;
        let context = read_owned_doc(
            txn,
            SelfConfigTarget::AgentContext,
            &self.agent_did,
            context_id,
        )
        .await?
        .context("context not found")?
        .1;
        let profile = read_owned_doc(
            txn,
            SelfConfigTarget::InferenceProfile,
            &self.agent_did,
            profile_id,
        )
        .await?
        .context("profile not found")?
        .1;
        let execution = match profile.get("execution_id").and_then(Value::as_str) {
            Some(id) => {
                read_owned_doc(
                    txn,
                    SelfConfigTarget::InferenceExecution,
                    &self.agent_did,
                    id,
                )
                .await?
                .context("execution not found")?
                .1
            }
            None => Map::new(),
        };
        Ok(BehaviorAnchor {
            doc,
            context,
            profile,
            execution,
        })
    }

    /// The write operation: load owned doc → merge patch → validate → publish
    /// the canonical candidate through the common desired-state owner → commit; abort wholesale on any failure.
    ///
    /// `resolve_unique` maps the behavior anchor to the target document's
    /// unique value (e.g. `tools_id` for the tools category).
    /// `allow_create` permits upsert-create (automation only); `on_create`
    /// injects identity/link fields the patch surface deliberately excludes.
    pub(crate) async fn apply(&self, request: ApplyRequest<'_>) -> Result<PatchOutcome> {
        ensure_admissible(request.target, &request.patch)?;

        let identity = self.identity()?;
        let this = self;
        let request = &request;
        let outcome = ConfigAccess::transact_local(
            &self.node,
            Some(identity),
            "self_config.apply",
            move |txn| Box::pin(async move { this.apply_in_txn(txn, request).await }),
        )
        .await
        .with_context(|| {
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

    async fn apply_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        request: &ApplyRequest<'_>,
    ) -> Result<PatchOutcome> {
        let anchor = self.load_behavior_anchor(txn).await?;
        let unique_value = (request.resolve_unique)(&anchor)?;

        let stored = read_owned_doc(txn, request.target, &self.agent_did, &unique_value).await?;
        let (_doc_id, stored_doc, creating) = match stored {
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
            self.guard_candidate_chain(txn, request.target, &merged)
                .await?;
        }

        let changed = safe_diff(request.target, &stored_doc, &merged);
        let plan = replacement_plan(request.target, &merged)?;
        apply_desired_state_plan(txn, &plan).await?;
        let doc_id = read_owned_doc(txn, request.target, &self.agent_did, &unique_value)
            .await?
            .context("published config is missing")?
            .0;

        Ok(PatchOutcome {
            collection: request.target.collection_name(),
            doc_id: Some(doc_id),
            created: creating,
            committed: false,
            changed,
            effect: EFFECT_TIMING_NOTE,
        })
    }

    async fn guard_candidate_chain(
        &self,
        txn: &ConfigApplyTxn<'_>,
        target: SelfConfigTarget,
        merged: &Map<String, Value>,
    ) -> Result<()> {
        let behavior = candidate_doc(
            txn,
            self.agent_did(),
            SelfConfigTarget::AgentBehavior,
            self.behavior_id(),
            target,
            merged,
        )
        .await?;
        anyhow::ensure!(
            behavior.get("enabled").and_then(Value::as_bool) != Some(false),
            "no-lockout: behavior disabled"
        );
        let context_id = behavior
            .get("context_id")
            .and_then(Value::as_str)
            .context("no-lockout: context missing")?;
        let context = candidate_doc(
            txn,
            self.agent_did(),
            SelfConfigTarget::AgentContext,
            context_id,
            target,
            merged,
        )
        .await?;
        let tools_id = context
            .get("tools_id")
            .and_then(Value::as_str)
            .context("no-lockout: tools missing")?;
        let tools = candidate_doc(
            txn,
            self.agent_did(),
            SelfConfigTarget::Tools,
            tools_id,
            target,
            merged,
        )
        .await?;
        guard_selection_keeps_gate(&tools)?;
        let profile_id = behavior
            .get("inference_profile_id")
            .and_then(Value::as_str)
            .context("no-lockout: profile missing")?;
        let profile = candidate_doc(
            txn,
            self.agent_did(),
            SelfConfigTarget::InferenceProfile,
            profile_id,
            target,
            merged,
        )
        .await?;
        let backend_id = profile
            .get("backend_id")
            .and_then(Value::as_str)
            .context("no-lockout: backend missing")?;
        let backend = candidate_doc(
            txn,
            self.agent_did(),
            SelfConfigTarget::InferenceBackend,
            backend_id,
            target,
            merged,
        )
        .await?;
        anyhow::ensure!(
            backend.get("enabled").and_then(Value::as_bool) != Some(false),
            "no-lockout: backend disabled"
        );
        Ok(())
    }

    /// Dry-run preview: merge + validate in memory, return the diff. Nothing
    /// is written; the consistent-snapshot transaction remains read-only.
    pub(crate) async fn preview(&self, request: ApplyRequest<'_>) -> Result<PatchOutcome> {
        ensure_admissible(request.target, &request.patch)?;
        let identity = self.identity()?;
        let this = self;
        let request = &request;
        ConfigAccess::transact_local(
            &self.node,
            Some(identity),
            "self_config.preview",
            move |txn| Box::pin(async move { this.preview_in_txn(txn, request).await }),
        )
        .await
    }

    async fn preview_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        request: &ApplyRequest<'_>,
    ) -> Result<PatchOutcome> {
        let anchor = self.load_behavior_anchor(txn).await?;
        let unique_value = (request.resolve_unique)(&anchor)?;
        let stored = read_owned_doc(txn, request.target, &self.agent_did, &unique_value).await?;
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
            self.guard_candidate_chain(txn, request.target, &merged)
                .await?;
        }
        validate_desired_state_plan(txn, &replacement_plan(request.target, &merged)?).await?;
        Ok(PatchOutcome {
            collection: request.target.collection_name(),
            doc_id: None,
            created: creating,
            committed: false,
            changed: safe_diff(request.target, &stored_doc, &merged),
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

pub(crate) fn guard_selection_keeps_gate(merged: &Map<String, Value>) -> Result<()> {
    let tools: Tools = decode_merged("Tools", merged)?;
    anyhow::ensure!(
        tools
            .self_config
            .as_ref()
            .and_then(|config| config.enable_self_config)
            .unwrap_or(false),
        "no-lockout guard: self-config must remain enabled"
    );
    Ok(())
}
pub(crate) fn validate_merged_selection(merged: &Map<String, Value>) -> Result<()> {
    let tools = decode_merged::<Tools>("Tools", merged)?;
    if let Some(lsp) = tools
        .integrations
        .as_ref()
        .and_then(|group| group.lsp.as_ref())
    {
        crate::toolset::lsp::LspConfigDocument::parse_self_config(lsp.config.as_deref())
            .map_err(anyhow::Error::msg)?;
    }
    tools.validate()
}
pub(crate) async fn read_owned_doc(
    txn: &ConfigApplyTxn<'_>,
    target: SelfConfigTarget,
    owner: &str,
    id: &str,
) -> Result<Option<(String, Map<String, Value>)>> {
    read_desired_state_record_in_txn(txn, target.collection(), owner, id)
        .await?
        .map(|(id, value)| {
            let doc = value.as_object().context("config object required")?.clone();
            Ok((id, doc))
        })
        .transpose()
}
fn replacement_plan(
    target: SelfConfigTarget,
    merged: &Map<String, Value>,
) -> Result<DesiredStateApplyPlan> {
    let value = Value::Object(merged.clone());
    DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: target.collection(),
        add: value.clone(),
        update: value,
    }])
}

async fn candidate_doc(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    wanted: SelfConfigTarget,
    id: &str,
    target: SelfConfigTarget,
    merged: &Map<String, Value>,
) -> Result<Map<String, Value>> {
    if wanted == target && merged.get(target.unique_field()).and_then(Value::as_str) == Some(id) {
        return Ok(merged.clone());
    }
    Ok(read_owned_doc(txn, wanted, owner, id)
        .await?
        .context("candidate chain reference missing")?
        .1)
}
fn safe_diff(
    target: SelfConfigTarget,
    before: &Map<String, Value>,
    after: &Map<String, Value>,
) -> Vec<FieldDelta> {
    let mut deltas = diff_docs(target, before, after);
    if target == SelfConfigTarget::InferenceBackend {
        for delta in &mut deltas {
            if delta.field == "auth" {
                for value in [&mut delta.from, &mut delta.to] {
                    if let Some(auth) = value.as_object_mut() {
                        if auth.contains_key("key") {
                            auth.insert("key".into(), Value::String("[redacted]".into()));
                        }
                    }
                }
            }
        }
    }
    deltas
}
