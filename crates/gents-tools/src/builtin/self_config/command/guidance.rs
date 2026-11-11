use super::*;
use agent_management::AgentApplyError;
use gents_protocol::operations::agent::NextCall;

/// The model tool's spelling of a declared next call.
pub(super) fn next_call(next: NextCall) -> Value {
    match next {
        NextCall::VerbHelp { resource, verb } => json!({"argv":[resource, verb, "--help"]}),
        NextCall::ResourceHelp { resource } => json!({"argv":["help", resource]}),
        NextCall::Inspect { resource } => json!({"argv":[resource, "get"]}),
        NextCall::List { resource } => json!({"argv":[resource, "list"]}),
        NextCall::Preview { resource, verb } => json!({"argv":[resource, "preview", verb]}),
    }
}

/// A refused agent operation answers with what happened and the recovery its
/// error declares for the requested operation. Another owner's failure passes
/// on unchanged to that owner's classifiers in `failure_guidance`.
pub(super) fn agent_failure(
    error: AgentApplyError,
    params: &ConfigureAgentParams,
) -> anyhow::Error {
    match error {
        AgentApplyError::Operation(error) => CommandGuidance {
            message: gents_protocol::error_chain::ErrorChain(&error).to_string(),
            next_call: next_call(agent_management::recovery_for(params, &error)),
        }
        .into(),
        AgentApplyError::Other(error) => error,
    }
}

fn missing_reference_next_step(
    missing: &gents_documents::MissingReference,
    deleting: bool,
) -> String {
    let target = missing.target.graphql_type();
    let id = &missing.target_id;
    if deleting {
        return format!("Deletion would leave a broken reference to {target} {id:?}. Update the referring document to remove or redirect the reference, then retry deletion.");
    }
    if missing.target == gents_store::collection::Collection::Skill {
        return format!("Skill {id:?} does not exist yet; create it with skill create or import it with skill import, then attach its ID with context update set.skill_ids.");
    }
    format!("{target} {id:?} is not among the documents you can read. Either it does not exist yet (create it with its own resource command first, or reference an existing ID), or another identity created it and you have no read access: a pack the node's User installed with the CLI leaves documents only the User reads, so list what you can read and ask the User to grant access or to let you install the pack yourself with pack install.")
}

impl ConfigCommandTool {
    pub(super) fn failure_guidance(
        &self,
        error: &anyhow::Error,
        argv: &[String],
        deleting: bool,
    ) -> (String, Value) {
        let missing = error.downcast_ref::<gents_documents::MissingReference>();
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
        if let Some(schema) = error.downcast_ref::<gents_documents::OutcomeSourceSchemaError>() {
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
