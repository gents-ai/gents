mod apply;
mod automation;
mod load;
mod snapshot;

pub(crate) use apply::apply_control_update;
pub(crate) use load::load_document_runtime_view;
pub(crate) use snapshot::resolve_document_runtime_snapshot_from_view;

use crate::document_config::*;
use crate::oauth_credential::OAuthCredential;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub(crate) struct DocumentRecord<T> {
    pub(crate) doc_id: String,
    pub(crate) value: T,
}

/// One principal's canonical documents. Logical keys are scoped by this view's
/// principal; loader rejects duplicates and no global-ID fallback is permitted.
#[derive(Debug, Clone)]
pub(crate) struct DocumentRuntimeView {
    pub(crate) principal: DocumentRecord<AgentPrincipal>,
    pub(crate) behaviors: HashMap<String, DocumentRecord<AgentBehavior>>,
    pub(crate) contexts: HashMap<String, DocumentRecord<AgentContext>>,
    pub(crate) compactions: HashMap<String, DocumentRecord<CompactionConfig>>,
    pub(crate) tools: HashMap<String, DocumentRecord<Tools>>,
    pub(crate) subagent_targets: HashMap<String, DocumentRecord<SubagentTargetDocument>>,
    pub(crate) skills: HashMap<String, DocumentRecord<SkillDocument>>,
    pub(crate) datastore_tool_surfaces:
        HashMap<String, DocumentRecord<DatastoreToolSurfaceDocument>>,
    pub(crate) eth_tools: HashMap<String, DocumentRecord<EthToolDocument>>,
    pub(crate) inference_profiles: HashMap<String, DocumentRecord<InferenceProfile>>,
    pub(crate) inference_sampling: HashMap<String, DocumentRecord<InferenceSampling>>,
    pub(crate) inference_execution: HashMap<String, DocumentRecord<InferenceExecution>>,
    pub(crate) inference_retry_policies: HashMap<String, DocumentRecord<InferenceRetryPolicy>>,
    pub(crate) backends: HashMap<String, DocumentRecord<InferenceBackend>>,
    pub(crate) tasks: HashMap<String, DocumentRecord<Task>>,
    pub(crate) schedules: HashMap<String, DocumentRecord<Schedule>>,
    pub(crate) triggers: HashMap<String, DocumentRecord<Trigger>>,
    pub(crate) event_sources: HashMap<String, DocumentRecord<EventSource>>,
    pub(crate) callbacks: HashMap<String, DocumentRecord<Callback>>,
    pub(crate) callback_bindings: HashMap<String, DocumentRecord<CallbackBinding>>,
    pub(crate) graph_definitions: HashMap<String, DocumentRecord<GraphDefinition>>,
    pub(crate) chain_key_bindings: HashMap<String, DocumentRecord<ChainKeyBindingDocument>>,
    pub(crate) tool_services: HashMap<String, DocumentRecord<ToolServiceRegistry>>,
    pub(crate) projection_acp_bindings: HashMap<String, DocumentRecord<ProjectionAcpBinding>>,
    pub(crate) callback_modules: HashMap<String, DocumentRecord<CallbackModule>>,
    pub(crate) repository_placements: HashMap<String, DocumentRecord<RepositoryPlacement>>,
    pub(crate) backend_observations: HashMap<String, InferenceBackendObservation>,
    pub(crate) oauth_credentials: HashMap<String, DocumentRecord<OAuthCredential>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlUpdateOutcome {
    Irrelevant,
    Applied,
    PendingVisibility,
    FullReload,
}

impl DocumentRuntimeView {
    pub(super) fn has_enabled_oauth_credential(&self, provider: &str) -> bool {
        self.oauth_credentials
            .values()
            .any(|record| record.value.provider == provider && record.value.enabled)
    }

    pub(crate) fn has_unresolved_behavior_references(&self) -> bool {
        !self.pending_visibility_details().is_empty()
    }

    pub(crate) fn pending_visibility_details(&self) -> Vec<String> {
        let mut details = Vec::new();
        if let Some(id) = self.principal.value.default_behavior_id.as_deref() {
            if !self.behaviors.contains_key(id) {
                details.push(format!(
                    "principal {} references missing default behavior {id}",
                    self.principal.value.agent_did
                ));
            }
        }
        for record in self.behaviors.values() {
            snapshot::collect_unresolved_behavior_references(self, &record.value, &mut details);
        }
        details.sort();
        details
    }
}

#[cfg(test)]
mod tests;

/// Resolve selected surface documents within this principal's tool owner.
pub(crate) fn merge_surface_tools(
    tools: &Tools,
    view: &DocumentRuntimeView,
) -> anyhow::Result<crate::document_config::MergedSurfaceTools> {
    crate::document_config::merge_datastore_tool_surfaces(
        tools,
        view.datastore_tool_surfaces
            .values()
            .map(|record| &record.value),
    )
}

#[derive(Debug, Default)]
pub(crate) struct ExpandedEthTools {
    pub queries: Vec<crate::eth::ResolvedEthQuery>,
    pub calls: Vec<crate::eth::ResolvedEthCall>,
}

/// Expand `eth_tool_ids` into query and call tools. Missing / foreign ids fail closed.
/// Disabled EthTools are skipped. Empty `query_methods` / `calls` advertise nothing.
pub(crate) fn expand_eth_tools(
    selection: &Tools,
    view: &DocumentRuntimeView,
) -> anyhow::Result<ExpandedEthTools> {
    expand_eth_tools_with(selection, |tool_id| {
        view.eth_tools.get(tool_id).map(|record| &record.value)
    })
}

pub(crate) fn expand_eth_tools_from_docs(
    selection: &Tools,
    docs: &[crate::document_config::EthToolDocument],
) -> anyhow::Result<ExpandedEthTools> {
    expand_eth_tools_with(selection, |tool_id| {
        docs.iter().find(|doc| doc.tool_id == tool_id)
    })
}

fn expand_eth_tools_with<'a>(
    selection: &Tools,
    lookup: impl Fn(&str) -> Option<&'a crate::document_config::EthToolDocument>,
) -> anyhow::Result<ExpandedEthTools> {
    use anyhow::{anyhow, bail};
    use std::collections::HashSet;

    let mut out = ExpandedEthTools::default();
    let mut linked_ids: HashSet<&str> = HashSet::new();
    let mut tool_names: HashSet<String> = HashSet::new();
    for tool_id in selection
        .integrations
        .as_ref()
        .and_then(|group| group.eth_tool_ids.as_deref())
        .unwrap_or(&[])
    {
        let tool_id = tool_id.trim();
        if tool_id.is_empty() {
            bail!(
                "Tools {} has an empty eth_tool_ids entry",
                selection.tools_id
            );
        }
        if !linked_ids.insert(tool_id) {
            bail!(
                "Tools {} lists EthTool {} more than once",
                selection.tools_id,
                tool_id
            );
        }
        let doc = lookup(tool_id).ok_or_else(|| {
            anyhow!(
                "Tools {} references missing EthTool {}",
                selection.tools_id,
                tool_id
            )
        })?;
        if doc.agent_did.trim() != selection.agent_did.trim() {
            bail!(
                "Tools {} references EthTool {} owned by a different agent",
                selection.tools_id,
                tool_id
            );
        }
        if !doc.enabled {
            continue;
        }
        if let Some(resolved) = crate::eth::ResolvedEthQuery::from_document(doc)? {
            if !tool_names.insert(resolved.tool_name()) {
                bail!(
                    "duplicate eth tool name {:?} after expanding EthTool {} for Tools {}",
                    resolved.tool_name(),
                    tool_id,
                    selection.tools_id
                );
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
                &doc.agent_did,
                doc.key_binding_id.as_deref(),
                crate::eth::HttpEthRpc::configured_timeout(doc.rpc_timeout_secs)?,
            )?;
            for call in calls {
                if !tool_names.insert(call.tool_name.clone()) {
                    bail!(
                        "duplicate eth tool name {:?} after expanding EthTool {} for Tools {}",
                        call.tool_name,
                        tool_id,
                        selection.tools_id
                    );
                }
                out.calls.push(call);
            }
        } else if !decls.is_empty() {
            bail!(
                "EthTool {} has call tools but missing rpc_url or chain_id",
                tool_id
            );
        }
    }
    Ok(out)
}
