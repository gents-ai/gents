//! Read the canonical config graph under the invoking principal's identity.
use super::ops::{read_owned_doc, BehaviorAnchor, SelfConfigCore, EFFECT_TIMING_NOTE};
use crate::config_client::patch::SelfConfigTarget;
use crate::config_client::{config_projection, ConfigAccess, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;

impl SelfConfigCore {
    pub(crate) async fn read_effective_config(
        &self,
        categories: &BTreeSet<String>,
        no_lockout: bool,
        dry_run: bool,
    ) -> Result<Value> {
        ConfigAccess::transact_local(
            self.node(),
            Some(self.identity()?),
            "self_config.read",
            move |txn| {
                Box::pin(
                    async move { self.read_in_txn(txn, categories, no_lockout, dry_run).await },
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
        dry_run: bool,
    ) -> Result<Value> {
        let anchor = self.load_behavior_anchor(txn).await?;
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
                    read_owned_doc(txn, target, self.agent_did(), &id).await?
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
        let mut skills = Vec::new();
        if let Some(ids) = anchor.context.get("skill_ids").and_then(Value::as_array) {
            for id in ids {
                let id = id.as_str().context("skill ID must be a string")?;
                if let Some((_, skill)) = crate::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Skill,
                    self.agent_did(),
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
            let owner = escape_graphql_string(self.agent_did());
            let response = txn
                .execute(&format!(
                    "{{ {}(filter: {{agent_did: {{_eq: \"{owner}\"}}}}) {{ {} }} }}",
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
                        let owned = row.get("behavior_id").and_then(Value::as_str)
                            == Some(self.behavior_id());
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
            "agent_did": self.agent_did(), "behavior_id": self.behavior_id(),
            "behavior": anchor.doc, "context": anchor.context, "inference_profile": anchor.profile,
            "documents": documents, "skills": skills, "automation": automation,
            "self_config": {"categories": categories, "no_lockout": no_lockout, "dry_run": dry_run},
            "effect_timing": EFFECT_TIMING_NOTE,
        }))
    }

    pub(crate) async fn task_owned(
        &self,
        txn: &ConfigApplyTxn<'_>,
        _anchor: &BehaviorAnchor,
        task_id: &str,
    ) -> Result<bool> {
        Ok(
            read_owned_doc(txn, SelfConfigTarget::Task, self.agent_did(), task_id)
                .await?
                .is_some_and(|(_, task)| {
                    task.get("behavior_id").and_then(Value::as_str) == Some(self.behavior_id())
                }),
        )
    }
}
