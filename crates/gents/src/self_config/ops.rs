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

mod documents;
mod grants;
mod guards;
mod request;

use documents::{candidate_doc, replacement_plan, safe_diff};
pub use documents::{apply_tool_grant_selection, validate_tool_network_selection};
pub(crate) use documents::{decode_merged, read_owned_doc, validate_merged_selection};
pub(crate) use grants::guard_reselection_keeps_grants_in_txn;
pub use grants::{guard_tools_keep_grants, reselection_keeps_grants, OperatorGrants};
pub use guards::{
    guard_agent_keeps_reach, guard_backend_auth, guard_backend_choice, guard_tools_keep_control,
};
pub(crate) use guards::{
    guard_backend_choice_in_txn, guard_compaction_choice_in_txn, profile_backend_id,
};
pub(crate) use request::ApplyRequest;

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
