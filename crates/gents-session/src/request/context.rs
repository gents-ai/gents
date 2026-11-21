use gents_protocol::row::AgentRequest;
use gents_protocol::session::{SessionCreationCause, SessionScope};
use gents_store::config_client::ConfigAccess;

pub const WORKFLOW_RETURN: &str = "Return your answer in your final reply. The workflow collects it automatically; you do not need to message its caller. Use agent_message only for another reachable session when the task requires it.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestContextSection {
    Instructions,
    WorkflowReturn,
    ProfileModel,
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
    profile_model: bool,
) -> Vec<RequestContextSection> {
    let mut sections = Vec::with_capacity(3);
    if instructions {
        sections.push(RequestContextSection::Instructions);
    }
    if creation == Some(SessionCreationCause::PluginJob) {
        sections.push(RequestContextSection::WorkflowReturn);
    }
    if profile_model {
        sections.push(RequestContextSection::ProfileModel);
    }
    sections
}

/// The verified creation cause of `request`'s session, read as `access`, the
/// Agent working the request.
pub async fn request_creation_cause(
    access: &ConfigAccess,
    request: &AgentRequest,
) -> Result<Option<SessionCreationCause>, RequestContextError> {
    crate::session::origin::verified_creation_cause(access, &SessionScope::of_request(request))
        .await
        .map_err(RequestContextError::CreationReceipt)
}

/// `profile_model` is the model of the Agent's admitted inference profile,
/// `None` for an Agent built without one.
pub fn request_context_body(
    instructions: Option<String>,
    creation: Option<SessionCreationCause>,
    profile_model: Option<&str>,
) -> Option<String> {
    let mut instructions = instructions;
    let parts = request_context_sections(instructions.is_some(), creation, profile_model.is_some())
        .into_iter()
        .filter_map(|section| match section {
            RequestContextSection::Instructions => instructions.take(),
            RequestContextSection::WorkflowReturn => Some(WORKFLOW_RETURN.to_owned()),
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
        gents_lean_vocab::families::prompt_assembly_request_context_sections::replay(|i| {
            request_context_sections(i.instructions, i.creation, i.profile_model)
        });
    }

    #[test]
    fn a_workflow_child_reads_its_instructions_then_where_its_answer_returns_then_its_model() {
        let child = request_context_body(
            Some("# Live workspace instructions".to_owned()),
            Some(SessionCreationCause::PluginJob),
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
            request_context_body(None, Some(SessionCreationCause::AgentNew), None),
            None
        );
    }
}
