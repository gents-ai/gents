use std::collections::BTreeMap;

use anyhow::Result;

use crate::config_writes::ConfigAccess;

use super::super::DesiredStateManifest;

/// Apply validates trigger filter syntax and `doc.*` template fields via the
/// publication owner's rule against the same introspected schema; resolving
/// fields below the top level remains outside this contract.
pub(crate) async fn validate_manifest_against_live(
    manifest: &DesiredStateManifest,
    access: &ConfigAccess,
) -> Result<Vec<String>> {
    let mut errors = Vec::new();
    for source in &manifest.event_sources {
        let source_collection = source.source_collection.trim();
        let source_id = source.event_source_id.trim();
        if source_collection.is_empty() || source_id.is_empty() {
            continue;
        }
        if let Err(error) = gents::store::graphql::validate_collection_identifier(source_collection)
        {
            errors.push(format!(
                "event source {} has invalid source_collection {:?}: {}",
                source_id, source.source_collection, error
            ));
            continue;
        }

        if let Some(filter) = source.filter.as_deref().map(str::trim) {
            if !filter.is_empty() {
                if let Err(err) = gents::store::graphql::validate_graphql_filter_fragment(filter) {
                    errors.push(format!(
                        "event source {} filter is not a valid filter fragment: {}",
                        source_id, err
                    ));
                } else {
                    let probe = format!(
                        r#"query {{ {collection}(filter: {filter}, limit: 1) {{ _docID }} }}"#,
                        collection = source_collection,
                        filter = filter,
                    );
                    match access.execute(&probe).await {
                        Ok(_) => {}
                        Err(err) => {
                            errors.push(format!(
                                "event source {} filter syntax error: {}",
                                source_id, err
                            ));
                        }
                    }
                }
            }
        }

        let mut joined = Vec::new();
        for trigger in &manifest.triggers {
            if trigger.node_did != source.node_did
                || !matches!(&trigger.source,
                    gents::documents::TriggerSource::Event { event_source_id }
                    if event_source_id == &source.event_source_id)
            {
                continue;
            }
            let Some(task) = manifest
                .tasks
                .iter()
                .find(|task| task.node_did == trigger.node_did && task.task_id == trigger.task_id)
            else {
                continue;
            };
            joined.push((trigger, task));
        }
        let expected_count_field = source
            .group
            .as_ref()
            .and_then(|group| group.expected_count.as_ref())
            .and_then(|count| match count {
                gents::documents::EventGroupCount::SourceField { source_field } => {
                    Some(source_field.as_str())
                }
                gents::documents::EventGroupCount::Fixed(_) => None,
            });
        if source.correlation_field.is_none() && expected_count_field.is_none() {
            let mut walks_templates = false;
            for (trigger, task) in &joined {
                match gents::store::config_client::event_trigger_document_field_names(trigger, task)
                {
                    Ok(names) if !names.is_empty() => walks_templates = true,
                    Err(error) => errors.push(format!("{error:#}")),
                    Ok(_) => {}
                }
            }
            if !walks_templates {
                continue;
            }
        }

        let introspect =
            match gents::tools::builtin::defra_query::introspection_query(source_collection) {
                Ok(introspect) => introspect,
                Err(err) => {
                    errors.push(format!(
                        "event source {} has invalid source_collection {:?}: {}",
                        source_id, source.source_collection, err
                    ));
                    continue;
                }
            };
        let response = match access.execute(&introspect).await {
            Ok(response) => response,
            Err(err) => {
                errors.push(format!(
                    "event source {} introspection of source_collection {} failed: {}",
                    source_id, source_collection, err
                ));
                continue;
            }
        };
        let Some(schema) =
            gents::tools::builtin::defra_query::parse_collection_schema(response.get("data"))
        else {
            errors.push(format!(
                "event source {} references unknown source_collection {}",
                source_id, source_collection
            ));
            continue;
        };
        let declared: BTreeMap<String, gents::tools::builtin::defra_query::SchemaField> = schema
            .fields
            .into_iter()
            .map(|field| (field.name.clone(), field))
            .collect();
        for (trigger, task) in joined {
            if let Err(error) = gents::store::config_client::validate_event_trigger_document_fields(
                trigger, task, source, &declared,
            ) {
                errors.push(format!("{error:#}"));
            }
        }
    }

    Ok(errors)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::Result;
    use defra_node::EmbeddedNode;
    use serde_json::json;

    use super::*;

    const OWNER: &str = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";

    const PROBE_SDL: &str = r#"
        type LiveProbe {
            batch: String
        }
        type WorkspaceReceipt {
            workspace_id: String
        }
    "#;

    async fn probe_access(tempdir: &tempfile::TempDir) -> Result<ConfigAccess> {
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(tempdir.path().join("data"))
                .build()
                .await?,
        );
        let access = ConfigAccess::local(node);
        access.add_collection(PROBE_SDL).await?;
        Ok(access)
    }

    fn manifest(
        sources: serde_json::Value,
        tasks: serde_json::Value,
        triggers: serde_json::Value,
    ) -> Result<DesiredStateManifest> {
        Ok(serde_json::from_value(json!({
            "node": { "node_did": OWNER },
            "event_sources": sources,
            "tasks": tasks,
            "triggers": triggers,
        }))?)
    }

    fn source(id: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut source = json!({
            "node_did": OWNER,
            "event_source_id": id,
            "source_collection": "LiveProbe",
        });
        let object = source.as_object_mut().expect("source object");
        for (field, value) in extra.as_object().expect("source fields") {
            object.insert(field.clone(), value.clone());
        }
        source
    }

    fn receipt_source(id: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut source = source(id, extra);
        source["source_collection"] = json!("WorkspaceReceipt");
        source
    }

    fn receipt_manifest(
        source_extra: serde_json::Value,
        emit_outcome: bool,
    ) -> Result<DesiredStateManifest> {
        manifest(
            json!([receipt_source("reviewed", source_extra)]),
            json ! ([{ "node_did" : OWNER , "task_id" : "review" , "agent_id" : "beh" , "prompt_template" : "Review attempt {{ doc.attempt }} for {{ doc.workspace_id }}" , "emit_outcome" : emit_outcome , }]),
            json ! ([{ "node_did" : OWNER , "trigger_id" : "on-review" , "task_id" : "review" , "source" : { "kind" : "event" , "event_source_id" : "reviewed" } , }]),
        )
    }

    #[tokio::test]
    async fn refuses_native_route_fields_without_the_runtime_preconditions() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let access = probe_access(&tempdir).await?;
        let grouped = receipt_manifest(
            json ! ({ "correlation_field" : "workspace_id" , "group" : { "expected_count" : 2 } }),
            true,
        )?;
        let errors = validate_manifest_against_live(&grouped, &access).await?;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("trigger on-review prompt_template references doc.attempt")
                && errors[0].contains("WorkspaceReceipt has no field \"attempt\""),
            "{errors:?}"
        );
        let outcome_less = receipt_manifest(json ! ({ "event_kind" : "created" }), false)?;
        let errors = validate_manifest_against_live(&outcome_less, &access).await?;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("trigger on-review prompt_template references doc.attempt")
                && errors[0].contains("WorkspaceReceipt has no field \"attempt\""),
            "{errors:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn refuses_a_filter_the_probe_query_cannot_execute() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let access = probe_access(&tempdir).await?;
        let manifest = manifest(
            json!([source(
                "filtered",
                json ! ({ "filter" : "{undeclared: {_eq: \"x\"}}" })
            )]),
            json!([]),
            json!([]),
        )?;
        let errors = validate_manifest_against_live(&manifest, &access).await?;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("filter syntax error"),
            "a filter that names no declared field must fail the probe: {errors:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn accepts_native_route_template_fields_the_schema_does_not_declare() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let access = probe_access(&tempdir).await?;
        let manifest = receipt_manifest(json ! ({ "event_kind" : "created" }), true)?;
        let errors = validate_manifest_against_live(&manifest, &access).await?;
        assert_eq!(errors, Vec::<String>::new(), "{errors:?}");
        Ok(())
    }

    #[tokio::test]
    async fn refuses_a_template_that_does_not_parse_without_a_correlation_or_count() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let access = probe_access(&tempdir).await?;
        let manifest = manifest(
            json!([source("broken", json ! ({ "correlation_field" : "batch" }),)]),
            json ! ([{ "node_did" : OWNER , "task_id" : "summarize" , "agent_id" : "beh" , "prompt_template" : "Summarize {{ doc.x" , }]),
            json ! ([{ "node_did" : OWNER , "trigger_id" : "on-broken" , "task_id" : "summarize" , "source" : { "kind" : "event" , "event_source_id" : "broken" } , }]),
        )?;
        let errors = validate_manifest_against_live(&manifest, &access).await?;
        assert ! (errors . iter () . any (| error | error . contains ("template parse error")) , "an unparsable template must fail the pre-flight without a correlation or count probe: {errors:?}");
        Ok(())
    }

    #[tokio::test]
    async fn refuses_a_template_path_the_source_collection_does_not_declare() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let access = probe_access(&tempdir).await?;
        let manifest = manifest(
            json!([source("templated", json!({"correlation_field": "batch"}))]),
            json!([{
                "node_did": OWNER,
                "task_id": "summarize",
                "agent_id": "beh",
                "prompt_template": "Summarize {{ doc.nope }}"
            }]),
            json!([{
                "node_did": OWNER,
                "trigger_id": "on-templated",
                "task_id": "summarize",
                "source": {"kind": "event", "event_source_id": "templated"}
            }]),
        )?;
        let errors = validate_manifest_against_live(&manifest, &access).await?;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("references doc.nope")
                && errors[0].contains("LiveProbe has no field \"nope\""),
            "{errors:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn refuses_an_undeclared_session_id_template_field() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let access = probe_access(&tempdir).await?;
        let manifest = manifest(
            json!([source("templated", json!({}))]),
            json!([{
                "node_did": OWNER,
                "task_id": "summarize",
                "agent_id": "beh",
                "prompt_template": "Summarize"
            }]),
            json!([{
                "node_did": OWNER,
                "trigger_id": "on-templated",
                "task_id": "summarize",
                "session_id_template": "{{ doc.session }}",
                "source": {"kind": "event", "event_source_id": "templated"}
            }]),
        )?;
        let errors = validate_manifest_against_live(&manifest, &access).await?;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("session_id_template references doc.session")
                && errors[0].contains("LiveProbe has no field \"session\""),
            "{errors:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn refuses_a_source_collection_the_node_does_not_have() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let access = probe_access(&tempdir).await?;
        let mut missing = source("missing", json!({"correlation_field": "batch"}));
        missing["source_collection"] = json!("LiveProbeLater");
        let manifest = manifest(json!([missing]), json!([]), json!([]))?;
        let errors = validate_manifest_against_live(&manifest, &access).await?;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("references unknown source_collection LiveProbeLater"),
            "{errors:?}"
        );
        Ok(())
    }
}
