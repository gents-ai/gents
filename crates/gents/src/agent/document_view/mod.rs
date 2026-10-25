mod apply;
mod automation;
mod load;
mod snapshot;

pub(crate) use apply::{apply_control_update, control_collection_names};
pub(crate) use load::load_document_runtime_view;
pub(crate) use snapshot::resolve_document_runtime_snapshot_from_view;

use crate::document_config::*;
use crate::oauth_credential::OAuthCredential;
use rapidhash::RapidHashMap;

use crate::admission::BackendAvailability;

#[derive(Debug, thiserror::Error)]
pub enum DocumentViewError {
    #[error("runtime view owner mismatch")]
    OwnerMismatch,
    #[error("runtime view requires an owner DID")]
    OwnerDidRequired,
    #[error("scoped principal is missing")]
    ScopedPrincipalMissing,
    #[error("OAuthCredential missing physical ID")]
    OAuthCredentialMissingPhysicalId,
    #[error("OAuthCredential owner mismatch")]
    OAuthCredentialOwnerMismatch,
    #[error("duplicate OAuthCredential {id}")]
    DuplicateOAuthCredential { id: String },
    #[error("decoding {name} {id:?}")]
    Decode {
        name: String,
        id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("{name} query did not return rows")]
    QueryRowsMissing { name: String },
    #[error("config row is not an object")]
    ConfigRowNotObject,
    #[error("config row missing physical ID")]
    ConfigRowMissingPhysicalId,
    #[error("config row has empty physical ID")]
    ConfigRowEmptyPhysicalId,
    #[error("foreign {name} row escaped owner filter")]
    ForeignRowEscapedOwnerFilter { name: String },
    #[error("{name} missing logical key")]
    MissingLogicalKey { name: String },
    #[error("missing canonical config")]
    MissingCanonicalConfig,
    #[error("ambiguous {name} reference {id:?} for {owner:?}")]
    AmbiguousReference {
        name: String,
        id: String,
        owner: String,
    },
    #[error("explicit configuration reference is blank")]
    BlankReference,
    #[error("missing {kind} reference {id:?}")]
    MissingReference { kind: &'static str, id: String },
    #[error("foreign configuration reference {id:?}")]
    ForeignReference { id: String },
    #[error("agent owner mismatch")]
    AgentOwnerMismatch,
    #[error("node {node_did} is disabled")]
    NodeDisabled { node_did: String },
    #[error("profile {profile_id} has no model selection")]
    ProfileWithoutModel { profile_id: String },
    /// An agent_config on a backend this build cannot run is unavailable, not
    /// pending: no arriving document repairs it.
    #[error("backend {backend_id} has provider kind {kind} this build does not know")]
    UnknownKindBackend { backend_id: String, kind: String },
    #[error("backend {backend_id} has no matching observation")]
    BackendObservationMissing { backend_id: String },
    #[error("backend {backend_id} is unavailable: {availability:?}")]
    BackendUnavailable {
        backend_id: String,
        availability: BackendAvailability,
    },
    #[error("provider has no node OAuth adapter")]
    ProviderWithoutOAuthAdapter,
    #[error("backend {backend_id} requires enabled OAuthCredential for {scope}")]
    OAuthCredentialRequired { backend_id: String, scope: String },
    #[error("Tools {tools_id} has an empty eth_tool_ids entry")]
    EthToolIdEmpty { tools_id: String },
    #[error("Tools {tools_id} lists EthTool {tool_id} more than once")]
    EthToolListedTwice { tools_id: String, tool_id: String },
    #[error("Tools {tools_id} references missing EthTool {tool_id}")]
    EthToolMissing { tools_id: String, tool_id: String },
    #[error("Tools {tools_id} references EthTool {tool_id} owned by a different agent")]
    EthToolForeignOwner { tools_id: String, tool_id: String },
    #[error("duplicate eth tool name {tool_name:?} after expanding EthTool {tool_id} for Tools {tools_id}")]
    DuplicateEthToolName {
        tool_name: String,
        tool_id: String,
        tools_id: String,
    },
    #[error("EthTool {tool_id} has call tools but missing rpc_url or chain_id")]
    EthToolCallsWithoutRpc { tool_id: String },
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    #[error(transparent)]
    DocumentConfig(Box<crate::document_config::DocumentConfigError>),
    #[error(transparent)]
    Eth(#[from] crate::eth::EthError),
}

impl From<crate::document_config::DocumentConfigError> for DocumentViewError {
    fn from(error: crate::document_config::DocumentConfigError) -> Self {
        Self::DocumentConfig(Box::new(error))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DocumentRecord<T> {
    pub(crate) doc_id: String,
    pub(crate) value: T,
}

#[derive(Debug, Clone)]
pub(crate) struct DocumentRuntimeView {
    pub(crate) node: DocumentRecord<Node>,
    pub(crate) agents: RapidHashMap<String, DocumentRecord<Agent>>,
    pub(crate) contexts: RapidHashMap<String, DocumentRecord<AgentContext>>,
    pub(crate) compactions: RapidHashMap<String, DocumentRecord<CompactionConfig>>,
    pub(crate) tools: RapidHashMap<String, DocumentRecord<Tools>>,
    pub(crate) agent_targets: RapidHashMap<String, DocumentRecord<AgentTargetDocument>>,
    pub(crate) skills: RapidHashMap<String, DocumentRecord<SkillDocument>>,
    pub(crate) datastore_tool_surfaces:
        RapidHashMap<String, DocumentRecord<DatastoreToolSurfaceDocument>>,
    pub(crate) eth_tools: RapidHashMap<String, DocumentRecord<EthToolDocument>>,
    pub(crate) inference_profiles: RapidHashMap<String, DocumentRecord<InferenceProfile>>,
    pub(crate) inference_sampling: RapidHashMap<String, DocumentRecord<InferenceSampling>>,
    pub(crate) inference_execution: RapidHashMap<String, DocumentRecord<InferenceExecution>>,
    pub(crate) inference_retry_policies: RapidHashMap<String, DocumentRecord<InferenceRetryPolicy>>,
    pub(crate) backends: RapidHashMap<String, DocumentRecord<InferenceBackend>>,
    pub(crate) tasks: RapidHashMap<String, DocumentRecord<Task>>,
    pub(crate) schedules: RapidHashMap<String, DocumentRecord<Schedule>>,
    pub(crate) triggers: RapidHashMap<String, DocumentRecord<Trigger>>,
    pub(crate) event_sources: RapidHashMap<String, DocumentRecord<EventSource>>,
    pub(crate) callbacks: RapidHashMap<String, DocumentRecord<Callback>>,
    pub(crate) callback_bindings: RapidHashMap<String, DocumentRecord<CallbackBinding>>,
    pub(crate) chain_key_bindings: RapidHashMap<String, DocumentRecord<ChainKeyBindingDocument>>,
    pub(crate) tool_services: RapidHashMap<String, DocumentRecord<ToolServiceRegistry>>,
    pub(crate) projection_acp_bindings: RapidHashMap<String, DocumentRecord<ProjectionAcpBinding>>,
    pub(crate) callback_modules: RapidHashMap<String, DocumentRecord<CallbackModule>>,
    pub(crate) repository_placements: RapidHashMap<String, DocumentRecord<RepositoryPlacement>>,
    pub(crate) backend_observations: RapidHashMap<String, InferenceBackendObservation>,
    pub(crate) unknown_kind_backends: RapidHashMap<String, String>,
    pub(crate) oauth_credentials: RapidHashMap<String, DocumentRecord<OAuthCredential>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlUpdateOutcome {
    Irrelevant,
    FullReload,
}

impl DocumentRuntimeView {
    pub(super) fn resolves_oauth_credential(
        &self,
        provider: &str,
        pick: crate::oauth_credential::AccountPick<'_>,
    ) -> bool {
        crate::oauth_credential::pick_oauth_credential(
            self.oauth_credentials.values().map(|record| &record.value),
            &self.node.value.node_did,
            provider,
            pick,
        )
        .is_some()
    }

    pub(crate) fn has_unresolved_agent_references(&self) -> bool {
        !self.pending_visibility_details().is_empty()
    }

    pub(crate) fn pending_visibility_details(&self) -> Vec<String> {
        let mut details = Vec::new();
        if let Some(id) = self.node.value.default_agent_id.as_deref() {
            if !self.agents.contains_key(id) {
                details.push(format!(
                    "principal {} references missing default agent {id}",
                    self.node.value.node_did
                ));
            }
        }
        for record in self.agents.values() {
            snapshot::collect_unresolved_behavior_references(self, &record.value, &mut details);
        }
        details.sort();
        details
    }
}

#[cfg(test)]
mod tests;

pub(crate) fn merge_surface_tools(
    tools: &Tools,
    view: &DocumentRuntimeView,
) -> Result<crate::document_config::MergedSurfaceTools, DocumentViewError> {
    Ok(crate::document_config::merge_datastore_tool_surfaces(
        tools,
        view.datastore_tool_surfaces
            .values()
            .map(|record| &record.value),
    )?)
}

#[derive(Debug, Default)]
pub(crate) struct ExpandedEthTools {
    pub queries: Vec<crate::eth::ResolvedEthQuery>,
    pub calls: Vec<crate::eth::ResolvedEthCall>,
}

pub(crate) fn expand_eth_tools(
    selection: &Tools,
    view: &DocumentRuntimeView,
) -> Result<ExpandedEthTools, DocumentViewError> {
    expand_eth_tools_with(selection, |tool_id| {
        view.eth_tools.get(tool_id).map(|record| &record.value)
    })
}

pub(crate) fn expand_eth_tools_from_docs(
    selection: &Tools,
    docs: &[crate::document_config::EthToolDocument],
) -> Result<ExpandedEthTools, DocumentViewError> {
    expand_eth_tools_with(selection, |tool_id| {
        docs.iter().find(|doc| doc.tool_id == tool_id)
    })
}

fn expand_eth_tools_with<'a>(
    selection: &Tools,
    lookup: impl Fn(&str) -> Option<&'a crate::document_config::EthToolDocument>,
) -> Result<ExpandedEthTools, DocumentViewError> {
    use rapidhash::{HashSetExt, RapidHashSet};

    let mut out = ExpandedEthTools::default();
    let mut linked_ids: RapidHashSet<&str> = RapidHashSet::new();
    let mut tool_names: RapidHashSet<String> = RapidHashSet::new();
    for tool_id in selection
        .integrations
        .as_ref()
        .and_then(|group| group.eth_tool_ids.as_deref())
        .unwrap_or(&[])
    {
        let tool_id = tool_id.trim();
        if tool_id.is_empty() {
            return Err(DocumentViewError::EthToolIdEmpty {
                tools_id: selection.tools_id.clone(),
            });
        }
        if !linked_ids.insert(tool_id) {
            return Err(DocumentViewError::EthToolListedTwice {
                tools_id: selection.tools_id.clone(),
                tool_id: tool_id.to_owned(),
            });
        }
        let doc = lookup(tool_id).ok_or_else(|| DocumentViewError::EthToolMissing {
            tools_id: selection.tools_id.clone(),
            tool_id: tool_id.to_owned(),
        })?;
        if doc.node_did.trim() != selection.node_did.trim() {
            return Err(DocumentViewError::EthToolForeignOwner {
                tools_id: selection.tools_id.clone(),
                tool_id: tool_id.to_owned(),
            });
        }
        if !doc.enabled {
            continue;
        }
        if let Some(resolved) = crate::eth::ResolvedEthQuery::from_document(doc)? {
            if !tool_names.insert(resolved.tool_name()) {
                return Err(DocumentViewError::DuplicateEthToolName {
                    tool_name: resolved.tool_name(),
                    tool_id: tool_id.to_owned(),
                    tools_id: selection.tools_id.clone(),
                });
            }
            out.queries.push(resolved);
        }
        let decls = crate::eth::parse_call_decls(doc.calls.as_deref())?;
        let rpc_url = doc.rpc_url.as_deref().unwrap_or("");
        let chain_id = doc.chain_id.unwrap_or(0).max(0) as u64;
        crate::eth::validate_call_decls(&decls, (chain_id > 0).then_some(chain_id))?;
        if !rpc_url.trim().is_empty() && chain_id > 0 {
            let calls = crate::eth::ResolvedEthCall::from_decls(
                &doc.tool_id,
                chain_id,
                rpc_url,
                &decls,
                &doc.node_did,
                doc.key_binding_id.as_deref(),
                crate::eth::HttpEthRpc::configured_timeout(doc.rpc_timeout_secs)?,
            )?;
            for call in calls {
                if !tool_names.insert(call.tool_name.clone()) {
                    return Err(DocumentViewError::DuplicateEthToolName {
                        tool_name: call.tool_name.clone(),
                        tool_id: tool_id.to_owned(),
                        tools_id: selection.tools_id.clone(),
                    });
                }
                out.calls.push(call);
            }
        } else if !decls.is_empty() {
            return Err(DocumentViewError::EthToolCallsWithoutRpc {
                tool_id: tool_id.to_owned(),
            });
        }
    }
    Ok(out)
}
