use super::*;
use crate::config_client::{
    validate_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};

impl ConfigCommandTool {
    pub(super) async fn validate_saved(&self, argv: &[String]) -> Result<String> {
        self.ensure_behavior_catalog("validate", None)?;
        anyhow::ensure!(argv.is_empty(), "validate takes no parameters; it checks saved configuration for your authenticated principal");
        let identity = self.core.identity()?;
        let (counts, mut errors, selected_surfaces) = ConfigAccess::transact_local(
            &self.node,
            Some(identity),
            "self_config.validate",
            |txn| Box::pin(async move {
                let references = crate::ConfigReferences::load_in_txn(txn, &self.agent_did).await?;
                let mut counts = BTreeMap::<&str, usize>::new();
                let mut errors = Vec::new();
                for ((collection, id), document) in references.documents() {
                    *counts.entry(collection.graphql_type()).or_default() += 1;
                    if let Err(error) = references.validate_document(*collection, document) {
                        let mut diagnostic = json!({
                            "collection": collection.graphql_type(),
                            "id": id,
                            "error": format!("{error:#}")
                        });
                        if let Some(missing) = error.downcast_ref::<crate::document_config::MissingReference>() {
                            diagnostic["field"] = json!(missing.field);
                            diagnostic["missing"] = json!({"collection":missing.target.graphql_type(),"id":missing.target_id});
                        }
                        if let Some((resource, _)) = HELP_INDEX.iter().find(|(resource, _)| {
                            crud::resource_target(resource).is_some_and(|target| target.collection_name() == collection.graphql_type())
                                && model_resources(&self.categories, self.allow_pack_install).contains(resource)
                        }) {
                            diagnostic["inspect_with"] = json!({"argv":[resource,"get"],"target_id":id});
                        }
                        errors.push(diagnostic);
                    }
                }
                if errors.is_empty() {
                    // Revalidate the saved snapshot without publishing it. Marking every
                    // document as a candidate also runs the owner's affected-field checks.
                    let plan = DesiredStateApplyPlan::new(references.documents().map(|((collection, _), value)| DesiredStateApplyDocument {
                        collection: *collection, add: value.clone(), update: value.clone(),
                    }).collect())?;
                    if let Err(error) = validate_desired_state_plan(txn, &plan).await {
                        if crate::config_client::is_transaction_step_unavailable(&error) {
                            return Err(error);
                        }
                        errors.push(json!({"error":format!("{error:#}")}));
                    }
                }
                let selected: std::collections::BTreeSet<String> = references.documents()
                    .filter(|((collection, _), _)| *collection == crate::Collection::Tools)
                    .filter_map(|(_, value)| serde_json::from_value::<crate::document_config::Tools>(value.clone()).ok())
                    .flat_map(|tools| tools.datastore.and_then(|d| d.datastore_tool_surface_ids).unwrap_or_default())
                    .collect();
                let surfaces = references.documents()
                    .filter(|((collection, id), _)| *collection == crate::Collection::DatastoreToolSurface && selected.contains(id))
                    .filter_map(|(_, value)| serde_json::from_value::<crate::document_config::DatastoreToolSurfaceDocument>(value.clone()).ok())
                    .collect::<Vec<_>>();
                Ok((counts, errors, surfaces))
            }),
        ).await?;
        for surface in selected_surfaces {
            for entry in surface.entries.unwrap_or_default() {
                use crate::document_config::SurfaceToolDecl;
                let (tool_name, collection, result) = match entry {
                    SurfaceToolDecl::Create(decl) => {
                        let result = if decl.collection == crate::mailbox::MAILBOX_COLLECTION {
                            crate::mailbox::validate_mailbox_write_decl(&decl)
                        } else {
                            crate::defra_write::BoundedWriteTool::new(
                                self.node.clone(),
                                decl.clone(),
                            )
                            .ensure_well_formed()
                        };
                        (decl.tool_name, decl.collection, result)
                    }
                    SurfaceToolDecl::Query(decl) => {
                        let result = crate::defra_query::BoundedQueryTool::new(
                            self.node.clone(),
                            decl.clone(),
                        )
                        .validate_schema()
                        .await;
                        (decl.tool_name, decl.collection, result)
                    }
                };
                if let Err(error) = result {
                    errors.push(json!({
                        "collection":"DatastoreToolSurface", "id":surface.surface_id,
                        "tool_name":tool_name, "schema_collection":collection,
                        "error":format!("{error:#}"),
                        "inspect_with":{"argv":["datastore","get"],"target_id":surface.surface_id},
                        "inspect_schema_with":{"tool":"schema","args":{"argv":["collection","get"],"target_id":collection}},
                        "next":"Use datastore update to correct this entry or schema collection update to correct the existing collection. Preserve unrelated fields and entries, then run config validate again."
                    }));
                }
            }
        }
        ordered! {
            "valid": errors.is_empty(),
            "checked_documents": counts.values().sum::<usize>(),
            "collections": counts,
            "errors": errors,
            "scope": "Saved configuration for the authenticated principal: canonical fields, references, publication checks and selected datastore tools against current collection schemas. Config and schema observations are not one atomic snapshot. Does not test credentials, remote destinations, runtime execution or user intent.",
            "next": if errors.is_empty() { "Inspect behavior get to verify selections match the user's request; exercise tools to verify runtime behavior. Report what remains untested." } else { "Read the named objects, correct existing objects with resource update (create only missing objects), and run validate again before reporting completion." },
            "committed": false,
        }.pretty()
    }
}
