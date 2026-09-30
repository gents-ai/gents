//! DID-parameterized self-configuration core (#654).
//!
//! Transport-agnostic operations behind the model-facing `config` command
//! tools: every write is a transactional read-modify-write on one owned
//! document, merged through the Lean-fenced patch layer
//! (`config_client::patch`), validated wholesale, and executed under the
//! agent DID so DefraDB ACP is the authorization boundary. A future MCP
//! surface wraps this same core with a DID from the incoming call.

use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use identity::Did;
use serde_json::{json, Map, Value};

use crate::config_client::patch::{
    apply_patch, diff_docs, ensure_admissible, FieldDelta, SelfConfigPatch, SelfConfigTarget,
};
use crate::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, validate_desired_state_plan,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::document_config::{BackendAuth, Tools};
use crate::tool_surface::SelfConfigProcessCeiling;
use crate::toolset::CommandNetworkMode;

#[derive(Debug, thiserror::Error)]
#[error("no owned {} with ID {id:?}. This is a document ID, not a behavior name. List this resource to find its exact IDs; behavior get shows a role's selected Context, Tools and profile", target.collection_name())]
pub(super) struct MissingConfigDocument {
    pub target: SelfConfigTarget,
    pub id: String,
}

#[derive(Debug, thiserror::Error)]
pub(super) struct MissingBehavior {
    pub behavior_id: String,
    /// Behavior IDs whose slug or display name equals the requested ID
    /// ignoring case. Never resolved implicitly: display names are mutable
    /// and not unique.
    pub suggestions: Vec<String>,
}

impl std::fmt::Display for MissingBehavior {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown behavior_id {:?}; ", self.behavior_id)?;
        match self.suggestions.as_slice() {
            [] => write!(
                f,
                "copy an exact ID from [\"behavior\",\"list\"] (behavior create returns \"<DID>:<slug>\")"
            ),
            [only] => write!(f, "did you mean {only:?}?"),
            several => write!(f, "did you mean one of {several:?}?"),
        }
    }
}

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
    lockout_behavior_id: String,
    no_lockout: bool,
    process_ceiling: SelfConfigProcessCeiling,
}

/// Outcome of an applied (or previewed) patch. Field order is the order the
/// model reads the receipt: outcome, what changed, what happens next, then
/// identifiers.
#[derive(Debug, serde::Serialize)]
pub struct PatchOutcome {
    pub committed: bool,
    pub collection: &'static str,
    /// The target document's logical ID.
    pub target_id: String,
    /// The behavior the command targeted. A command without an explicit
    /// behavior targets the invoking one, so the receipt names it rather than
    /// leaving that default implicit.
    pub behavior_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<Value>,
    pub created: bool,
    pub changed: Vec<FieldDelta>,
    pub effect: &'static str,
    pub doc_id: Option<String>,
}

pub(super) fn target_destination(document: &Map<String, Value>, owner: &str) -> Value {
    json!({
        "kind": if document.get("target_agent_did").and_then(Value::as_str) == Some(owner) { "local" } else { "remote" },
        "target_agent_did": document.get("target_agent_did"),
        "behavior_id": document.get("behavior_id"),
        "runtime_verified": false
    })
}

fn connection_view(
    target: SelfConfigTarget,
    anchor: &BehaviorAnchor,
    document: &Map<String, Value>,
    owner: &str,
) -> Option<Value> {
    match target {
        SelfConfigTarget::InferenceProfile => {
            let selected_id = anchor.ref_id("inference_profile_id");
            let profile_id = document.get("profile_id")?.as_str()?;
            let selected = selected_id.as_deref() == Some(profile_id);
            let mut view = json!({"behavior_id": anchor.doc.get("behavior_id"), "selected_profile_id": selected_id, "selected": selected});
            if !selected {
                view["select_with"] = json!({"argv":["behavior","update"],"target_id":anchor.doc.get("behavior_id"),"set":{"inference_profile_id":profile_id}});
            }
            Some(view)
        }
        SelfConfigTarget::SubagentTarget => Some(target_destination(document, owner)),
        _ => None,
    }
}

/// Behavior anchor loaded fresh per call, so a prior `config behavior` edit
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
            lockout_behavior_id: behavior_id.clone(),
            behavior_id,
            no_lockout: false,
            process_ceiling: SelfConfigProcessCeiling::default(),
        })
    }

    pub fn with_no_lockout(mut self, no_lockout: bool) -> Self {
        self.no_lockout = no_lockout;
        self
    }

    /// Preserve the invoking behavior as the recoverability anchor while a
    /// catalog-authorized command targets a sibling behavior. Candidate reads
    /// still incorporate a shared document being patched, so edits to shared
    /// inference configuration cannot indirectly lock out the invoker.
    pub(crate) fn with_lockout_behavior_id(mut self, behavior_id: String) -> Self {
        self.lockout_behavior_id = behavior_id;
        self
    }

    pub(crate) fn with_process_ceiling(
        mut self,
        process_ceiling: SelfConfigProcessCeiling,
    ) -> Self {
        self.process_ceiling = process_ceiling;
        self
    }

    pub(crate) fn process_ceiling(&self) -> &SelfConfigProcessCeiling {
        &self.process_ceiling
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
            return Err(MissingBehavior {
                behavior_id: self.behavior_id.clone(),
                suggestions: Vec::new(),
            }
            .into());
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
            Some(_) if request.require_create => bail!(
                "{} {unique_value:?} already exists; use update with its exact ID",
                request.target.collection_name()
            ),
            Some((doc_id, doc)) => (Some(doc_id), doc, false),
            None if request.allow_create => (None, Map::new(), true),
            None => {
                return Err(MissingConfigDocument {
                    target: request.target,
                    id: unique_value,
                }
                .into())
            }
        };

        let mut merged = apply_patch(request.target, &stored_doc, &request.patch);
        if creating {
            (request.on_create)(&unique_value, &mut merged)?;
        }

        (request.normalize)(txn, &anchor, &stored_doc, &mut merged).await?;
        (request.validate)(txn, &anchor, &stored_doc, &merged).await?;

        if self.no_lockout && request.guard_selected_chain {
            if self.lockout_behavior_id == self.behavior_id {
                (request.guard)(&anchor, &stored_doc, &merged)?;
            }
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
            behavior_id: self.behavior_id.clone(),
            connection: connection_view(request.target, &anchor, &merged, &self.agent_did),
            target_id: unique_value,
            doc_id: Some(doc_id),
            created: creating,
            committed: false,
            changed,
            effect: if request.target == SelfConfigTarget::DatastoreToolSurface {
                "Install its collection schemas before selecting this surface. Read tools get, then add its ID to set.datastore.datastore_tool_surface_ids, preserving existing selections. Tools apply after reconciliation to later requests."
            } else if creating && request.target == SelfConfigTarget::InferenceProfile {
                "Creating a profile does not select it for a behavior. To use it, call behavior update with set.inference_profile_id equal to this target_id. Selecting it preserves the previous profile and its settings. The selection applies to later requests after reconciliation."
            } else if request.target == SelfConfigTarget::Tools
                && request.patch.iter().any(|(field, _)| field == "subagents")
                && merged.get("subagents").is_some_and(|group| {
                    group.get("enabled").and_then(Value::as_bool) != Some(true)
                        && group
                            .get("target_ids")
                            .and_then(Value::as_array)
                            .is_some_and(|ids| !ids.is_empty())
                })
            {
                "Selected targets are inactive: subagents.enabled is not true. To enable delegation, call tools update with options.behavior set to this behavior_id and set.subagents containing enabled:true plus the existing target_ids. Tools apply after reconciliation to later requests."
            } else {
                EFFECT_TIMING_NOTE
            },
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
            &self.lockout_behavior_id,
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
            .context("no-lockout: Behavior.context_id is missing; preserve the current Context selection or select an existing Context")?;
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
            .with_context(|| format!("no-lockout: Context {context_id:?} has no tools_id; select existing Tools that preserve your config access"))?;
        let tools = candidate_doc(
            txn,
            self.agent_did(),
            SelfConfigTarget::Tools,
            tools_id,
            target,
            merged,
        )
        .await?;
        guard_tools_keep_control(&self.stored_lockout_tools(txn).await?, &tools)?;
        let profile_id = behavior
            .get("inference_profile_id")
            .and_then(Value::as_str)
            .context(
                "no-lockout: Behavior.inference_profile_id is missing; select an existing profile",
            )?;
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

    /// The invoker's Tools as committed, before this candidate.
    async fn stored_lockout_tools(&self, txn: &ConfigApplyTxn<'_>) -> Result<Map<String, Value>> {
        let owner = self.agent_did();
        let read = |target, id: String| async move {
            read_owned_doc(txn, target, owner, &id)
                .await?
                .map(|(_, doc)| doc)
                .context("no-lockout: stored reference chain is incomplete")
        };
        let behavior = read(
            SelfConfigTarget::AgentBehavior,
            self.lockout_behavior_id.clone(),
        )
        .await?;
        let field = |doc: &Map<String, Value>, name: &str| {
            doc.get(name)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .with_context(|| format!("no-lockout: stored {name} missing"))
        };
        let context = read(
            SelfConfigTarget::AgentContext,
            field(&behavior, "context_id")?,
        )
        .await?;
        read(SelfConfigTarget::Tools, field(&context, "tools_id")?).await
    }

    /// Preview: merge + validate in memory, return the diff. Nothing
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
            Some(_) if request.require_create => bail!(
                "{} {unique_value:?} already exists; use update with its exact ID",
                request.target.collection_name()
            ),
            Some((_, doc)) => (doc, false),
            None if request.allow_create => (Map::new(), true),
            None => {
                return Err(MissingConfigDocument {
                    target: request.target,
                    id: unique_value,
                }
                .into())
            }
        };
        let mut merged = apply_patch(request.target, &stored_doc, &request.patch);
        if creating {
            (request.on_create)(&unique_value, &mut merged)?;
        }
        (request.normalize)(txn, &anchor, &stored_doc, &mut merged).await?;
        (request.validate)(txn, &anchor, &stored_doc, &merged).await?;
        if self.no_lockout && request.guard_selected_chain {
            if self.lockout_behavior_id == self.behavior_id {
                (request.guard)(&anchor, &stored_doc, &merged)?;
            }
            self.guard_candidate_chain(txn, request.target, &merged)
                .await?;
        }
        validate_desired_state_plan(txn, &replacement_plan(request.target, &merged)?).await?;
        Ok(PatchOutcome {
            collection: request.target.collection_name(),
            behavior_id: self.behavior_id.clone(),
            changed: safe_diff(request.target, &stored_doc, &merged),
            connection: connection_view(request.target, &anchor, &merged, &self.agent_did),
            target_id: unique_value,
            doc_id: None,
            created: creating,
            committed: false,
            effect: "preview: nothing was written; send the same create/update call without preview to apply",
        })
    }
}

/// Focused convenience over canonical Tools groups, not a separate config type.
pub fn apply_tool_grant_selection(
    tools: &mut Tools,
    enable_lsp: Option<bool>,
    enable_graph_tools: Option<bool>,
    network_mode: Option<CommandNetworkMode>,
) {
    if let Some(enabled) = enable_lsp {
        let integrations = tools.integrations.get_or_insert_with(Default::default);
        if enabled {
            integrations.lsp.get_or_insert_with(Default::default);
        } else {
            integrations.lsp = None;
        }
    }
    if let Some(enabled) = enable_graph_tools {
        tools
            .built_ins
            .get_or_insert_with(Default::default)
            .enable_graph_tools = Some(enabled);
    }
    if let Some(network_mode) = network_mode {
        tools
            .host
            .get_or_insert_with(Default::default)
            .bash
            .get_or_insert_with(Default::default)
            .network_mode = Some(network_mode);
    }
}

/// Pure admission fence for the focused sibling network selection. The
/// canonical Tools writer remains [`apply_tool_grant_selection`].
pub fn validate_tool_network_selection(network_mode: Option<CommandNetworkMode>) -> Result<()> {
    anyhow::ensure!(
        network_mode.is_none_or(|mode| mode == CommandNetworkMode::Disabled),
        "config behavior tools may only narrow network_mode to disabled"
    );
    Ok(())
}

/// Per-call plumbing for one category patch. Boxed closures keep the core's
/// write operation single-sourced while each tool supplies target resolution,
/// validation, creation defaults, and its slice of the no-lockout guard.
pub(crate) struct ApplyRequest<'a> {
    pub(crate) target: SelfConfigTarget,
    pub(crate) patch: SelfConfigPatch,
    pub(crate) allow_create: bool,
    pub(crate) require_create: bool,
    pub(crate) guard_selected_chain: bool,
    pub(crate) resolve_unique: Box<dyn Fn(&BehaviorAnchor) -> Result<String> + Send + Sync + 'a>,
    pub(crate) on_create:
        Box<dyn Fn(&str, &mut Map<String, Value>) -> Result<()> + Send + Sync + 'a>,
    pub(crate) normalize: NormalizeFn<'a>,
    pub(crate) validate: ValidateFn<'a>,
    /// Invoker-only no-lockout slice over (stored, candidate) target documents.
    pub(crate) guard: Box<
        dyn Fn(&BehaviorAnchor, &Map<String, Value>, &Map<String, Value>) -> Result<()>
            + Send
            + Sync
            + 'a,
    >,
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

pub(crate) type NormalizeFn<'a> = Box<
    dyn for<'b> Fn(
            &'b ConfigApplyTxn<'b>,
            &'b BehaviorAnchor,
            &'b Map<String, Value>,
            &'b mut Map<String, Value>,
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
            require_create: false,
            guard_selected_chain: true,
            resolve_unique: Box::new(|_| bail!("resolve_unique not set (internal bug)")),
            on_create: Box::new(|_, _| Ok(())),
            normalize: Box::new(|_, _, _, _| Box::pin(async { Ok(()) })),
            validate: Box::new(|_, _, _, _| Box::pin(async { Ok(()) })),
            guard: Box::new(|_, _, _| Ok(())),
        }
    }
}

/// Decode a merged document projection into a typed document for structural
/// validation. The error names the field path and its advertised shape, not
/// the Rust type serde expected there.
pub(crate) fn decode_merged<T: serde::de::DeserializeOwned>(
    collection: &str,
    merged: &Map<String, Value>,
) -> Result<T> {
    serde_path_to_error::deserialize(Value::Object(merged.clone())).map_err(|error| {
        let path = error.path().to_string();
        let inner = error.inner().to_string();
        match super::command::field_shape(collection, &path) {
            Some(shape) => {
                let found = inner.split(", expected ").next().unwrap_or(&inner);
                let shape = shape
                    .as_str()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| shape.to_string());
                anyhow!("{collection} field {path}: {found}; expected {shape}")
            }
            None if path != "." => anyhow!("{collection} field {path}: {inner}"),
            None => anyhow!("merged {collection} document is not valid: {inner}"),
        }
    })
}

/// Lean `SelfConfig.keepsReach`: the invoking behavior stays enabled and keeps
/// the Setup tag it had, which routes persona-request protection and desktop
/// reachability to the Engineer.
pub fn guard_behavior_keeps_reach(
    stored: &Map<String, Value>,
    candidate: &Map<String, Value>,
) -> Result<()> {
    let setup_tag = |doc: &Map<String, Value>| {
        doc.get("tags")
            .and_then(Value::as_array)
            .is_some_and(|tags| {
                tags.iter().any(|tag| {
                    tag.as_str() == Some(crate::agent::persona_ops::SETUP_STEWARD_BEHAVIOR_TAG)
                })
            })
    };
    anyhow::ensure!(
        candidate.get("enabled").and_then(Value::as_bool) != Some(false),
        "no-lockout guard: behavior must remain enabled"
    );
    anyhow::ensure!(
        !setup_tag(stored) || setup_tag(candidate),
        "no-lockout guard: the Setup tag must remain on the configurator"
    );
    Ok(())
}

/// Lean `SelfConfig.keepsControl`: the invoker's candidate Tools keep its
/// self-config tool on and keep the agents group, the no-lockout guard and the
/// `tools` category it already had. This is the only self-protection on its
/// own Tools (#1796).
pub fn guard_tools_keep_control(
    stored: &Map<String, Value>,
    candidate: &Map<String, Value>,
) -> Result<()> {
    let control = |tools: Tools| {
        let config = tools.self_config.unwrap_or_default();
        [
            config.enable_self_config.unwrap_or(false),
            tools
                .subagents
                .and_then(|agents| agents.enabled)
                .unwrap_or(false),
            config.self_config_no_lockout.unwrap_or(false),
            config
                .self_config_categories
                .as_ref()
                .is_none_or(|categories| {
                    categories.iter().any(|category| category.trim() == "tools")
                }),
        ]
    };
    let [_, had_agents, had_guard, had_tools] = control(decode_merged("Tools", stored)?);
    let [self_config, agents, guard, tools] = control(decode_merged("Tools", candidate)?);
    anyhow::ensure!(
        self_config,
        "no-lockout guard: self-config must remain enabled in Tools.self_config.enable_self_config. Read tools get and preserve that group when updating Tools or selecting a replacement Context"
    );
    anyhow::ensure!(
        !had_agents || agents,
        "no-lockout guard: the agents tools must remain enabled"
    );
    anyhow::ensure!(
        !had_guard || guard,
        "no-lockout guard: self_config_no_lockout must remain enabled"
    );
    anyhow::ensure!(
        !had_tools || tools,
        "no-lockout guard: self-config must keep the tools category"
    );
    Ok(())
}
/// Lean `SelfConfig.authGuard`: the model may not introduce or change a raw
/// API key, and a principal-OAuth candidate keeps the stored account reference
/// (none for a non-OAuth backend). A stored or candidate `auth` that does not
/// decode is rejected.
pub fn guard_backend_auth(
    stored: &Map<String, Value>,
    candidate: &Map<String, Value>,
) -> Result<()> {
    let auth = |doc: &Map<String, Value>| {
        serde_json::from_value::<BackendAuth>(doc.get("auth").cloned().unwrap_or(Value::Null))
            .map_err(|error| anyhow!("backend auth is not valid: {error}"))
    };
    let (stored, candidate) = (auth(stored)?, auth(candidate)?);
    if matches!(candidate, BackendAuth::ApiKey { .. }) {
        anyhow::ensure!(
            candidate == stored,
            "raw API keys are operator-managed; select an environment or OAuth reference"
        );
    }
    if let BackendAuth::PrincipalOAuth { account_ref } = &candidate {
        let stored_ref = match &stored {
            BackendAuth::PrincipalOAuth { account_ref } => account_ref.as_ref(),
            _ => None,
        };
        anyhow::ensure!(
            account_ref.as_ref() == stored_ref,
            "OAuth account references are operator-managed; keep the backend's account_ref as stored"
        );
    }
    Ok(())
}
/// Lean `SelfConfig.backendChoiceAllowed`: whether a model selection may move
/// from the `current` backend (none on create) to `next`.
pub fn guard_backend_choice(
    current: Option<&crate::InferenceBackend>,
    next: &crate::InferenceBackend,
) -> Result<()> {
    let BackendAuth::PrincipalOAuth { account_ref } = &next.auth else {
        return Ok(());
    };
    // The #2117 resolver replaces this with the provider's earliest-connected
    // enabled account; until then the default is the original account.
    let default_account: Option<&String> = None;
    let allowed = match current {
        Some(current) if current.provider_kind == next.provider_kind => current.auth == next.auth,
        _ => account_ref.as_ref() == default_account,
    };
    anyhow::ensure!(
        allowed,
        "backend {} selects another {} account; keep the current account, or pick an account-free backend or another provider's default account",
        next.backend_id,
        next.provider_kind
    );
    Ok(())
}
/// [`guard_backend_choice`] over the owner's stored backends, inside the
/// write transaction; `current_backend_id` is `None` on create.
pub(crate) async fn guard_backend_choice_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    current_backend_id: Option<&str>,
    next_backend_id: &str,
) -> Result<()> {
    let backend = |id: String| async move {
        read_owned_doc(txn, SelfConfigTarget::InferenceBackend, owner, &id)
            .await?
            .map(|(_, doc)| crate::InferenceBackend::from_value(&Value::Object(doc)))
            .transpose()
    };
    let current = match current_backend_id {
        Some(id) => backend(id.to_owned()).await?,
        None => None,
    };
    let next = backend(next_backend_id.to_owned())
        .await?
        .with_context(|| format!("InferenceBackend {next_backend_id:?} not found"))?;
    guard_backend_choice(current.as_ref(), &next)
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
        .with_context(|| format!("candidate chain references missing {} {id:?}; inspect the role with behavior get, then select an existing document or create this dependency before selecting it", wanted.collection_name()))?
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
