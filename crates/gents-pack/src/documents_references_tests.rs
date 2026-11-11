use std::collections::BTreeSet;

use gents_documents::{config_projection, references_of};
use gents_store::collection::Collection;
use serde_json::{json, Value};

/// Identifier-shaped fields that do not name a same-owner configuration
/// document, so no reference consumer selects or grants through them.
const NOT_REFERENCES: &[(Collection, &str)] = &[
    // Removed EthTool collection; read and dropped from rc0 documents.
    (Collection::Tools, "integrations.eth_tool_ids"),
    (Collection::AgentTarget, "target_node_did"),
    (Collection::InferenceBackend, "auth.access_key_id"),
    (Collection::InferenceBackend, "claude_platform.workspace_id"),
    (Collection::ToolServiceRegistry, "send_node_did"),
    (Collection::ProjectionAcpBinding, "policy_id"),
    (Collection::ProjectionAcpBinding, "previous_policy_id"),
    (Collection::ProjectionAcpBinding, "projection_id"),
    (Collection::ProjectionAcpBinding, "staged_policy_id"),
    (Collection::Task, "hooks.hook_id"),
    (Collection::CallbackModule, "signer_did"),
    (Collection::EvalDefinition, "cases.case_id"),
    (Collection::EvalDefinition, "cases.stages.stage_id"),
];

fn identifier_paths(
    schema: &Value,
    definitions: &serde_json::Map<String, Value>,
    prefix: &str,
    visiting: &mut Vec<String>,
    paths: &mut BTreeSet<String>,
) {
    if let Some(name) = schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
    {
        if !visiting.iter().any(|seen| seen == name) {
            visiting.push(name.to_owned());
            if let Some(definition) = definitions.get(name) {
                identifier_paths(definition, definitions, prefix, visiting, paths);
            }
            visiting.pop();
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        for branch in schema
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            identifier_paths(branch, definitions, prefix, visiting, paths);
        }
    }
    if let Some(items) = schema.get("items") {
        identifier_paths(items, definitions, prefix, visiting, paths);
    }
    for (name, property) in schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        if ["_id", "_ids", "_did"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        {
            paths.insert(path.clone());
        }
        identifier_paths(property, definitions, &path, visiting, paths);
    }
}

#[test]
fn every_canonical_reference_field_reaches_reference_consumers() {
    let samples = [
        (
            Collection::Node,
            json!({"node_did":"owner", "default_agent_did":"agent"}),
        ),
        (
            Collection::Agent,
            json!({"node_did":"owner", "agent_did":"agent", "creation_nonce":"nonce",
                "context_id":"context", "inference_profile_id":"profile"}),
        ),
        (
            Collection::AgentContext,
            json!({"node_did":"owner", "context_id":"context", "tools_id":"tools",
                "compaction_id":"compaction", "skill_ids":["skill"]}),
        ),
        (
            Collection::Compaction,
            json!({"node_did":"owner", "compaction_id":"compaction",
                "inference_profile_id":"profile"}),
        ),
        (
            Collection::Tools,
            json!({"node_did":"owner", "tools_id":"tools",
                "remote":{"services":[{"mcp_service_id":"service"}]},
                "agents":{"target_ids":["target"]},
                "datastore":{"datastore_tool_surface_ids":["surface"]}}),
        ),
        (
            Collection::AgentTarget,
            json!({"node_did":"owner", "target_id":"target", "target_node_did":"owner",
                "agent_did":"agent", "name":"target"}),
        ),
        (
            Collection::InferenceProfile,
            json!({"node_did":"owner", "profile_id":"profile", "backend_id":"backend",
                "model_name":"model", "sampling_id":"sampling", "execution_id":"execution"}),
        ),
        (
            Collection::InferenceExecution,
            json!({"node_did":"owner", "execution_id":"execution", "retry_policy_id":"retry"}),
        ),
        (
            Collection::ProjectionAcpBinding,
            json!({"node_did":"owner", "binding_id":"binding", "agent_did":"agent",
                "policy_id":"policy"}),
        ),
        (
            Collection::Task,
            json!({"node_did":"owner", "task_id":"task", "agent_did":"agent",
                "prompt_template":"work"}),
        ),
        (
            Collection::Trigger,
            json!({"node_did":"owner", "trigger_id":"scheduled", "task_id":"task",
                "source":{"kind":"schedule", "schedule_id":"schedule"}}),
        ),
        (
            Collection::Trigger,
            json!({"node_did":"owner", "trigger_id":"evented", "task_id":"task",
                "source":{"kind":"event", "event_source_id":"source"}}),
        ),
        (
            Collection::Callback,
            json!({"node_did":"owner", "callback_id":"callback",
                "handler":{"kind":"module", "module_id":"module", "workspace_agents":{
                    "writer_agent_did":"writer", "integrator_agent_did":"integrator",
                    "readers":[{"agent_did":"reader"}]}}}),
        ),
        (
            Collection::CallbackBinding,
            json!({"node_did":"owner", "binding_id":"binding", "agent_did":"agent",
                "event_source_id":"source", "callback_id":"callback", "input_fields":["name"]}),
        ),
        (
            Collection::Task,
            json!({"node_did":"owner", "task_id":"task", "agent_did":"agent",
                "prompt_template":"work", "emit_outcome":true,
                "outcome_readers":[{"agent_did":"reader"}]}),
        ),
        (
            Collection::RepositoryPlacement,
            json!({"node_did":"owner", "repository_id":"repository", "host_path":"/repo",
                "readers":[{"agent_did":"reader"}]}),
        ),
    ];
    let mut reported = BTreeSet::new();
    for (collection, sample) in samples {
        let value = config_projection(collection, Some(&sample))
            .unwrap()
            .1
            .unwrap();
        for reference in references_of(collection, &value, "owner").unwrap() {
            assert!(
                !reference.id.is_empty(),
                "{collection:?} {}",
                reference.field
            );
            reported.insert((collection, reference.field.to_owned()));
        }
    }
    let mut declared = BTreeSet::new();
    for collection in Collection::ALL {
        let schema =
            serde_json::to_value(crate::pack::authored_document_schema(collection)).unwrap();
        let definitions = schema
            .get("$defs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut paths = BTreeSet::new();
        identifier_paths(&schema, &definitions, "", &mut Vec::new(), &mut paths);
        for path in paths {
            if path != collection.unique_field()
                && path != "node_did"
                && !NOT_REFERENCES.contains(&(collection, path.as_str()))
            {
                declared.insert((collection, path));
            }
        }
    }
    assert_eq!(
        declared, reported,
        "classify each identifier field as a reference in references_of or in NOT_REFERENCES"
    );
}
