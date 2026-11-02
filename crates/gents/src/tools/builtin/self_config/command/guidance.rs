use super::*;

fn missing_reference_next_step(
    missing: &crate::documents::MissingReference,
    deleting: bool,
) -> String {
    let target = missing.target.graphql_type();
    let id = &missing.target_id;
    if deleting {
        return format!("Deletion would leave a broken reference to {target} {id:?}. Update the referring document to remove or redirect the reference, then retry deletion.");
    }
    if missing.target == crate::Collection::Skill {
        return format!("Skill {id:?} does not exist yet; create it with skill create or import it with skill import, then attach its ID with context update set.skill_ids.");
    }
    format!("{target} {id:?} does not exist yet; create it with its own resource command first, or reference an existing ID.")
}

impl ConfigCommandTool {
    pub(super) fn failure_guidance(
        &self,
        error: &anyhow::Error,
        argv: &[String],
        deleting: bool,
    ) -> (String, Value) {
        let missing = error.downcast_ref::<crate::documents::MissingReference>();
        let message = missing.map_or_else(
            || format!("{error:#}"),
            |missing| {
                format!(
                    "{} Cause: {error:#}",
                    missing_reference_next_step(missing, deleting)
                )
            },
        );
        if let Some(hint) = error.downcast_ref::<CommandGuidance>() {
            return (message, json!({"next_call":hint.next_call}));
        }
        if let Some(schema) =
            error.downcast_ref::<crate::store::config_client::OutcomeSourceSchemaError>()
        {
            let next = if schema.missing_handoff {
                json!({"argv":["collection","preview","update",schema.collection],"options":{"patch":[{"op":"add","path":format!("/{}/Fields/-",schema.collection),"value":{"Name":"handoff_id","Kind":"String"}}]}})
            } else {
                json!({"argv":["collection","get",schema.collection]})
            };
            return (message, json!({"tool":"schema","next_call":next}));
        }
        if let Some(missing) = error.downcast_ref::<super::ops::MissingConfigDocument>() {
            if let Some((resource, _)) = HELP_INDEX
                .iter()
                .find(|(r, _)| crud::resource_target(r) == Some(missing.target))
            {
                return (message, json!({"next_call":{"argv":[resource,"list"]}}));
            }
        }
        let resources = model_resources(&self.categories, self.core.held_grants().pack_install);
        if error.downcast_ref::<super::ops::MissingAgent>().is_some()
            && self.categories.contains("node")
        {
            return (message, json!({"next_call":{"argv":["agent","list"]}}));
        }
        if let Some(missing) = missing {
            let collection = if deleting {
                missing.collection
            } else {
                missing.target
            };
            if let Some(resource) = resources.iter().find(|resource| {
                crud::resource_target(resource)
                    .is_some_and(|target| target.collection() == collection)
            }) {
                let next = if deleting {
                    json!({"argv":[resource,"get"],"target_id":missing.id})
                } else {
                    json!({"argv":[resource,"list"]})
                };
                return (message, json!({"next_call":next}));
            }
        }
        if argv.first().is_some_and(|word| word == "schema") {
            return (
                message,
                json!({"tool":"schema","next_call":{"argv":["help"]}}),
            );
        }
        if matches!(argv.first().map(String::as_str), Some("pack"))
            && matches!(argv.get(1).map(String::as_str), Some("install" | "update"))
        {
            if let Some(package) = argv.get(2) {
                return (
                    message,
                    json!({"next_call":{"argv":["pack","preview",argv[1],package]}}),
                );
            }
        }
        let resource = argv.first().map(String::as_str).unwrap_or("help");
        let next = if resources
            .iter()
            .any(|name| name.split(' ').next() == Some(resource))
        {
            let verb = argv.iter().skip(1).find(|word| word.as_str() != "preview");
            match verb.map(String::as_str) {
                Some(verb @ ("create" | "update" | "edit"))
                    if crud::resource_target(resource).is_some() =>
                {
                    json!({"argv":[resource,verb,"--help"]})
                }
                _ => json!({"argv":["help",resource]}),
            }
        } else {
            json!({"argv":["help"]})
        };
        (message, json!({"next_call":next}))
    }
}
