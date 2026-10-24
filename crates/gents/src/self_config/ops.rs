use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::job::PluginJobSettings;
use identity::Did;
use serde_json::{json, Map, Value};

use crate::backend_provider::BackendProviderOauthExt;
use crate::config_client::patch::{
    apply_patch, diff_docs, ensure_admissible, FieldDelta, SelfConfigPatch, SelfConfigTarget,
};
use crate::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, validate_desired_state_plan,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::document_config::{BackendAuth, IntegrationTools, SelfConfigTools, Tools};
use crate::tool_surface::SelfConfigProcessCeiling;
use crate::toolset::CommandNetworkMode;

#[derive(Debug, thiserror::Error)]
#[error("no owned {} with ID {id:?}. This is a document ID, not an agent name. List this resource to find its exact IDs; agent get shows a role's selected Context, Tools and profile", target.collection_name())]
pub(super) struct MissingConfigDocument {
    pub target: SelfConfigTarget,
    pub id: String,
}

#[derive(Debug, thiserror::Error)]
pub(super) struct MissingAgent {
    pub agent_id: String,
    pub suggestions: Vec<String>,
}

impl std::fmt::Display for MissingAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown agent_id {:?}; ", self.agent_id)?;
        match self.suggestions.as_slice() {
            [] => write!(
                f,
                "copy an exact ID from [\"agent\",\"list\"] (agent create returns \"<DID>:<slug>\")"
            ),
            [only] => write!(f, "did you mean {only:?}?"),
            several => write!(f, "did you mean one of {several:?}?"),
        }
    }
}

pub const EFFECT_TIMING_NOTE: &str = "Committed changes are picked up by the runtime reconciler \
     (typically within a few seconds) and apply to requests dispatched after \
     the resulting generation swap; the current turn keeps its existing \
     configuration.";

/// Display text is model-facing recovery text; keep it stable.
#[derive(Debug, thiserror::Error)]
pub enum GuardError {
    #[error("no-lockout guard: agent must remain enabled")]
    AgentDisabled,
    #[error("no-lockout guard: the Engineer tag must remain on the configurator")]
    EngineerTagRemoved,
    #[error("no-lockout guard: self-config must remain enabled in Tools.self_config.enable_self_config. Read tools get and preserve that group when updating Tools or selecting a replacement Context")]
    SelfConfigDisabled,
    #[error("no-lockout guard: the agents tools must remain enabled")]
    AgentsToolsDisabled,
    #[error("no-lockout guard: self_config_no_lockout must remain enabled")]
    NoLockoutDisabled,
    #[error("no-lockout guard: self-config must keep the tools category")]
    ToolsCategoryRemoved,
    #[error("config agent tools may only narrow network_mode to disabled")]
    NetworkModeWidened,
    #[error("Tools.self_config does not decode; its operator grants cannot be read")]
    SelfConfigGroup(#[source] serde_json::Error),
    #[error("Tools.integrations does not decode; its operator grants cannot be read")]
    IntegrationsGroup(#[source] serde_json::Error),
    #[error("Tools.integrations.plugins names {plugin:?} twice; its job grant cannot be read")]
    DuplicatePluginJob { plugin: String },
    #[error("{refusal}")]
    JobGrantRefused { refusal: String },
    #[error("pack installation is operator-managed and cannot be self-granted")]
    PackInstallSelfGrant,
    #[error("{collection} field {path}: {found}; expected {shape}")]
    FieldShape {
        collection: String,
        path: String,
        found: String,
        shape: String,
    },
    #[error("{collection} field {path}: {inner}")]
    Field {
        collection: String,
        path: String,
        inner: String,
    },
    #[error("merged {collection} document is not valid: {inner}")]
    MergedDocument { collection: String, inner: String },
}

#[derive(Clone)]
pub struct SelfConfigCore {
    node: Arc<EmbeddedNode>,
    node_did: String,
    agent_id: String,
    lockout_agent_id: String,
    no_lockout: bool,
    process_ceiling: SelfConfigProcessCeiling,
    held_grants: OperatorGrants,
}

#[derive(Debug, serde::Serialize)]
pub struct PatchOutcome {
    pub committed: bool,
    pub collection: &'static str,
    pub target_id: String,
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<Value>,
    pub created: bool,
    pub changed: Vec<FieldDelta>,
    pub effect: &'static str,
    pub doc_id: Option<String>,
}

pub(super) fn target_destination(document: &Map<String, Value>, owner: &str) -> Value {
    json!({
        "kind": if document.get("target_node_did").and_then(Value::as_str) == Some(owner) { "local" } else { "remote" },
        "target_node_did": document.get("target_node_did"),
        "agent_id": document.get("agent_id"),
        "runtime_verified": false
    })
}

fn connection_view(
    target: SelfConfigTarget,
    anchor: &AgentAnchor,
    document: &Map<String, Value>,
    owner: &str,
) -> Option<Value> {
    match target {
        SelfConfigTarget::InferenceProfile => {
            let selected_id = anchor.ref_id("inference_profile_id");
            let profile_id = document.get("profile_id")?.as_str()?;
            let selected = selected_id.as_deref() == Some(profile_id);
            let mut view = json!({"agent_id": anchor.doc.get("agent_id"), "selected_profile_id": selected_id, "selected": selected});
            if !selected {
                view["select_with"] = json!({"argv":["agent","update"],"target_id":anchor.doc.get("agent_id"),"set":{"inference_profile_id":profile_id}});
            }
            Some(view)
        }
        SelfConfigTarget::AgentTarget => Some(target_destination(document, owner)),
        _ => None,
    }
}

pub(crate) struct AgentAnchor {
    pub(crate) doc: Map<String, Value>,
    pub(crate) context: Map<String, Value>,
    pub(crate) profile: Map<String, Value>,
    pub(crate) execution: Map<String, Value>,
}

impl AgentAnchor {
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
    pub fn new(node: Arc<EmbeddedNode>, node_did: String, agent_id: String) -> Result<Self> {
        if node_did.trim().is_empty() {
            bail!("self-config requires a non-empty node DID (fail closed)");
        }
        if agent_id.trim().is_empty() {
            bail!("self-config requires a non-empty agent id (fail closed)");
        }
        Ok(Self {
            node,
            node_did,
            lockout_agent_id: agent_id.clone(),
            agent_id,
            no_lockout: false,
            process_ceiling: SelfConfigProcessCeiling::default(),
            held_grants: OperatorGrants::default(),
        })
    }

    pub fn with_no_lockout(mut self, no_lockout: bool) -> Self {
        self.no_lockout = no_lockout;
        self
    }

    /// The operator grants the invoking agent holds. Sibling cores built for
    /// a catalog-authorized target carry the invoker's grants, never the
    /// target's.
    pub fn with_held_grants(mut self, held: OperatorGrants) -> Self {
        self.held_grants = held;
        self
    }

    pub fn held_grants(&self) -> &OperatorGrants {
        &self.held_grants
    }

    /// Preserve the invoking agent as the recoverability anchor while a
    /// catalog-authorized command targets a sibling agent.
    pub(crate) fn with_lockout_agent_id(mut self, agent_id: String) -> Self {
        self.lockout_agent_id = agent_id;
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

    pub fn node_did(&self) -> &str {
        &self.node_did
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub(crate) fn node_handle(&self) -> Arc<EmbeddedNode> {
        self.node.clone()
    }

    pub(crate) fn identity(&self) -> Result<Did> {
        Did::new(self.node_did.clone())
            .map_err(|error| anyhow!("node DID is not ACP-addressable: {error}"))
    }

    pub(crate) async fn load_agent_anchor(&self, txn: &ConfigApplyTxn<'_>) -> Result<AgentAnchor> {
        let Some((_doc_id, doc)) =
            read_owned_doc(txn, SelfConfigTarget::Agent, &self.node_did, &self.agent_id).await?
        else {
            return Err(MissingAgent {
                agent_id: self.agent_id.clone(),
                suggestions: Vec::new(),
            }
            .into());
        };
        let owner = doc.get("node_did").and_then(Value::as_str).unwrap_or("");
        if owner != self.node_did {
            bail!(
                "agent {} is owned by {owner:?}, not this node — self-config is self only",
                self.agent_id
            );
        }
        let context = match doc.get("context_id").and_then(Value::as_str) {
            Some(context_id) => {
                read_owned_doc(
                    txn,
                    SelfConfigTarget::AgentContext,
                    &self.node_did,
                    context_id,
                )
                .await?
                .context("context not found")?
                .1
            }
            None => Map::new(),
        };
        let profile_id = doc
            .get("inference_profile_id")
            .and_then(Value::as_str)
            .context("agent inference profile is missing")?;
        let profile = read_owned_doc(
            txn,
            SelfConfigTarget::InferenceProfile,
            &self.node_did,
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
                    &self.node_did,
                    id,
                )
                .await?
                .context("execution not found")?
                .1
            }
            None => Map::new(),
        };
        Ok(AgentAnchor {
            doc,
            context,
            profile,
            execution,
        })
    }

    pub(crate) async fn apply(&self, request: ApplyRequest<'_>) -> Result<PatchOutcome> {
        ensure_admissible(request.target, &request.patch)?;

        let identity = self.identity()?;
        let this = self;
        let request = &request;
        let outcome = ConfigAccess::transact_local_write(
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
        let anchor = self.load_agent_anchor(txn).await?;
        if self.agent_id != self.lockout_agent_id
            && matches!(
                request.target,
                SelfConfigTarget::Agent | SelfConfigTarget::AgentContext | SelfConfigTarget::Tools
            )
        {
            anyhow::ensure!(
                !anchor
                    .doc
                    .get("tags")
                    .and_then(Value::as_array)
                    .is_some_and(|tags| tags
                        .iter()
                        .any(|tag| tag.as_str() == Some(super::ENGINEER_AGENT_TAG))),
                "protected agent cannot be edited through sibling configuration"
            );
        }

        let unique_value = (request.resolve_unique)(&anchor)?;

        let stored = read_owned_doc(txn, request.target, &self.node_did, &unique_value).await?;
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
        guard_reselection_keeps_grants_in_txn(
            txn,
            self,
            request.target,
            (!creating).then_some(&stored_doc),
            &merged,
        )
        .await?;

        if self.no_lockout && request.guard_selected_chain {
            if self.lockout_agent_id == self.agent_id {
                (request.guard)(&anchor, &stored_doc, &merged)?;
            }
            self.guard_candidate_chain(txn, request.target, &merged)
                .await?;
        }

        let changed = safe_diff(request.target, &stored_doc, &merged);
        let plan = replacement_plan(request.target, &merged)?;
        apply_desired_state_plan(txn, &plan).await?;
        let doc_id = read_owned_doc(txn, request.target, &self.node_did, &unique_value)
            .await?
            .context("published config is missing")?
            .0;

        Ok(PatchOutcome {
            collection: request.target.collection_name(),
            agent_id: self.agent_id.clone(),
            connection: connection_view(request.target, &anchor, &merged, &self.node_did),
            target_id: unique_value,
            doc_id: Some(doc_id),
            created: creating,
            committed: false,
            changed,
            effect: if request.target == SelfConfigTarget::DatastoreToolSurface {
                "Install its collection schemas before selecting this surface. Read tools get, then add its ID to set.datastore.datastore_tool_surface_ids, preserving existing selections. Tools apply after reconciliation to later requests."
            } else if creating && request.target == SelfConfigTarget::InferenceProfile {
                "Creating a profile does not select it for an agent. To use it, call agent update with set.inference_profile_id equal to this target_id. Selecting it preserves the previous profile and its settings. The selection applies to later requests after reconciliation."
            } else if request.target == SelfConfigTarget::Tools
                && request.patch.iter().any(|(field, _)| field == "agents")
                && merged.get("agents").is_some_and(|group| {
                    group.get("enabled").and_then(Value::as_bool) != Some(true)
                        && group
                            .get("target_ids")
                            .and_then(Value::as_array)
                            .is_some_and(|ids| !ids.is_empty())
                })
            {
                "Selected targets are inactive: agents.enabled is not true. To enable delegation, call tools update with options.agent set to this agent_id and set.agents containing enabled:true plus the existing target_ids. Tools apply after reconciliation to later requests."
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
        let agent = candidate_doc(
            txn,
            self.node_did(),
            SelfConfigTarget::Agent,
            &self.lockout_agent_id,
            target,
            merged,
        )
        .await?;
        anyhow::ensure!(
            agent.get("enabled").and_then(Value::as_bool) != Some(false),
            "no-lockout: agent disabled"
        );
        let context_id = agent
            .get("context_id")
            .and_then(Value::as_str)
            .context("no-lockout: Agent.context_id is missing; preserve the current Context selection or select an existing Context")?;
        let context = candidate_doc(
            txn,
            self.node_did(),
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
            self.node_did(),
            SelfConfigTarget::Tools,
            tools_id,
            target,
            merged,
        )
        .await?;
        guard_tools_keep_control(&self.stored_lockout_tools(txn).await?, &tools)?;
        let profile_id = agent
            .get("inference_profile_id")
            .and_then(Value::as_str)
            .context(
                "no-lockout: Agent.inference_profile_id is missing; select an existing profile",
            )?;
        let profile = candidate_doc(
            txn,
            self.node_did(),
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
            self.node_did(),
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

    async fn stored_lockout_tools(&self, txn: &ConfigApplyTxn<'_>) -> Result<Map<String, Value>> {
        let owner = self.node_did();
        let read = |target, id: String| async move {
            read_owned_doc(txn, target, owner, &id)
                .await?
                .map(|(_, doc)| doc)
                .context("no-lockout: stored reference chain is incomplete")
        };
        let agent = read(SelfConfigTarget::Agent, self.lockout_agent_id.clone()).await?;
        let field = |doc: &Map<String, Value>, name: &str| {
            doc.get(name)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .with_context(|| format!("no-lockout: stored {name} missing"))
        };
        let context = read(SelfConfigTarget::AgentContext, field(&agent, "context_id")?).await?;
        read(SelfConfigTarget::Tools, field(&context, "tools_id")?).await
    }

    pub(crate) async fn preview(&self, request: ApplyRequest<'_>) -> Result<PatchOutcome> {
        ensure_admissible(request.target, &request.patch)?;
        let identity = self.identity()?;
        let this = self;
        let request = &request;
        ConfigAccess::transact_local_readonly(
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
        let anchor = self.load_agent_anchor(txn).await?;
        if self.agent_id != self.lockout_agent_id
            && matches!(
                request.target,
                SelfConfigTarget::Agent | SelfConfigTarget::AgentContext | SelfConfigTarget::Tools
            )
        {
            anyhow::ensure!(
                !anchor
                    .doc
                    .get("tags")
                    .and_then(Value::as_array)
                    .is_some_and(|tags| tags
                        .iter()
                        .any(|tag| tag.as_str() == Some(super::ENGINEER_AGENT_TAG))),
                "protected agent cannot be edited through sibling configuration"
            );
        }

        let unique_value = (request.resolve_unique)(&anchor)?;
        let stored = read_owned_doc(txn, request.target, &self.node_did, &unique_value).await?;
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
        guard_reselection_keeps_grants_in_txn(
            txn,
            self,
            request.target,
            (!creating).then_some(&stored_doc),
            &merged,
        )
        .await?;
        if self.no_lockout && request.guard_selected_chain {
            if self.lockout_agent_id == self.agent_id {
                (request.guard)(&anchor, &stored_doc, &merged)?;
            }
            self.guard_candidate_chain(txn, request.target, &merged)
                .await?;
        }
        validate_desired_state_plan(txn, &replacement_plan(request.target, &merged)?).await?;
        Ok(PatchOutcome {
            collection: request.target.collection_name(),
            agent_id: self.agent_id.clone(),
            changed: safe_diff(request.target, &stored_doc, &merged),
            connection: connection_view(request.target, &anchor, &merged, &self.node_did),
            target_id: unique_value,
            doc_id: None,
            created: creating,
            committed: false,
            effect: "preview: nothing was written; send the same create/update call without preview to apply",
        })
    }
}

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

pub fn validate_tool_network_selection(
    network_mode: Option<CommandNetworkMode>,
) -> Result<(), GuardError> {
    if !network_mode.is_none_or(|mode| mode == CommandNetworkMode::Disabled) {
        return Err(GuardError::NetworkModeWidened);
    }
    Ok(())
}

pub(crate) struct ApplyRequest<'a> {
    pub(crate) target: SelfConfigTarget,
    pub(crate) patch: SelfConfigPatch,
    pub(crate) allow_create: bool,
    pub(crate) require_create: bool,
    pub(crate) guard_selected_chain: bool,
    pub(crate) resolve_unique: ResolveUniqueFn<'a>,
    pub(crate) on_create: OnCreateFn<'a>,
    pub(crate) normalize: NormalizeFn<'a>,
    pub(crate) validate: ValidateFn<'a>,
    pub(crate) guard: GuardFn<'a>,
}

pub(crate) type ResolveUniqueFn<'a> =
    Box<dyn Fn(&AgentAnchor) -> Result<String> + Send + Sync + 'a>;
pub(crate) type OnCreateFn<'a> =
    Box<dyn Fn(&str, &mut Map<String, Value>) -> Result<()> + Send + Sync + 'a>;
pub(crate) type GuardFn<'a> = Box<
    dyn Fn(&AgentAnchor, &Map<String, Value>, &Map<String, Value>) -> Result<()> + Send + Sync + 'a,
>;

pub(crate) type ValidateFn<'a> = Box<
    dyn for<'b> Fn(
            &'b ConfigApplyTxn<'b>,
            &'b AgentAnchor,
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
            &'b AgentAnchor,
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

pub(crate) fn decode_merged<T>(
    collection: &str,
    merged: &Map<String, Value>,
) -> Result<T, GuardError>
where
    T: serde::de::DeserializeOwned,
{
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
                GuardError::FieldShape {
                    collection: collection.to_owned(),
                    path,
                    found: found.to_owned(),
                    shape,
                }
            }
            None if path != "." => GuardError::Field {
                collection: collection.to_owned(),
                path,
                inner,
            },
            None => GuardError::MergedDocument {
                collection: collection.to_owned(),
                inner,
            },
        }
    })
}

/// Lean `SelfConfig.keepsReach`.
pub fn guard_agent_keeps_reach(
    stored: &Map<String, Value>,
    candidate: &Map<String, Value>,
) -> Result<(), GuardError> {
    let engineer_tag = |doc: &Map<String, Value>| {
        doc.get("tags")
            .and_then(Value::as_array)
            .is_some_and(|tags| {
                tags.iter()
                    .any(|tag| tag.as_str() == Some(crate::self_config::ENGINEER_AGENT_TAG))
            })
    };
    if candidate.get("enabled").and_then(Value::as_bool) == Some(false) {
        return Err(GuardError::AgentDisabled);
    }
    if engineer_tag(stored) && !engineer_tag(candidate) {
        return Err(GuardError::EngineerTagRemoved);
    }
    Ok(())
}

/// Lean `SelfConfig.keepsControl`; operator grants are bounded separately by
/// [`guard_tools_keep_grants`].
pub fn guard_tools_keep_control(
    stored: &Map<String, Value>,
    candidate: &Map<String, Value>,
) -> Result<(), GuardError> {
    let control = |tools: Tools| {
        let config = tools.self_config.unwrap_or_default();
        [
            config.enable_self_config.unwrap_or(false),
            tools
                .agents
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
    if !self_config {
        return Err(GuardError::SelfConfigDisabled);
    }
    if had_agents && !agents {
        return Err(GuardError::AgentsToolsDisabled);
    }
    if had_guard && !guard {
        return Err(GuardError::NoLockoutDisabled);
    }
    if had_tools && !tools {
        return Err(GuardError::ToolsCategoryRemoved);
    }
    Ok(())
}

/// Operator-managed grants carried by a Tools document (Lean
/// `SelfConfig.Grants`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OperatorGrants {
    /// `self_config.enable_pack_install`.
    pub pack_install: bool,
    /// `integrations.plugins[].job` by plugin coordinate. A plugin with no
    /// entry has the empty ceiling.
    pub plugin_jobs: BTreeMap<String, PluginJobSettings>,
}

impl OperatorGrants {
    pub fn from_tools_json(tools: &Map<String, Value>) -> Result<Self, GuardError> {
        let config = match tools.get("self_config") {
            None | Some(Value::Null) => SelfConfigTools::default(),
            Some(group) => serde_json::from_value::<SelfConfigTools>(group.clone())
                .map_err(GuardError::SelfConfigGroup)?,
        };
        let integrations = match tools.get("integrations") {
            None | Some(Value::Null) => IntegrationTools::default(),
            Some(group) => serde_json::from_value::<IntegrationTools>(group.clone())
                .map_err(GuardError::IntegrationsGroup)?,
        };
        let mut plugin_jobs = BTreeMap::new();
        for plugin in integrations.plugins.into_iter().flatten() {
            let Some(job) = plugin.job else { continue };
            if plugin_jobs.insert(plugin.plugin.clone(), job).is_some() {
                return Err(GuardError::DuplicatePluginJob {
                    plugin: plugin.plugin,
                });
            }
        }
        Ok(Self {
            pack_install: config.enable_pack_install.unwrap_or(false),
            plugin_jobs,
        })
    }

    /// Lean `Grants.boundedBy` and `keepsJobGrants`: pack installation is
    /// held-bounded: carried by `stored` or held by `held`. Job blocks are
    /// bounded by `stored` alone ([`Self::job_refusal`]).
    pub fn bounded_by(&self, stored: &Self, held: &Self) -> bool {
        (!self.pack_install || stored.pack_install || held.pack_install)
            && self.job_refusal(stored).is_none()
    }

    /// Lean `JobGrant.narrows` over every job block `self` carries: the first
    /// block that adds to, widens or edits the policy of the block `stored`
    /// carries for the same plugin, as a refusal naming the field. An absent
    /// block is the empty ceiling, so a block with no stored counterpart is
    /// an addition.
    fn job_refusal(&self, stored: &Self) -> Option<String> {
        self.plugin_jobs.iter().find_map(|(plugin, job)| {
            let Some(base) = stored.plugin_jobs.get(plugin) else {
                return Some(format!(
                    "integrations.plugins[{plugin}].job is not in the stored Tools; only the operator may add a job block"
                ));
            };
            let field = if !job.agent_targets.iter().all(|t| base.agent_targets.contains(t)) {
                "agent_targets"
            } else if job.max_live > base.max_live {
                "max_live"
            } else if job.max_spawns > base.max_spawns {
                "max_spawns"
            } else if job.max_messages_per_slot > base.max_messages_per_slot {
                "max_messages_per_slot"
            } else if job.result_bytes > base.result_bytes {
                "result_bytes"
            } else if job.max_wall_secs > base.max_wall_secs {
                "max_wall_secs"
            } else if job.policy != base.policy {
                "policy"
            } else {
                return None;
            };
            Some(format!(
                "integrations.plugins[{plugin}].job.{field} differs from the stored block beyond narrowing; only the operator may widen a job grant or edit its policy"
            ))
        })
    }
}

/// Lean `SelfConfig.keepsGrants`; `stored` is `None` when no document is
/// replaced. Unlike [`guard_tools_keep_control`] this always runs, from the
/// validate slot.
pub fn guard_tools_keep_grants(
    held: &OperatorGrants,
    stored: Option<&Map<String, Value>>,
    candidate: &Map<String, Value>,
) -> Result<(), GuardError> {
    let candidate = OperatorGrants::from_tools_json(candidate)?;
    let stored = stored
        .map(OperatorGrants::from_tools_json)
        .transpose()?
        .unwrap_or_default();
    if let Some(refusal) = candidate.job_refusal(&stored) {
        return Err(GuardError::JobGrantRefused { refusal });
    }
    if !candidate.bounded_by(&stored, held) {
        return Err(GuardError::PackInstallSelfGrant);
    }
    Ok(())
}

/// Lean `SelfConfig.reselectionKeepsGrants`.
pub fn reselection_keeps_grants(
    held: &OperatorGrants,
    before: Option<&Map<String, Value>>,
    after: Option<&Map<String, Value>>,
) -> Result<(), GuardError> {
    match after {
        None => Ok(()),
        Some(after) => guard_tools_keep_grants(held, before, after),
    }
}

/// The native side of Lean `SelfConfig.chainKeepsGrants`. An unchanged
/// selection passes without reads (Lean
/// `chain_unchanged_selection_keeps_grants`).
pub(crate) async fn guard_reselection_keeps_grants_in_txn(
    txn: &ConfigApplyTxn<'_>,
    core: &SelfConfigCore,
    target: SelfConfigTarget,
    stored: Option<&Map<String, Value>>,
    candidate: &Map<String, Value>,
) -> Result<()> {
    let field = match target {
        SelfConfigTarget::AgentContext => "tools_id",
        SelfConfigTarget::Agent => "context_id",
        _ => return Ok(()),
    };
    let selected = |doc: &Map<String, Value>| {
        doc.get(field)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
    };
    let before = stored.and_then(&selected);
    let after = selected(candidate);
    if before == after {
        return Ok(());
    }
    let owner = core.node_did();
    let before_tools = selected_tools_in_txn(txn, owner, target, before.as_deref()).await?;
    let after_tools = selected_tools_in_txn(txn, owner, target, after.as_deref()).await?;
    reselection_keeps_grants(core.held_grants(), before_tools.as_ref(), after_tools.as_ref())
        .with_context(|| {
            format!(
                "{field} {:?} selects Tools carrying an operator grant this agent does not hold; select Tools without it or ask the operator to grant it",
                after.as_deref().unwrap_or_default()
            )
        })
}

async fn selected_tools_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    target: SelfConfigTarget,
    id: Option<&str>,
) -> Result<Option<Map<String, Value>>> {
    let Some(id) = id else {
        return Ok(None);
    };
    match target {
        SelfConfigTarget::AgentContext => tools_by_id_in_txn(txn, owner, id).await,
        _ => context_tools_in_txn(txn, owner, id).await,
    }
}

async fn tools_by_id_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    tools_id: &str,
) -> Result<Option<Map<String, Value>>> {
    Ok(
        read_owned_doc(txn, SelfConfigTarget::Tools, owner, tools_id)
            .await?
            .map(|(_, doc)| doc),
    )
}

async fn context_tools_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    context_id: &str,
) -> Result<Option<Map<String, Value>>> {
    let Some((_, context)) =
        read_owned_doc(txn, SelfConfigTarget::AgentContext, owner, context_id).await?
    else {
        return Ok(None);
    };
    match context.get("tools_id").and_then(Value::as_str) {
        Some(tools_id) if !tools_id.is_empty() => tools_by_id_in_txn(txn, owner, tools_id).await,
        _ => Ok(None),
    }
}

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
    if let BackendAuth::NodeOAuth { account_ref } = &candidate {
        let stored_ref = match &stored {
            BackendAuth::NodeOAuth { account_ref } => account_ref.as_ref(),
            _ => None,
        };
        anyhow::ensure!(
            account_ref.as_ref() == stored_ref,
            "OAuth account references are operator-managed; keep the backend's account_ref as stored"
        );
    }
    Ok(())
}
/// Lean `SelfConfig.backendChoiceAllowed`; `default_account` is `next`'s
/// provider default account (Lean `dflt`).
pub fn guard_backend_choice(
    current: Option<&crate::InferenceBackend>,
    next: &crate::InferenceBackend,
    default_account: Option<&str>,
) -> Result<()> {
    let BackendAuth::NodeOAuth { account_ref } = &next.auth else {
        return Ok(());
    };
    let allowed = match current {
        Some(current) if current.provider_kind == next.provider_kind => current.auth == next.auth,
        _ => account_ref.as_deref() == default_account,
    };
    anyhow::ensure!(
        allowed,
        "backend {} selects another {} account; keep the current account, or pick an account-free backend or another provider's default account",
        next.backend_id,
        next.provider_kind
    );
    Ok(())
}
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
            .map_err(anyhow::Error::from)
    };
    let current = match current_backend_id {
        Some(id) => backend(id.to_owned()).await?,
        None => None,
    };
    let next = backend(next_backend_id.to_owned())
        .await?
        .with_context(|| format!("InferenceBackend {next_backend_id:?} not found"))?;
    let default_account = match (&next.auth, next.provider_kind.oauth_provider()) {
        (BackendAuth::NodeOAuth { .. }, Some(provider)) => {
            crate::oauth_credential::provider_default_account_ref(txn, owner, provider).await?
        }
        _ => None,
    };
    guard_backend_choice(current.as_ref(), &next, default_account.as_deref())
}
pub(crate) async fn guard_compaction_choice_in_txn(
    txn: &ConfigApplyTxn<'_>,
    target: SelfConfigTarget,
    anchor: &AgentAnchor,
    stored: &Map<String, Value>,
    merged: &Map<String, Value>,
) -> Result<()> {
    let field = |doc: &Map<String, Value>, name: &str| {
        doc.get(name).and_then(Value::as_str).map(ToOwned::to_owned)
    };
    let owner = field(&anchor.doc, "node_did").context("agent is missing node_did")?;
    let read = |collection: SelfConfigTarget, id: Option<String>, name: &'static str| {
        let owner = owner.clone();
        async move {
            Ok::<_, anyhow::Error>(match id {
                Some(id) => read_owned_doc(txn, collection, &owner, &id)
                    .await?
                    .and_then(|(_, doc)| field(&doc, name)),
                None => None,
            })
        }
    };
    let (current, next) = match target {
        SelfConfigTarget::AgentContext => {
            let profile = field(&anchor.doc, "inference_profile_id");
            (
                (field(stored, "compaction_id"), profile.clone()),
                (field(merged, "compaction_id"), profile),
            )
        }
        SelfConfigTarget::Agent => {
            let context = |doc: &Map<String, Value>| {
                read(
                    SelfConfigTarget::AgentContext,
                    field(doc, "context_id"),
                    "compaction_id",
                )
            };
            (
                (
                    context(stored).await?,
                    field(stored, "inference_profile_id"),
                ),
                (
                    context(merged).await?,
                    field(merged, "inference_profile_id"),
                ),
            )
        }
        _ => return Ok(()),
    };
    if current == next {
        return Ok(());
    }
    let backend = |(compaction, profile): (Option<String>, Option<String>)| async {
        let profile = read(
            SelfConfigTarget::Compaction,
            compaction,
            "inference_profile_id",
        )
        .await?
        .or(profile)
        .context("agent inference profile is missing")?;
        profile_backend_id(txn, &owner, &profile).await
    };
    let (current, next) = (backend(current).await?, backend(next).await?);
    guard_backend_choice_in_txn(txn, &owner, Some(&current), &next).await
}
pub(crate) async fn profile_backend_id(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    profile_id: &str,
) -> Result<String> {
    read_owned_doc(txn, SelfConfigTarget::InferenceProfile, owner, profile_id)
        .await?
        .and_then(|(_, doc)| doc.get("backend_id")?.as_str().map(ToOwned::to_owned))
        .with_context(|| format!("InferenceProfile {profile_id:?} not found"))
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
    Ok(tools.validate()?)
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
        .with_context(|| format!("candidate chain references missing {} {id:?}; inspect the role with agent get, then select an existing document or create this dependency before selecting it", wanted.collection_name()))?
        .1)
}
/// Receipts summarize longer text fields by length and SHA-256: the caller
/// already holds the value it sent, and a prompt echoed in full costs its
/// size again in the model's context.
const RECEIPT_TEXT_LIMIT: usize = 2048;

fn safe_diff(
    target: SelfConfigTarget,
    before: &Map<String, Value>,
    after: &Map<String, Value>,
) -> Vec<FieldDelta> {
    let mut deltas = diff_docs(target, before, after);
    for delta in &mut deltas {
        for value in [&mut delta.from, &mut delta.to] {
            if let Some(text) = value
                .as_str()
                .filter(|text| text.len() > RECEIPT_TEXT_LIMIT)
            {
                use sha2::{Digest, Sha256};
                *value = json!({"chars": text.chars().count(), "sha256": format!("{:x}", Sha256::digest(text))});
            }
        }
    }
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
