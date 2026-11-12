use super::*;

impl ConfigCommandTool {
    pub(super) async fn patch(
        &self,
        core: &SelfConfigCore,
        verb: &str,
        request: ApplyRequest<'static>,
    ) -> Result<String> {
        let stringified = stringified_field(&request.patch);
        let collection = request.target.collection_name();
        self.patch_outcome(core, verb, request)
            .await
            .map_err(|error| json_string_hint(collection, stringified, error))
    }

    /// A Skill or MCP service write through the handlers every client calls
    /// (Lean `DocumentOperations`), as the invoking Agent.
    pub(super) async fn document_write(
        &self,
        target: SelfConfigTarget,
        id: &str,
        mode: gents_protocol::operations::document::WriteMode,
        patch: SelfConfigPatch,
        preview: bool,
    ) -> Result<String> {
        use gents_documents::operations::{mcp_service, skill, OperationFailure};
        use gents_protocol::operations::document::DocumentOperationError;
        if preview {
            anyhow::ensure!(self.preview, "preview is not granted for this agent");
        } else {
            self.execution.enter_mutation();
        }
        let access = self.core.access()?;
        let policy = gents_inference::resolved::ProviderProfilePolicy;
        let (resource, result) = match target {
            SelfConfigTarget::Skill => (
                "skill",
                skill::save(
                    &access,
                    &skill::SkillWrite {
                        owner: &self.node_did,
                        skill_id: id,
                        mode,
                        patch: &patch,
                        node_binding: self.core.node_binding(),
                    },
                    &policy,
                    !preview,
                )
                .await
                .map_err(|failure| {
                    failure.map(|error| match error {
                        gents_protocol::operations::skill::SkillOperationError::Document(error) => {
                            error
                        }
                        other => DocumentOperationError::Passthrough(other.into()),
                    })
                }),
            ),
            SelfConfigTarget::ToolServiceRegistry => (
                "mcp-service",
                mcp_service::save(
                    &access,
                    &mcp_service::McpServiceWrite {
                        owner: &self.node_did,
                        service_id: id,
                        mode,
                        patch: &patch,
                        node_binding: self.core.node_binding(),
                    },
                    &policy,
                    !preview,
                )
                .await
                .map_err(|failure| {
                    failure.map(|error| {
                        match error {
                        gents_protocol::operations::mcp_service::McpServiceOperationError::Document(
                            error,
                        ) => error,
                        other => DocumentOperationError::Passthrough(other.into()),
                    }
                    })
                }),
            ),
            other => bail!(
                "{} is not written through the document handlers",
                other.collection_name()
            ),
        };
        let stringified = stringified_field(&patch);
        let receipt = result.map_err(|failure| match failure {
            OperationFailure::Operation(DocumentOperationError::Missing { id, .. }) => {
                super::ops::MissingConfigDocument { target, id }.into()
            }
            OperationFailure::Operation(error) => {
                let next_call = guidance::next_call(error.recovery(resource));
                json_string_hint(
                    target.collection_name(),
                    stringified,
                    CommandGuidance {
                        message: gents_protocol::error_chain::ErrorChain(&error).to_string(),
                        next_call,
                    }
                    .into(),
                )
            }
            OperationFailure::Other(error) => error,
        })?;
        outcome_text(&PatchOutcome::from_receipt(
            target,
            self.core.agent_did(),
            receipt,
        ))
    }

    async fn patch_outcome(
        &self,
        core: &SelfConfigCore,
        verb: &str,
        request: ApplyRequest<'static>,
    ) -> Result<String> {
        let help_verb = if request.require_create {
            "create"
        } else {
            "update"
        };
        let outcome = match verb {
            "preview" => {
                anyhow::ensure!(self.preview, "preview is not granted for this agent");
                core.preview(request).await
            }
            "edit" => {
                self.execution.enter_mutation();
                core.apply(request).await
            }
            _ => unreachable!("patch verb"),
        };
        match outcome {
            Ok(outcome) => outcome_text(&outcome),
            Err(ops::ApplyFailure::Other(error)) => Err(error),
            Err(ops::ApplyFailure::Inference { document, error }) => inference_refusal(
                verb == "preview",
                &error,
                error.recovery(document, help_verb),
            ),
        }
    }
}

/// A refused inference operation answers with what happened and the call its
/// error names. A refused preview is a successful answer carrying both.
pub(super) fn inference_refusal(
    preview: bool,
    error: &gents_protocol::operations::inference::InferenceOperationError,
    next: gents_protocol::operations::NextCall,
) -> Result<String> {
    let message = gents_protocol::error_chain::ErrorChain(error).to_string();
    let next_call = guidance::next_call(next);
    match error.refusal().filter(|_| preview) {
        Some(refusal) => gents_session::session::output_render::ordered! {
            "admitted": false,
            "committed": false,
            "refusal": refusal.as_str(),
            "message": message,
            "next_call": next_call,
        }
        .pretty(),
        None => Err(CommandGuidance { message, next_call }.into()),
    }
}

/// External premise: serde reports a JSON string decoded where a map or
/// sequence was expected only in its `invalid type: string` message.
fn serde_found_string(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("invalid type: string")
}

fn stringified_field(patch: &SelfConfigPatch) -> Option<String> {
    patch
        .iter()
        .find(|(_, value)| {
            value.as_ref().and_then(Value::as_str).is_some_and(|text| {
                serde_json::from_str::<Value>(text)
                    .is_ok_and(|parsed| parsed.is_object() || parsed.is_array())
            })
        })
        .map(|(field, _)| field.clone())
}

fn json_string_hint(
    collection: &str,
    stringified: Option<String>,
    error: anyhow::Error,
) -> anyhow::Error {
    match stringified.filter(|_| serde_found_string(&error)) {
        Some(field) => anyhow!(
            "{collection} field {field:?} holds a JSON string; send a native JSON object or array in set, not a string. ({error:#})"
        ),
        None => error,
    }
}
