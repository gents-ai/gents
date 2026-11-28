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
    /// (Lean `DocumentOperations`), as the invoking Agent, with its attempt in
    /// the same transaction.
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
        let attempt = self.experience_attempt(!preview).await?;
        let node_did = self.core.node_did().to_owned();
        let request = match target {
            SelfConfigTarget::Skill => DocumentAttempt {
                write: DocumentWriteSpec::Skill(skill::SkillWrite {
                    owner: &self.node_did,
                    skill_id: id,
                    mode,
                    patch: &patch,
                    node_binding: self.core.node_binding(),
                }),
                commit: !preview,
                attempt: attempt.as_ref(),
                node_did: &node_did,
            },
            SelfConfigTarget::ToolServiceRegistry => DocumentAttempt {
                write: DocumentWriteSpec::Mcp(mcp_service::McpServiceWrite {
                    owner: &self.node_did,
                    service_id: id,
                    mode,
                    patch: &patch,
                    node_binding: self.core.node_binding(),
                }),
                commit: !preview,
                attempt: attempt.as_ref(),
                node_did: &node_did,
            },
            other => bail!(
                "{} is not written through the document handlers",
                other.collection_name()
            ),
        };
        let request = &request;
        let written = if preview {
            access
                .transact_readonly("self_config.document.preview", |txn| {
                    Box::pin(save_document_in_txn(txn, request))
                })
                .await
        } else {
            access
                .transact_write("self_config.document", |txn| {
                    Box::pin(save_document_in_txn(txn, request))
                })
                .await
        };
        let stringified = stringified_field(&patch);
        let resource = if target == SelfConfigTarget::Skill {
            "skill"
        } else {
            "mcp-service"
        };
        let (receipt, staged) = written.map_err(|failure| match failure {
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
        let mut outcome = PatchOutcome::from_receipt(target, self.core.agent_did(), receipt);
        outcome.experience = staged.map(|staged| {
            json!({
                "attempt_key": staged.attempt_key,
                "problem_key": staged.problem_key,
            })
        });
        if preview && self.experience.is_some() {
            outcome.experience = Some(json!("not recorded on preview"));
        }
        outcome_text(&outcome)
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
        let mut outcome = match verb {
            "preview" => {
                anyhow::ensure!(self.preview, "preview is not granted for this agent");
                core.preview(request).await
            }
            "edit" => {
                self.execution.enter_mutation();
                let attempt = self.experience_attempt(true).await?;
                core.apply_with_experience(request, attempt).await
            }
            _ => unreachable!("patch verb"),
        };
        if verb == "preview" && self.experience.is_some() {
            if let Ok(outcome) = &mut outcome {
                outcome.experience = Some(json!("not recorded on preview"));
            }
        }
        match outcome {
            Ok(outcome)
                if outcome.committed
                    && outcome.collection == SelfConfigTarget::Tools.collection_name() =>
            {
                let rebinding = crate::tools_rebinding::after_tools_commit(&self.envelope).await;
                outcome_text(&super::rebinding::applied(outcome, rebinding))
            }
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

async fn stage_write_attempt(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    commit: bool,
    attempt: Option<&crate::memory::AttemptRequest>,
    receipt: &gents_protocol::operations::document::DocumentReceipt,
) -> Result<
    Option<crate::memory::StagedAttempt>,
    gents_documents::operations::OperationFailure<
        gents_protocol::operations::document::DocumentOperationError,
    >,
> {
    let Some(attempt) = attempt.filter(|_| commit) else {
        return Ok(None);
    };
    let changes = receipt
        .changed
        .iter()
        .map(|change| gents_documents::patch::FieldDelta {
            field: change.field.clone(),
            from: change.from.clone(),
            to: change.to.clone(),
        })
        .collect::<Vec<_>>();
    let targets = [crate::memory::ExperienceTarget::from_changes(
        receipt.collection,
        &receipt.id,
        receipt.doc_id.clone(),
        &changes,
    )];
    let summary = crate::memory::change_summary(receipt.collection, &receipt.id, &changes);
    let staged = attempt
        .stage_in_txn(txn, node_did, &targets, &summary)
        .await
        .map_err(gents_documents::operations::OperationFailure::Other)?;
    Ok(Some(staged))
}

type DocumentFailure = gents_documents::operations::OperationFailure<
    gents_protocol::operations::document::DocumentOperationError,
>;
type SkillFailure = gents_documents::operations::OperationFailure<
    gents_protocol::operations::skill::SkillOperationError,
>;
type McpServiceFailure = gents_documents::operations::OperationFailure<
    gents_protocol::operations::mcp_service::McpServiceOperationError,
>;

enum DocumentWriteSpec<'a> {
    Skill(gents_documents::operations::skill::SkillWrite<'a>),
    Mcp(gents_documents::operations::mcp_service::McpServiceWrite<'a>),
}

struct DocumentAttempt<'a> {
    write: DocumentWriteSpec<'a>,
    commit: bool,
    attempt: Option<&'a crate::memory::AttemptRequest>,
    node_did: &'a str,
}

async fn save_document_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    request: &DocumentAttempt<'_>,
) -> Result<
    (
        gents_protocol::operations::document::DocumentReceipt,
        Option<crate::memory::StagedAttempt>,
    ),
    DocumentFailure,
> {
    let policy = gents_inference::resolved::ProviderProfilePolicy;
    let receipt = match &request.write {
        DocumentWriteSpec::Skill(write) => {
            gents_documents::operations::skill::save_in_txn(txn, write, &policy, request.commit)
                .await
                .map_err(skill_failure)?
        }
        DocumentWriteSpec::Mcp(write) => gents_documents::operations::mcp_service::save_in_txn(
            txn,
            write,
            &policy,
            request.commit,
        )
        .await
        .map_err(mcp_service_failure)?,
    };
    let staged = stage_write_attempt(
        txn,
        request.node_did,
        request.commit,
        request.attempt,
        &receipt,
    )
    .await?;
    Ok((receipt, staged))
}

fn skill_failure(failure: SkillFailure) -> DocumentFailure {
    failure.map(|error| match error {
        gents_protocol::operations::skill::SkillOperationError::Document(error) => error,
        other => gents_protocol::operations::document::DocumentOperationError::Passthrough(
            gents_protocol::error_chain::PassedThrough::error(other),
        ),
    })
}

fn mcp_service_failure(failure: McpServiceFailure) -> DocumentFailure {
    failure.map(|error| match error {
        gents_protocol::operations::mcp_service::McpServiceOperationError::Document(error) => error,
        other => gents_protocol::operations::document::DocumentOperationError::Passthrough(
            gents_protocol::error_chain::PassedThrough::error(other),
        ),
    })
}
