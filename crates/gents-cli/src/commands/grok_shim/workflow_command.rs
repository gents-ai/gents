//! Host-side `/workflow stop|pause|runs` controls. Everything else after
//! `/workflow` reaches the model as ordinary prompt text.
use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use gents::config_client::ConfigAccess;
use gents::tools::background::job::control::control;
use gents::tools::background::job::view::{list_jobs, JobFilter, StateFilter};
use gents_protocol::job::{JobControlKind, JobControlOutcome, JobReadError, JobView};

use crate::commands::job::{
    clean, control_lines, operator_scope, read_error, resolve_job, state_name, ACTIVE_STATES,
};

#[derive(Debug, PartialEq)]
pub(super) enum WorkflowCommand {
    Stop { handle: Option<String> },
    Pause { handle: Option<String> },
    Runs,
}

impl WorkflowCommand {
    pub(super) fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        if words.next() != Some("/workflow") {
            return None;
        }
        let verb = words.next().map(str::to_ascii_lowercase);
        let handle = words.next().map(str::to_owned);
        if words.next().is_some() {
            return None;
        }
        match (verb.as_deref(), handle) {
            (None | Some("runs"), None) => Some(Self::Runs),
            (Some("stop"), handle) => Some(Self::Stop { handle }),
            (Some("pause"), handle) => Some(Self::Pause { handle }),
            _ => None,
        }
    }

    /// Operator controls run beside the node, which satisfies the owner's
    /// local-only rule. The ACP caller must authorize the exact attached
    /// session before invoking this.
    pub(super) async fn execute(
        &self,
        node: &Arc<EmbeddedNode>,
        principal: &str,
        session: &str,
    ) -> Result<String> {
        let access = ConfigAccess::local(node.clone());
        let (kind, handle) = match self {
            Self::Runs => return overview(&access, principal, session, "Workflow runs:").await,
            Self::Stop { handle } => (JobControlKind::Stop, handle),
            Self::Pause { handle } => (JobControlKind::Pause, handle),
        };
        let Some(handle) = handle else {
            let verb = match kind {
                JobControlKind::Stop => "stop",
                JobControlKind::Pause => "pause",
            };
            let header = format!("Name a run: /workflow {verb} <handle>");
            return overview(&access, principal, session, &header).await;
        };
        let view = match resolve_job(&access, principal, handle, Some(session)).await {
            Ok(view) if view.session_id == session => view,
            Ok(_) => return Ok(UNKNOWN_RUN.to_owned()),
            Err(error) => {
                return Ok(match error.downcast_ref::<JobReadError>() {
                    Some(typed)
                        if !matches!(
                            typed,
                            JobReadError::NotAJob | JobReadError::NotAuthorized
                        ) =>
                    {
                        error.to_string()
                    }
                    _ => UNKNOWN_RUN.to_owned(),
                })
            }
        };
        let result = control(&access, &operator_scope(&view), &view.tool_call_id, kind).await;
        Ok(control_reply(kind, &view, result))
    }
}

const UNKNOWN_RUN: &str = "workflow_control_unknown_run\nnext:\n  /workflow runs";

async fn overview(
    access: &ConfigAccess,
    principal: &str,
    session: &str,
    header: &str,
) -> Result<String> {
    let views = match list_jobs(
        access,
        &JobFilter {
            session_id: Some(session.to_owned()),
            requester: Some(principal.to_owned()),
            state: StateFilter::States(ACTIVE_STATES.to_vec()),
            plugin: None,
        },
    )
    .await
    {
        Ok(views) => views,
        Err(error) => return Ok(read_error(error).to_string()),
    };
    if views.is_empty() {
        return Ok("No active workflow runs in this session.".to_owned());
    }
    let mut lines = vec![header.to_owned()];
    lines.extend(views.iter().map(|view| {
        format!(
            "  {}  {}  {}",
            clean(&view.handle),
            state_name(view.state),
            view.address
        )
    }));
    Ok(lines.join("\n"))
}

/// The host-turn text for one control result: the shim's answer for an applied
/// control, the CLI's rendering for every other outcome and error.
pub(super) fn control_reply(
    kind: JobControlKind,
    view: &JobView,
    result: Result<JobControlOutcome, JobReadError>,
) -> String {
    let handle = clean(&view.handle);
    match (kind, &result) {
        (
            JobControlKind::Stop,
            Ok(JobControlOutcome::Applied {
                killed_children, ..
            }),
        ) => format!("Stopped {handle} ({killed_children} agents interrupted)."),
        (JobControlKind::Pause, Ok(JobControlOutcome::Applied { .. })) => {
            format!("Paused {handle}. Its tool's help says how a paused run is resumed.")
        }
        _ => match control_lines(kind, view, result) {
            Ok(lines) => lines.join("\n"),
            Err(error) => error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_command_parser_table() {
        for (text, expected) in [
            ("/workflow", WorkflowCommand::Runs),
            (" /workflow RUNS ", WorkflowCommand::Runs),
            ("/workflow runs", WorkflowCommand::Runs),
            ("/workflow stop", WorkflowCommand::Stop { handle: None }),
            (
                "/workflow stop alpha",
                WorkflowCommand::Stop {
                    handle: Some("alpha".to_owned()),
                },
            ),
            ("/workflow PAUSE", WorkflowCommand::Pause { handle: None }),
            (
                "/workflow pause s1:call-1",
                WorkflowCommand::Pause {
                    handle: Some("s1:call-1".to_owned()),
                },
            ),
        ] {
            assert_eq!(WorkflowCommand::parse(text), Some(expected), "{text}");
        }
        for text in [
            "/workflow research the topic",
            "/workflow resume alpha",
            "/workflow answer alpha yes",
            "/workflow save alpha",
            "/workflow stop alpha beta",
            "/workflow runs now",
            "/workflows",
            "/goal",
            "stop",
        ] {
            assert_eq!(WorkflowCommand::parse(text), None, "{text}");
        }
    }
}
