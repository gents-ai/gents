use gents_protocol::row::AgentRequest;
use gents_protocol::session::{SessionCreationCause, SessionScope};
use gents_store::config_client::ConfigAccess;

pub const WORKFLOW_RETURN: &str = "Return your answer in your final reply. The workflow collects it automatically; you do not need to message its caller. Use agent_message only for another reachable session when the task requires it.";

pub const DELEGATED_ASSIGNMENT: &str = "This opening turn is an authorized assignment from the session that started you. Complete the task and return your answer in your final reply; the runtime delivers it to your caller automatically. Do not use `agent_message` or `sessions` on your starter session.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestContextSection {
    Instructions,
    WorkflowReturn,
    DelegatedAssignment,
    ProfileModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestCreationContext {
    pub cause: Option<SessionCreationCause>,
    pub opening: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RequestContextError {
    #[error("the session's creation receipt could not be read for the request context")]
    CreationReceipt(#[source] anyhow::Error),
}

/// Lean `PromptAssembly.RequestContext.sections`.
pub fn request_context_sections(
    instructions: bool,
    creation: Option<SessionCreationCause>,
    opening: bool,
    profile_model: bool,
) -> Vec<RequestContextSection> {
    let mut sections = Vec::with_capacity(4);
    if instructions {
        sections.push(RequestContextSection::Instructions);
    }
    if creation == Some(SessionCreationCause::PluginJob) {
        sections.push(RequestContextSection::WorkflowReturn);
    }
    if creation == Some(SessionCreationCause::AgentNew) && opening {
        sections.push(RequestContextSection::DelegatedAssignment);
    }
    if profile_model {
        sections.push(RequestContextSection::ProfileModel);
    }
    sections
}

/// The verified creation cause of `request`'s session and whether `request` is
/// that receipt's opening request, read as `access`, the Agent working the
/// request.
pub async fn request_creation_context(
    access: &ConfigAccess,
    request: &AgentRequest,
) -> Result<RequestCreationContext, RequestContextError> {
    let info = crate::session::origin::verified_creation_cause_and_opening(
        access,
        &SessionScope::of_request(request),
    )
    .await
    .map_err(RequestContextError::CreationReceipt)?;
    Ok(match info {
        Some(info) => RequestCreationContext {
            cause: Some(info.cause),
            opening: info.opening_request_doc_id == request.doc_id,
        },
        None => RequestCreationContext {
            cause: None,
            opening: false,
        },
    })
}

/// `profile_model` is the model of the Agent's admitted inference profile,
/// `None` for an Agent built without one.
pub fn request_context_body(
    instructions: Option<String>,
    creation: Option<SessionCreationCause>,
    opening: bool,
    profile_model: Option<&str>,
) -> Option<String> {
    let mut instructions = instructions;
    let parts = request_context_sections(
        instructions.is_some(),
        creation,
        opening,
        profile_model.is_some(),
    )
    .into_iter()
    .filter_map(|section| match section {
        RequestContextSection::Instructions => instructions.take(),
        RequestContextSection::WorkflowReturn => Some(WORKFLOW_RETURN.to_owned()),
        RequestContextSection::DelegatedAssignment => Some(DELEGATED_ASSIGNMENT.to_owned()),
        RequestContextSection::ProfileModel => {
            profile_model.map(|model| format!("Your profile's model: {model}."))
        }
    })
    .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_context_sections_match_lean() {
        use gents_claude::origin::Cause;
        gents_lean_vocab::families::prompt_assembly_request_context_sections::replay(|i| {
            let creation = match i.creation {
                Cause::Client => None,
                Cause::AgentNew => Some(SessionCreationCause::AgentNew),
                Cause::TaskTrigger => Some(SessionCreationCause::TaskTrigger),
                Cause::PluginJob => Some(SessionCreationCause::PluginJob),
            };
            request_context_sections(i.instructions, creation, i.opening, i.profile_model)
        });
    }

    #[test]
    fn a_workflow_child_reads_its_instructions_then_where_its_answer_returns_then_its_model() {
        let child = request_context_body(
            Some("# Live workspace instructions".to_owned()),
            Some(SessionCreationCause::PluginJob),
            false,
            Some("model-a"),
        );
        assert_eq!(
            child.as_deref(),
            Some(
                format!(
                    "# Live workspace instructions\n\n{WORKFLOW_RETURN}\n\nYour profile's model: model-a."
                )
                .as_str()
            )
        );
        assert_eq!(
            request_context_body(None, Some(SessionCreationCause::AgentNew), true, None).as_deref(),
            Some(DELEGATED_ASSIGNMENT)
        );
        assert_eq!(
            request_context_body(None, Some(SessionCreationCause::AgentNew), false, None),
            None
        );
    }
}
