use super::*;

pub(super) fn entry_examples() -> Value {
    use crate::document_config::{
        SurfaceToolDecl, WriteToolDecl, WriteToolField, WriteToolFieldFill,
    };
    let create = SurfaceToolDecl::Create(WriteToolDecl {
        notification: None,
        tool_name: "record_result".into(),
        collection: "WorkResult".into(),
        description: "Record the result for the current input".into(),
        fields: vec![
            WriteToolField {
                name: "result".into(),
                required: true,
                fill: None,
            },
            WriteToolField {
                name: "correlation".into(),
                required: false,
                fill: Some(WriteToolFieldFill::Correlation),
            },
        ],
        output_obligation: None,
    });
    json!({"entries": [create]})
}

impl ConfigCommandTool {
    pub(super) async fn datastore(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("tools")?;
        let preview = argv.first().is_some_and(|arg| arg == "preview");
        let argv = if preview { &argv[1..] } else { argv };
        let verb = argv
            .first()
            .map(String::as_str)
            .context("run config help datastore")?;
        let id = required_resource_id(argv.get(1), "SURFACE_ID")?;
        let target = SelfConfigTarget::DatastoreToolSurface;
        if verb == "get" && !preview {
            anyhow::ensure!(argv.len() == 2, "datastore get accepts only SURFACE_ID");
            return self.exact_read(target, id).await;
        }
        anyhow::ensure!(
            matches!(verb, "create" | "edit"),
            "run config help datastore; expected create or edit"
        );
        let mut request = ApplyRequest::new(target, parse_patch(&argv[2..], target)?);
        let surface_id = id.clone();
        request.resolve_unique = Box::new(move |_| Ok(surface_id.clone()));
        request.allow_create = verb == "create";
        request.require_create = verb == "create";
        let owner = self.agent_did.clone();
        request.on_create = Box::new(move |id, doc| {
            doc.insert("surface_id".into(), json!(id));
            doc.insert("agent_did".into(), json!(owner));
            Ok(())
        });
        // Protect Setup through indirect shared surface references in the same
        // transaction that publishes the edit. ACP remains the write authority.
        let protected_owner = escape_graphql_string(&self.agent_did);
        request.validate = Box::new(move |txn, _, _, merged| {
            let owner = protected_owner.clone();
            let surface_id = merged["surface_id"].as_str().unwrap_or_default().to_owned();
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        r#"{{
                    AgentBehavior(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{context_id tags}}
                    AgentContext(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{context_id tools_id}}
                    Tools(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{tools_id datastore}}
                }}"#
                    ))
                    .await?;
                let data = response
                    .get("data")
                    .context("missing surface reference data")?;
                let rows = |name: &str| -> Result<&Vec<Value>> {
                    data[name]
                        .as_array()
                        .with_context(|| format!("missing {name} references"))
                };
                for behavior in rows("AgentBehavior")? {
                    if !behavior["tags"].as_array().is_some_and(|tags| {
                        tags.iter()
                            .any(|tag| tag == crate::agent::persona_ops::SETUP_STEWARD_BEHAVIOR_TAG)
                    }) {
                        continue;
                    }
                    for context in rows("AgentContext")?
                        .iter()
                        .filter(|row| row["context_id"] == behavior["context_id"])
                    {
                        for tools in rows("Tools")?
                            .iter()
                            .filter(|row| row["tools_id"] == context["tools_id"])
                        {
                            anyhow::ensure!(
                                !tools["datastore"]["datastore_tool_surface_ids"].as_array()
                                    .is_some_and(|ids| ids.iter().any(|id| id == &surface_id)),
                                "surface is referenced by protected Setup; create a separate surface for the working behavior"
                            );
                        }
                    }
                }
                Ok(())
            })
        });
        self.patch(
            &self.core,
            if preview { "preview" } else { "edit" },
            request,
        )
        .await
    }
}
