use super::ops::{read_owned_doc, AgentAnchor, SelfConfigCore, EFFECT_TIMING_NOTE};
use anyhow::{Context, Result};
use gents_documents::patch::SelfConfigTarget;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

pub fn execution_settings(document: Option<&Value>) -> Result<Value> {
    use gents_documents::inference_defaults::{
        DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
    };
    use gents_inference::resolved::*;
    let mut document = document.cloned().unwrap_or_else(|| {
        json!({
            "node_did": "", "execution_id": ""
        })
    });
    if let Some(object) = document.as_object_mut() {
        object.remove("_docID");
    }
    let execution: gents_documents::InferenceExecution = serde_json::from_value(document)?;
    execution.validate()?;
    Ok(json!({
        "max_turns": execution.max_turns.unwrap_or(DEFAULT_MAX_TURNS as i64),
        "max_total_tokens": execution.max_total_tokens,
        "stream_batch_ms": execution.stream_batch_ms.unwrap_or(DEFAULT_STREAM_BATCH_MS as i64),
        "stream_liveness_timeout_secs": execution.stream_liveness_timeout_secs.unwrap_or(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS as i64),
        "provider_idle_timeout_secs": execution.provider_idle_timeout_secs.unwrap_or(DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS as i64),
        "deadline_duration_secs": execution.deadline_duration_secs.unwrap_or(DEFAULT_DEADLINE_DURATION_SECS as i64),
        "retry_policy_id": execution.retry_policy_id,
    }))
}

impl SelfConfigCore {
    pub async fn read_effective_config(
        &self,
        categories: &BTreeSet<String>,
        no_lockout: bool,
        preview: bool,
    ) -> Result<Value> {
        let node = self.node_handle();
        ConfigAccess::transact_local_readonly(
            &node,
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
            .map(serde_json::from_value::<gents_documents::Tools>)
            .transpose()?;
        let selection = canonical_tools
            .as_ref()
            .map(crate::surface::ResolvedToolSelection::from_document)
            .transpose()?;
        let lsp_selected = selection.as_ref().is_some_and(|s| s.enable_lsp);
        let configured_network_mode = canonical_tools
            .as_ref()
            .and_then(|tools| tools.host.as_ref())
            .and_then(|host| host.bash.as_ref())
            .and_then(|bash| bash.network_mode)
            .unwrap_or(crate::native::CommandNetworkMode::Inherit);
        let effective_network_mode = selection
            .as_ref()
            .and_then(|selection| selection.command_policy.as_ref())
            .map(crate::native::CommandExecutionPolicy::effective_network_mode)
            .unwrap_or(configured_network_mode);
        let network_enforcement = selection
            .as_ref()
            .and_then(|selection| selection.command_policy.as_ref())
            .map(crate::native::CommandExecutionPolicy::network_enforcement_disclosure)
            .unwrap_or("no host command policy is active");
        let requested_file_mode = tools
            .and_then(|tools| tools.pointer("/host/files/mode"))
            .and_then(Value::as_str)
            .map(gents_documents::FileToolMode::parse)
            .transpose()?
            .unwrap_or_default();
        let requested_bash_mode = tools
            .and_then(|tools| tools.pointer("/host/bash/mode"))
            .and_then(Value::as_str)
            .map(gents_documents::BashMode::parse)
            .transpose()?
            .unwrap_or_default();
        let process_ceiling = self.process_ceiling();
        let effective_file_mode = requested_file_mode.meet(process_ceiling.file_mode);
        let effective_bash_mode = requested_bash_mode.meet(process_ceiling.bash_mode);
        let mut root_access = Value::Null;
        let effective_root = if effective_file_mode != gents_documents::FileToolMode::Off
            || effective_bash_mode != gents_documents::BashMode::Off
        {
            let policy = crate::surface::load_workspace_root_policy_in_txn(
                txn,
                process_ceiling.root.as_deref(),
                self.node_did(),
            )
            .await?;
            // The root policy is read as the caller; another Agent's own access
            // to it is a relationship only a User grants and the caller cannot see.
            let foreign = self.identity()?.as_str() != self.agent_did();
            if let Some(tools) = canonical_tools.as_mut() {
                let stored_root = tools.host.as_ref().and_then(|host| host.root.clone());
                crate::surface::canonicalize_tools_root(tools, &policy, stored_root.as_deref())?;
            }
            let configured_root = canonical_tools
                .as_ref()
                .and_then(|tools| tools.host.as_ref())
                .and_then(|host| host.root.as_deref())
                .filter(|root| !root.trim().is_empty());
            let effective = crate::surface::resolve_effective_tool_root(
                self.agent_did(),
                configured_root.map(Path::new),
                process_ceiling.root.as_deref(),
            )?;
            if let (true, true, Some(root)) = (foreign, policy.configured, effective.as_deref()) {
                let published =
                    crate::surface::published_root_covering_in_txn(txn, self.node_did(), root)
                        .await?;
                root_access = crate::surface::reader_grant_step(
                    self.agent_did(),
                    published.as_deref(),
                    false,
                );
            }
            effective
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
                if let Some((_, skill)) = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Skill,
                    self.node_did(),
                    id,
                )
                .await?
                {
                    skills.push(skill);
                }
            }
        }
        let inventory =
            gents_documents::views::tasks::inventory(txn, self.node_did(), true, None).await?;
        let automation = json!({
            "Task": inventory.tasks,
            "Trigger": inventory.triggers,
            "Schedule": inventory.schedules,
            "EventSource": inventory.event_sources,
        });
        let mut effective = json!({
            "node_did": self.node_did(), "agent_did": self.agent_did(),
            "agent": anchor.doc, "context": anchor.context, "inference_profile": anchor.profile,
            "documents": documents, "skills": skills, "automation": automation,
            "execution_settings": execution_settings(documents.get("InferenceExecution"))?,
            "self_config": {"categories": categories, "no_lockout": no_lockout, "preview": preview},
            "tool_grants": {
                "configured": { "lsp": lsp_selected, "network_mode": configured_network_mode },
                "confirmed_by": "canonical Tools selection decoded from durable configuration",
                "activation": "Applies after reconciliation to later dispatched requests. Tool registration and successful execution must be tested in the working agent.",
                "lsp_readiness": "Selection does not prove a language server is installed, started, or indexed.",
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
        });
        if !root_access.is_null() {
            effective["runtime_effective"]["root_access"] = root_access;
        }
        Ok(effective)
    }

    pub async fn task_owned(
        &self,
        txn: &ConfigApplyTxn<'_>,
        _anchor: &AgentAnchor,
        task_id: &str,
    ) -> Result<bool> {
        Ok(
            read_owned_doc(txn, SelfConfigTarget::Task, self.node_did(), task_id)
                .await?
                .is_some_and(|(_, task)| {
                    task.get("agent_did").and_then(Value::as_str) == Some(self.agent_did())
                }),
        )
    }
}
