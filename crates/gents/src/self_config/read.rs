//! Read the canonical config graph under the invoking node's identity.
use super::ops::{read_owned_doc, AgentAnchor, SelfConfigCore, EFFECT_TIMING_NOTE};
use crate::config_client::patch::SelfConfigTarget;
use crate::config_client::{config_projection, ConfigAccess, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

/// Displays execution defaults without changing the authored document or a running request.
pub(super) fn execution_settings(document: Option<&Value>) -> Result<Value> {
    use crate::config::*;
    let mut document = document.cloned().unwrap_or_else(|| {
        json!({
            "node_did": "", "execution_id": ""
        })
    });
    if let Some(object) = document.as_object_mut() {
        object.remove("_docID");
    }
    let execution: crate::document_config::InferenceExecution = serde_json::from_value(document)?;
    execution.validate()?;
    Ok(json!({
        "max_turns": execution.max_turns.unwrap_or(DEFAULT_MAX_TURNS as i64),
        "max_total_tokens": execution.max_total_tokens,
        "stream_batch_ms": execution.stream_batch_ms.unwrap_or(DEFAULT_STREAM_BATCH_MS as i64),
        "stream_liveness_timeout_secs": execution.stream_liveness_timeout_secs.unwrap_or(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS as i64),
        "provider_idle_timeout_secs": execution.provider_idle_timeout_secs.unwrap_or(DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS as i64),
        "deadline_duration_secs": execution.deadline_duration_secs.unwrap_or(DEFAULT_DEADLINE_DURATION_SECS as i64),
        "retry_policy_id": execution.retry_policy_id,
        "meaning": "Unset fields use the defaults shown. deadline_duration_secs caps elapsed time per request, including model and tool work; provider_idle_timeout_secs caps provider silence. max_total_tokens null means unlimited; retry_policy_id null uses the request-origin retry policy. stream_batch_ms batches persistence; stream_liveness_timeout_secs is the renewed execution lease, independent of provider output."
    }))
}

impl SelfConfigCore {
    pub(crate) async fn read_effective_config(
        &self,
        categories: &BTreeSet<String>,
        no_lockout: bool,
        preview: bool,
    ) -> Result<Value> {
        ConfigAccess::transact_local(
            self.node(),
            Some(self.identity()?),
            "self_config.read",
            move |txn| {
                Box::pin(
                    async move { self.read_in_txn(txn, categories, no_lockout, preview).await },
                )
            },
        )
        .await
    }

    async fn read_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        categories: &BTreeSet<String>,
        no_lockout: bool,
        preview: bool,
    ) -> Result<Value> {
        let anchor = self.load_agent_anchor(txn).await?;
        let mut documents = serde_json::Map::new();
        for (target, field) in [
            (SelfConfigTarget::Tools, "tools_id"),
            (SelfConfigTarget::Compaction, "compaction_id"),
            (SelfConfigTarget::InferenceBackend, "backend_id"),
            (SelfConfigTarget::InferenceSampling, "sampling_id"),
            (SelfConfigTarget::InferenceExecution, "execution_id"),
            (SelfConfigTarget::InferenceRetryPolicy, "retry_policy_id"),
        ] {
            if let Some(id) = anchor.ref_id(field) {
                if let Some((_, mut doc)) =
                    read_owned_doc(txn, target, self.node_did(), &id).await?
                {
                    if target == SelfConfigTarget::InferenceBackend
                        && doc
                            .get("auth")
                            .and_then(|auth| auth.get("kind"))
                            .and_then(Value::as_str)
                            == Some("api_key")
                    {
                        doc.insert("auth".into(), json!({"kind":"api_key", "key":"[redacted]"}));
                    }
                    documents.insert(target.collection_name().into(), Value::Object(doc));
                }
            }
        }
        let tools = documents.get(SelfConfigTarget::Tools.collection_name());
        let mut canonical_tools = tools
            .cloned()
            .map(serde_json::from_value::<crate::document_config::Tools>)
            .transpose()?;
        let selection = canonical_tools
            .as_ref()
            .map(crate::tool_surface::ResolvedToolSelection::from_document)
            .transpose()?;
        let lsp_selected = selection.as_ref().is_some_and(|s| s.enable_lsp);
        let graph_selected = selection.as_ref().is_some_and(|s| s.enable_graph_tools);
        let configured_network_mode = canonical_tools
            .as_ref()
            .and_then(|tools| tools.host.as_ref())
            .and_then(|host| host.bash.as_ref())
            .and_then(|bash| bash.network_mode)
            .unwrap_or(crate::toolset::CommandNetworkMode::Inherit);
        let effective_network_mode = selection
            .as_ref()
            .and_then(|selection| selection.command_policy.as_ref())
            .map(|policy| policy.network_mode)
            .unwrap_or(configured_network_mode);
        let network_enforcement = selection
            .as_ref()
            .and_then(|selection| selection.command_policy.as_ref())
            .map(crate::toolset::CommandExecutionPolicy::network_enforcement_disclosure)
            .unwrap_or("no host command policy is active");
        let requested_file_mode = tools
            .and_then(|tools| tools.pointer("/host/files/mode"))
            .and_then(Value::as_str)
            .map(crate::tool_surface::FileToolMode::parse)
            .transpose()?
            .unwrap_or_default();
        let requested_bash_mode = tools
            .and_then(|tools| tools.pointer("/host/bash/mode"))
            .and_then(Value::as_str)
            .map(crate::tool_surface::BashMode::parse)
            .transpose()?
            .unwrap_or_default();
        let process_ceiling = self.process_ceiling();
        let effective_file_mode = requested_file_mode.meet(process_ceiling.file_mode);
        let effective_bash_mode = requested_bash_mode.meet(process_ceiling.bash_mode);
        let effective_root = if effective_file_mode != crate::tool_surface::FileToolMode::Off
            || effective_bash_mode != crate::tool_surface::BashMode::Off
        {
            let policy = crate::tool_surface::load_workspace_root_policy_in_txn(
                txn,
                process_ceiling.root.as_deref(),
            )
            .await?;
            if let Some(tools) = canonical_tools.as_mut() {
                crate::tool_surface::canonicalize_tools_root(tools, &policy)?;
            }
            let configured_root = canonical_tools
                .as_ref()
                .and_then(|tools| tools.host.as_ref())
                .and_then(|host| host.root.as_deref())
                .filter(|root| !root.trim().is_empty());
            crate::tool_surface::resolve_effective_tool_root(
                self.agent_id(),
                configured_root.map(Path::new),
                process_ceiling.root.as_deref(),
            )?
        } else {
            None
        };
        let configured_root = canonical_tools
            .as_ref()
            .and_then(|tools| tools.host.as_ref())
            .and_then(|host| host.root.as_deref())
            .filter(|root| !root.trim().is_empty());
        let mut skills = Vec::new();
        if let Some(ids) = anchor.context.get("skill_ids").and_then(Value::as_array) {
            for id in ids {
                let id = id.as_str().context("skill ID must be a string")?;
                if let Some((_, skill)) = crate::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Skill,
                    self.node_did(),
                    id,
                )
                .await?
                {
                    skills.push(skill);
                }
            }
        }
        let mut automation = serde_json::Map::new();
        let mut task_ids = BTreeSet::new();
        let mut schedule_ids = BTreeSet::new();
        let mut event_source_ids = BTreeSet::new();
        for target in [
            SelfConfigTarget::Task,
            SelfConfigTarget::Trigger,
            SelfConfigTarget::Schedule,
            SelfConfigTarget::EventSource,
        ] {
            let (fields, _) = config_projection(target.collection(), None)?;
            let owner = escape_graphql_string(self.node_did());
            let response = txn
                .execute(&format!(
                    "{{ {}(filter: {{node_did: {{_eq: \"{owner}\"}}}}) {{ {} }} }}",
                    target.collection_name(),
                    fields.join(" "),
                ))
                .await?;
            let rows = response
                .get("data")
                .and_then(|data| data.get(target.collection_name()))
                .and_then(Value::as_array)
                .context("configuration query missing rows")?;
            let mut selected = Vec::new();
            let mut identities = BTreeSet::new();
            for row in rows {
                let id = row
                    .get(target.unique_field())
                    .and_then(Value::as_str)
                    .context("configuration ID missing")?;
                anyhow::ensure!(
                    identities.insert(id),
                    "ambiguous scoped configuration identity"
                );
                let include = match target {
                    SelfConfigTarget::Task => {
                        let owned =
                            row.get("agent_id").and_then(Value::as_str) == Some(self.agent_id());
                        if owned {
                            task_ids.insert(id.to_owned());
                        }
                        owned
                    }
                    SelfConfigTarget::Trigger => {
                        let owned = row
                            .get("task_id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| task_ids.contains(id));
                        if owned {
                            if let Some(source) = row.get("source") {
                                if let Some(id) = source.get("schedule_id").and_then(Value::as_str)
                                {
                                    schedule_ids.insert(id.to_owned());
                                }
                                if let Some(id) =
                                    source.get("event_source_id").and_then(Value::as_str)
                                {
                                    event_source_ids.insert(id.to_owned());
                                }
                            }
                        }
                        owned
                    }
                    SelfConfigTarget::Schedule => schedule_ids.contains(id),
                    SelfConfigTarget::EventSource => event_source_ids.contains(id),
                    _ => unreachable!("automation target"),
                };
                if include {
                    selected.push(row.clone());
                }
            }
            automation.insert(target.collection_name().into(), Value::Array(selected));
        }
        Ok(json!({
            "node_did": self.node_did(), "agent_id": self.agent_id(),
            "agent": anchor.doc, "context": anchor.context, "inference_profile": anchor.profile,
            "documents": documents, "skills": skills, "automation": automation,
            "execution_settings": execution_settings(documents.get("InferenceExecution"))?,
            "self_config": {"categories": categories, "no_lockout": no_lockout, "preview": preview},
            "tool_grants": {
                "configured": { "lsp": lsp_selected, "native_graph_tools": graph_selected, "network_mode": configured_network_mode },
                "confirmed_by": "canonical Tools selection decoded from durable configuration",
                "activation": "Applies after reconciliation to later dispatched requests. Tool registration and successful execution must be tested in the working agent.",
                "lsp_readiness": "Selection does not prove a language server is installed, started, or indexed.",
                "graph_readiness": "Selection does not install a pack or grant graph caller admission. Use native list_graphs/run_graph on this node; do not adopt another runtime home or rebuild a CLI.",
            },
            "runtime_effective": {
                "meaning": "agent_narrowing is saved permission; effective also applies this process's ceiling. Save lasting role restrictions in Tools even when this process already blocks access. Use tools edit with options.agent to select the role.",
                "process_ceiling": process_ceiling,
                "agent_narrowing": {
                    "requested_file_mode": requested_file_mode,
                    "requested_bash_mode": requested_bash_mode,
                    "configured_root": configured_root,
                },
                "effective": {
                    "file_mode": effective_file_mode,
                    "bash_mode": effective_bash_mode,
                    "root": effective_root,
                    "network_mode": effective_network_mode,
                    "network_enforcement": network_enforcement,
                },
                "confirmed_by": "resolved host policy; sandbox availability and command enforcement are checked at execution time",
            },
            "effect_timing": EFFECT_TIMING_NOTE,
        }))
    }

    pub(crate) async fn task_owned(
        &self,
        txn: &ConfigApplyTxn<'_>,
        _anchor: &AgentAnchor,
        task_id: &str,
    ) -> Result<bool> {
        Ok(
            read_owned_doc(txn, SelfConfigTarget::Task, self.node_did(), task_id)
                .await?
                .is_some_and(|(_, task)| {
                    task.get("agent_id").and_then(Value::as_str) == Some(self.agent_id())
                }),
        )
    }
}
