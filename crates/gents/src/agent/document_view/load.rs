use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;

use crate::backend_registry::lookup_backend_at_cid;
use crate::document_config::{
    ensure_agent_principal, list_all_tool_selection_records, list_event_trigger_records,
    list_schedule_records, list_task_records, load_agent_behavior_at_cid,
    load_agent_principal_at_cid, load_inference_profile_at_cid, load_skill_at_cid,
    load_tool_selection_at_cid, load_tool_selection_record, AgentBehavior, AgentPrincipal,
    InferenceProfile, SkillDocument, ToolSelectionDocument,
};

use super::{DocumentRecord, DocumentRuntimeView, UnversionedDocumentRecord};

#[derive(Debug, Clone, serde::Deserialize)]
struct CommitParentRow {
    cid: String,
    #[serde(rename = "fieldName")]
    field_name: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct CompositeHeadEvidenceRow {
    cid: String,
    #[serde(default)]
    heads: Vec<CommitParentRow>,
}

fn sole_current_composite_head<'a>(
    rows: &'a [CompositeHeadEvidenceRow],
    collection: &str,
    doc_id: &str,
) -> Result<&'a CompositeHeadEvidenceRow> {
    use std::collections::HashSet;

    let nested_composite_cids = rows
        .iter()
        .flat_map(|row| row.heads.iter())
        .filter(|head| head.field_name.as_deref() == Some("_C"))
        .map(|head| head.cid.as_str())
        .collect::<HashSet<_>>();
    let current = rows
        .iter()
        .filter(|row| !nested_composite_cids.contains(row.cid.as_str()))
        .collect::<Vec<_>>();
    match current.as_slice() {
        [current] => Ok(*current),
        [] => anyhow::bail!("{collection} {doc_id} has no current composite head"),
        current => anyhow::bail!(
            "{collection} {doc_id} has {} current composite heads; refusing ambiguous provenance",
            current.len()
        ),
    }
}

async fn verified_current_version(
    node: &EmbeddedNode,
    collection: &str,
    doc_id: &str,
) -> Result<crate::SignedDocumentVersionRef> {
    let escaped_doc_id = crate::graphql::escape_graphql_string(doc_id);
    let query = format!(
        r#"query {{
            _commits(
                docID: ["{escaped_doc_id}"],
                filter: {{ fieldName: {{ _eq: "_C" }} }}
            ) {{
                cid
                heads {{ cid fieldName }}
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "querying {collection} {doc_id} composite evidence failed: {:?}",
            response.errors
        );
    }
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("_commits"))
        .cloned()
        .map(serde_json::from_value::<Vec<CompositeHeadEvidenceRow>>)
        .transpose()?
        .unwrap_or_default();
    let current = sole_current_composite_head(&rows, collection, doc_id)?;
    let signer_did = node
        .verified_block_signer_did(&current.cid)
        .await
        .map_err(|error| {
            anyhow!(
                "cryptographically verifying {collection} {doc_id} composite commit {}: {error}",
                current.cid
            )
        })?;
    if signer_did.trim().is_empty() {
        anyhow::bail!(
            "cryptographically verifying {collection} {doc_id} composite commit {} returned an empty signer DID",
            current.cid
        );
    }
    Ok(crate::SignedDocumentVersionRef::new(
        crate::DocumentVersionRef::new(doc_id, &current.cid),
        signer_did,
    ))
}

fn exact_record<T>(
    collection: &str,
    logical_id: String,
    expected_doc_id: &str,
    source: crate::SignedDocumentVersionRef,
    snapshot: Option<(String, T)>,
) -> Result<DocumentRecord<T>> {
    let (snapshot_doc_id, value) = snapshot.ok_or_else(|| {
        anyhow!(
            "{} composite commit {} did not reconstruct document {expected_doc_id}",
            collection,
            source.version.composite_commit_cid
        )
    })?;
    if snapshot_doc_id != expected_doc_id || source.version.doc_id != expected_doc_id {
        anyhow::bail!(
            "{} composite commit {} reconstructed document {}, expected {expected_doc_id}",
            collection,
            source.version.composite_commit_cid,
            snapshot_doc_id
        );
    }
    if logical_id.trim().is_empty() {
        anyhow::bail!("{collection} {expected_doc_id} has an empty logical id");
    }
    DocumentRecord::from_verified_fact(
        crate::ConfigFactRef::new(collection, logical_id, source),
        value,
    )
}

pub(super) async fn load_verified_principal_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<DocumentRecord<AgentPrincipal>> {
    let source = verified_current_version(node, "AgentPrincipal", doc_id).await?;
    let snapshot = load_agent_principal_at_cid(node, &source.version.composite_commit_cid).await?;
    let logical_id = snapshot
        .as_ref()
        .map(|(_, value)| value.agent_did.clone())
        .unwrap_or_default();
    exact_record("AgentPrincipal", logical_id, doc_id, source, snapshot)
}

pub(super) async fn load_verified_behavior_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<DocumentRecord<AgentBehavior>> {
    let source = verified_current_version(node, "AgentBehavior", doc_id).await?;
    let snapshot = load_agent_behavior_at_cid(node, &source.version.composite_commit_cid).await?;
    let logical_id = snapshot
        .as_ref()
        .map(|(_, value)| value.behavior_id.clone())
        .unwrap_or_default();
    exact_record("AgentBehavior", logical_id, doc_id, source, snapshot)
}

pub(super) async fn load_verified_tool_selection_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<DocumentRecord<ToolSelectionDocument>> {
    let source = verified_current_version(node, "ToolSelection", doc_id).await?;
    let snapshot = load_tool_selection_at_cid(node, &source.version.composite_commit_cid).await?;
    let logical_id = snapshot
        .as_ref()
        .map(|(_, value)| value.selection_id.clone())
        .unwrap_or_default();
    exact_record("ToolSelection", logical_id, doc_id, source, snapshot)
}

pub(super) async fn load_verified_profile_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<DocumentRecord<InferenceProfile>> {
    let source = verified_current_version(node, "InferenceProfile", doc_id).await?;
    let snapshot =
        load_inference_profile_at_cid(node, &source.version.composite_commit_cid).await?;
    let logical_id = snapshot
        .as_ref()
        .map(|(_, value)| value.profile_id.clone())
        .unwrap_or_default();
    exact_record("InferenceProfile", logical_id, doc_id, source, snapshot)
}

pub(super) async fn load_verified_backend_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<DocumentRecord<crate::backend_registry::InferenceBackend>> {
    let source = verified_current_version(node, "InferenceBackend", doc_id).await?;
    let snapshot = lookup_backend_at_cid(node, &source.version.composite_commit_cid).await?;
    let logical_id = snapshot
        .as_ref()
        .map(|(_, value)| value.backend_id.clone())
        .unwrap_or_default();
    exact_record("InferenceBackend", logical_id, doc_id, source, snapshot)
}

pub(super) async fn load_verified_skill_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<DocumentRecord<SkillDocument>> {
    let source = verified_current_version(node, "Skill", doc_id).await?;
    let snapshot = load_skill_at_cid(node, &source.version.composite_commit_cid).await?;
    let logical_id = snapshot
        .as_ref()
        .map(|(_, value)| value.skill_id.clone())
        .unwrap_or_default();
    exact_record("Skill", logical_id, doc_id, source, snapshot)
}

pub(super) fn insert_unique<T>(
    records: &mut std::collections::HashMap<String, DocumentRecord<T>>,
    record: DocumentRecord<T>,
) -> Result<()> {
    let logical_id = record.fact.logical_id.clone();
    if let Some(existing) = records.get(&logical_id) {
        anyhow::bail!(
            "{} logical id {} resolves to duplicate documents {} and {}",
            record.fact.collection,
            logical_id,
            existing.doc_id,
            record.doc_id
        );
    }
    records.insert(logical_id, record);
    Ok(())
}

pub(crate) async fn load_document_runtime_view(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<DocumentRuntimeView> {
    use std::collections::HashMap;

    ensure_agent_principal(node, agent_did).await?;
    let principal_rows =
        list_logical_doc_ids(node, "AgentPrincipal", "agent_did", agent_did).await?;
    let principal_doc_id = match principal_rows.as_slice() {
        [doc_id] => doc_id,
        [] => anyhow::bail!("AgentPrincipal {agent_did} was not persisted"),
        doc_ids => anyhow::bail!(
            "AgentPrincipal logical id {agent_did} resolves to {} duplicate documents: {}",
            doc_ids.len(),
            doc_ids.join(", ")
        ),
    };
    let principal = load_verified_principal_by_doc_id(node, principal_doc_id).await?;
    if principal.value.agent_did != agent_did {
        anyhow::bail!(
            "AgentPrincipal exact snapshot {} belongs to {}, expected {agent_did}",
            principal.doc_id,
            principal.value.agent_did
        );
    }

    let mut view = DocumentRuntimeView {
        principal,
        behaviors: HashMap::new(),
        skills: HashMap::new(),
        tool_selections: HashMap::new(),
        inference_profiles: HashMap::new(),
        backends: HashMap::new(),
        oauth_credentials: HashMap::new(),
        tasks: HashMap::new(),
        schedules: HashMap::new(),
        event_triggers: HashMap::new(),
    };

    for doc_id in list_logical_doc_ids(node, "ToolSelection", "agent_did", agent_did).await? {
        let record = load_verified_tool_selection_by_doc_id(node, &doc_id).await?;
        if record.value.agent_did != agent_did {
            anyhow::bail!(
                "ToolSelection exact snapshot {} belongs to {}, expected {agent_did}",
                record.doc_id,
                record.value.agent_did
            );
        }
        insert_unique(&mut view.tool_selections, record)?;
    }

    for doc_id in list_all_config_doc_ids(node, "InferenceProfile").await? {
        let record = load_verified_profile_by_doc_id(node, &doc_id).await?;
        insert_unique(&mut view.inference_profiles, record)?;
    }

    for doc_id in list_all_config_doc_ids(node, "InferenceBackend").await? {
        let record = load_verified_backend_by_doc_id(node, &doc_id).await?;
        insert_unique(&mut view.backends, record)?;
    }

    match crate::chatgpt_codex::list_oauth_credentials(node, agent_did).await {
        Ok(credentials) => {
            for credential in credentials {
                let doc_id = credential.doc_id.clone().unwrap_or_default();
                view.oauth_credentials.insert(
                    credential.credential_id.clone(),
                    UnversionedDocumentRecord {
                        doc_id,
                        value: credential,
                    },
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                agent_did = %agent_did,
                error = %error,
                "runtime document view could not load OAuthCredential documents; treating as none"
            );
        }
    }

    for doc_id in list_logical_doc_ids(node, "AgentBehavior", "agent_did", agent_did).await? {
        let record = load_verified_behavior_by_doc_id(node, &doc_id).await?;
        if record.value.agent_did != agent_did {
            anyhow::bail!(
                "AgentBehavior exact snapshot {} belongs to {}, expected {agent_did}",
                record.doc_id,
                record.value.agent_did
            );
        }
        insert_unique(&mut view.behaviors, record)?;
    }

    for doc_id in list_logical_doc_ids(node, "Skill", "agent_did", agent_did).await? {
        let record = load_verified_skill_by_doc_id(node, &doc_id).await?;
        if record.value.agent_did != agent_did {
            anyhow::bail!(
                "Skill exact snapshot {} belongs to {}, expected {agent_did}",
                record.doc_id,
                record.value.agent_did
            );
        }
        insert_unique(&mut view.skills, record)?;
    }

    for (doc_id, task) in list_task_records(node).await? {
        if task.task_id.trim().is_empty() {
            tracing::warn!(
                doc_id = %doc_id,
                "runtime document view skipped Task document with empty task_id"
            );
            continue;
        }
        let task_id = task.task_id.clone();
        view.tasks.insert(
            task_id,
            UnversionedDocumentRecord {
                doc_id,
                value: task,
            },
        );
    }

    for (doc_id, schedule) in list_schedule_records(node).await? {
        if schedule.schedule_id.trim().is_empty() {
            tracing::warn!(
                doc_id = %doc_id,
                "runtime document view skipped Schedule document with empty schedule_id"
            );
            continue;
        }
        let schedule_id = schedule.schedule_id.clone();
        view.schedules.insert(
            schedule_id,
            UnversionedDocumentRecord {
                doc_id,
                value: schedule,
            },
        );
    }

    for (doc_id, trigger) in list_event_trigger_records(node).await? {
        if trigger.trigger_id.trim().is_empty() {
            tracing::warn!(
                doc_id = %doc_id,
                "runtime document view ignoring EventTrigger with empty trigger_id"
            );
            continue;
        }
        view.event_triggers.insert(
            trigger.trigger_id.clone(),
            UnversionedDocumentRecord {
                doc_id,
                value: trigger,
            },
        );
    }

    hydrate_referenced_tool_selections(node, agent_did, &mut view).await?;

    Ok(view)
}

async fn list_logical_doc_ids(
    node: &EmbeddedNode,
    collection: &str,
    logical_field: &str,
    logical_id: &str,
) -> Result<Vec<String>> {
    let escaped_logical_id = crate::graphql::escape_graphql_string(logical_id);
    let query = format!(
        r#"{{
            {collection}(filter: {{ {logical_field}: {{ _eq: "{escaped_logical_id}" }} }}) {{
                _docID
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "checking duplicate {collection} logical id {logical_id} failed: {:?}",
            response.errors
        );
    }
    let mut doc_ids = response
        .data
        .as_ref()
        .and_then(|data| data.get(collection))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("_docID").and_then(serde_json::Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    doc_ids.sort();
    doc_ids.dedup();
    Ok(doc_ids)
}

async fn list_all_config_doc_ids(node: &EmbeddedNode, collection: &str) -> Result<Vec<String>> {
    let query = format!("{{ {collection} {{ _docID }} }}");
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "listing {collection} configuration documents failed: {:?}",
            response.errors
        );
    }
    let mut doc_ids = response
        .data
        .as_ref()
        .and_then(|data| data.get(collection))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("_docID").and_then(serde_json::Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    doc_ids.sort();
    doc_ids.dedup();
    Ok(doc_ids)
}

async fn hydrate_referenced_tool_selections(
    node: &EmbeddedNode,
    agent_did: &str,
    view: &mut DocumentRuntimeView,
) -> Result<()> {
    let missing_selection_ids = view
        .behaviors
        .values()
        .filter_map(|record| {
            record
                .value
                .tool_selection_id
                .as_deref()
                .and_then(super::snapshot::non_empty)
        })
        .filter(|selection_id| !view.tool_selections.contains_key(*selection_id))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    for selection_id in missing_selection_ids {
        let selection = match load_tool_selection_record(node, &selection_id).await? {
            Some(selection) => selection,
            None => match find_tool_selection_by_scan(node, &selection_id).await? {
                Some(selection) => {
                    tracing::warn!(
                        agent_did = %agent_did,
                        selection_id = %selection_id,
                        "runtime document view recovered referenced tool selection through unfiltered scan"
                    );
                    selection
                }
                None => continue,
            },
        };
        let (doc_id, _) = selection;
        let record = load_verified_tool_selection_by_doc_id(node, &doc_id).await?;
        if record.value.agent_did != agent_did {
            tracing::warn!(
                agent_did = %agent_did,
                selection_id = %selection_id,
                selection_agent_did = %record.value.agent_did,
                "runtime document view ignored referenced tool selection owned by another agent"
            );
            continue;
        }
        tracing::warn!(
            agent_did = %agent_did,
            selection_id = %selection_id,
            doc_id = %doc_id,
            "runtime document view recovered referenced tool selection missing from agent filter query"
        );
        insert_unique(&mut view.tool_selections, record)?;
    }

    Ok(())
}

async fn find_tool_selection_by_scan(
    node: &EmbeddedNode,
    selection_id: &str,
) -> Result<Option<(String, ToolSelectionDocument)>> {
    let rows = list_all_tool_selection_records(node).await?;
    let available = rows
        .iter()
        .take(8)
        .map(|(_, selection)| format!("{}@{}", selection.selection_id, selection.agent_did))
        .collect::<Vec<_>>()
        .join(", ");
    let available_count = rows.len();
    let found = rows
        .into_iter()
        .find(|(_, selection)| selection.selection_id == selection_id);
    if found.is_none() {
        tracing::warn!(
            selection_id = %selection_id,
            available_count = available_count,
            available = %available,
            "runtime document view scan did not find referenced tool selection"
        );
    }
    Ok(found)
}

#[cfg(test)]
mod provenance_tests {
    use super::*;

    fn head(cid: &str, parents: &[&str]) -> CompositeHeadEvidenceRow {
        CompositeHeadEvidenceRow {
            cid: cid.to_string(),
            heads: parents
                .iter()
                .map(|cid| CommitParentRow {
                    cid: (*cid).to_string(),
                    field_name: Some("_C".to_string()),
                })
                .collect(),
        }
    }

    #[test]
    fn exact_config_admission_requires_one_current_composite_head() {
        let empty = sole_current_composite_head(&[], "Skill", "doc-skill")
            .unwrap_err()
            .to_string();
        assert!(empty.contains("no current composite head"));

        let ambiguous_rows = [head("bafy-left", &[]), head("bafy-right", &[])];
        let ambiguous = sole_current_composite_head(&ambiguous_rows, "Skill", "doc-skill")
            .unwrap_err()
            .to_string();
        assert!(ambiguous.contains("2 current composite heads"));

        let linear_rows = [
            head("bafy-parent", &[]),
            head("bafy-child", &["bafy-parent"]),
        ];
        assert_eq!(
            sole_current_composite_head(&linear_rows, "Skill", "doc-skill")
                .unwrap()
                .cid,
            "bafy-child"
        );
    }
}
