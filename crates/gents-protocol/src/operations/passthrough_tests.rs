use crate::error_chain::{ErrorChain, ErrorReport, PassedThrough};

/// `AccessClassification.Printer`.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Printer {
    ErrorChain,
    ErrorReport,
    Joined,
}

/// The passed-through error: message `0` of the model, caused by message `1`.
#[derive(Debug, thiserror::Error)]
#[error("the passed-through refusal")]
struct Passed(#[source] std::io::Error);

const PASSED: &str = "the passed-through refusal";

fn passed() -> PassedThrough {
    PassedThrough::error(Passed(std::io::Error::other("its cause")))
}

/// The operation owner's `Passthrough` variant named `wrapper`, around [`passed`].
fn passthrough(wrapper: &str) -> Box<dyn std::error::Error + Send + Sync> {
    use super::{
        agent, document, goal, inference, mailbox, p2p, pack, permission, request, session, task,
        workspace_root,
    };
    match wrapper {
        "agent_operation_error_passthrough" => {
            Box::new(agent::AgentOperationError::Passthrough(passed()))
        }
        "document_operation_error_passthrough" => {
            Box::new(document::DocumentOperationError::Passthrough(passed()))
        }
        "document_delete_error_passthrough" => {
            Box::new(document::DocumentDeleteError::Passthrough(passed()))
        }
        "goal_operation_error_passthrough" => {
            Box::new(goal::GoalOperationError::Passthrough(passed()))
        }
        "inference_operation_error_passthrough" => {
            Box::new(inference::InferenceOperationError::Passthrough(passed()))
        }
        "mailbox_operation_error_passthrough" => {
            Box::new(mailbox::MailboxOperationError::Passthrough(passed()))
        }
        "registry_error_passthrough" => Box::new(p2p::RegistryError::Passthrough(passed())),
        "application_pairing_error_passthrough" => {
            Box::new(p2p::ApplicationPairingError::Passthrough(passed()))
        }
        "pack_operation_error_passthrough" => {
            Box::new(pack::PackOperationError::Passthrough(passed()))
        }
        "agent_permission_error_passthrough" => {
            Box::new(permission::AgentPermissionError::Passthrough(passed()))
        }
        "request_operation_error_passthrough" => {
            Box::new(request::RequestOperationError::Passthrough(passed()))
        }
        "session_operation_error_passthrough" => {
            Box::new(session::SessionOperationError::Passthrough(passed()))
        }
        "task_operation_error_passthrough" => {
            Box::new(task::TaskOperationError::Passthrough(passed()))
        }
        "workspace_root_error_passthrough" => {
            Box::new(workspace_root::WorkspaceRootError::Passthrough(passed()))
        }
        other => panic!(
            "operation owner {other} has no constructor here; add it with its \
             `AccessClassification.PassthroughWrapper` constructor"
        ),
    }
}

/// `AccessClassification.passthroughPrinted`: the printers of the whole chain
/// print a passed-through message once; joining every message, as anyhow's
/// `{:#}` and `{:?}` do, prints it twice.
#[test]
fn every_operation_passthrough_is_printed_once() {
    gents_lean_vocab::families::access_classification_passthrough_printed::replay::<String, Printer>(
        |input| {
            let error = passthrough(&input.passthrough_wrapper);
            let error: &(dyn std::error::Error + 'static) = error.as_ref();
            let printed = match input.printer {
                Printer::ErrorChain => ErrorChain(error).to_string(),
                Printer::ErrorReport => ErrorReport(error).to_string(),
                Printer::Joined => std::iter::successors(Some(error), |cause| cause.source())
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(": "),
            };
            printed.matches(PASSED).count() as u64
        },
    );
}
