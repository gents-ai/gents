//! Codex-independent state used to decide whether a durable observation is
//! semantically new. Wire-protocol types belong in the emit/read boundary,
//! not in the stream's equality and de-duplication model.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectionStatus {
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CollabTool {
    SpawnAgent,
    ResumeAgent,
    Wait,
    SendInput,
    CloseAgent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChildStatus {
    Pending,
    Running,
    Interrupted,
    Completed,
    Errored,
    Shutdown,
    NotFound,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CollabProjection {
    pub(super) status: ProjectionStatus,
    pub(super) tool: CollabTool,
    pub(super) receiver_thread_id: String,
    pub(super) child_model: Option<String>,
    pub(super) child_status: ChildStatus,
    pub(super) child_failure_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ToolProjectionStatus {
    Mcp(ProjectionStatus),
    Command(ProjectionStatus),
    Collab(CollabProjection),
    DeferredCollab,
    DeferredFileChange,
    FileChange(ProjectionStatus),
}

impl ToolProjectionStatus {
    pub(super) fn command_status(&self) -> ProjectionStatus {
        match self {
            Self::Command(status) => *status,
            Self::Mcp(_)
            | Self::Collab(_)
            | Self::DeferredCollab
            | Self::DeferredFileChange
            | Self::FileChange(_) => ProjectionStatus::InProgress,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectionEvent {
    Started,
    Completed,
}

pub(super) fn collab_projection_events(
    previous: Option<&ToolProjectionStatus>,
    current: &CollabProjection,
) -> Vec<ProjectionEvent> {
    match previous {
        Some(ToolProjectionStatus::Collab(previous)) => {
            if previous.status == ProjectionStatus::InProgress
                && current.status != ProjectionStatus::InProgress
            {
                vec![ProjectionEvent::Completed]
            } else if previous != current {
                // A completed-item snapshot is also the wire update for
                // agentsStates. Child lifecycle/model/failure changes must be
                // observable after the spawn operation itself has settled;
                // emitting Completed without Started refreshes presentation
                // state without reopening the item lifecycle.
                vec![ProjectionEvent::Completed]
            } else {
                Vec::new()
            }
        }
        None | Some(ToolProjectionStatus::DeferredCollab) => {
            if current.status == ProjectionStatus::InProgress {
                vec![ProjectionEvent::Started]
            } else {
                vec![ProjectionEvent::Started, ProjectionEvent::Completed]
            }
        }
        Some(_) if current.status == ProjectionStatus::InProgress => {
            vec![ProjectionEvent::Started]
        }
        Some(_) => vec![ProjectionEvent::Completed],
    }
}

pub(super) fn stabilize_projection_kind(
    previous: Option<&ToolProjectionStatus>,
    current: ToolProjectionStatus,
) -> ToolProjectionStatus {
    match (previous, current) {
        (Some(ToolProjectionStatus::Collab(previous)), ToolProjectionStatus::DeferredCollab) => {
            ToolProjectionStatus::Collab(previous.clone())
        }
        (Some(ToolProjectionStatus::Collab(previous)), ToolProjectionStatus::Mcp(status)) => {
            let mut current = previous.clone();
            current.status = status;
            ToolProjectionStatus::Collab(current)
        }
        (Some(ToolProjectionStatus::Mcp(_)), ToolProjectionStatus::DeferredCollab) => previous
            .cloned()
            .expect("matched a previous MCP projection"),
        (Some(ToolProjectionStatus::Mcp(_)), ToolProjectionStatus::Collab(current)) => {
            ToolProjectionStatus::Mcp(current.status)
        }
        (_, current) => current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running_spawn() -> CollabProjection {
        CollabProjection {
            status: ProjectionStatus::Completed,
            tool: CollabTool::SpawnAgent,
            receiver_thread_id: "child-thread".to_string(),
            child_model: Some("model".to_string()),
            child_status: ChildStatus::Running,
            child_failure_reason: None,
        }
    }

    #[test]
    fn child_presentation_updates_refresh_without_reopening_spawn() {
        let running = running_spawn();
        assert_eq!(
            collab_projection_events(None, &running),
            vec![ProjectionEvent::Started, ProjectionEvent::Completed]
        );

        let previous = ToolProjectionStatus::Collab(running.clone());
        assert!(collab_projection_events(Some(&previous), &running).is_empty());

        let mut completed = running;
        completed.child_status = ChildStatus::Completed;
        assert_eq!(
            collab_projection_events(Some(&previous), &completed),
            vec![ProjectionEvent::Completed]
        );

        let in_progress = CollabProjection {
            status: ProjectionStatus::InProgress,
            ..completed.clone()
        };
        let previous = ToolProjectionStatus::Collab(in_progress);
        assert_eq!(
            collab_projection_events(Some(&previous), &completed),
            vec![ProjectionEvent::Completed]
        );
    }

    #[test]
    fn emitted_projection_kind_survives_link_flicker_and_late_links() {
        let collab = ToolProjectionStatus::Collab(running_spawn());
        let stabilized =
            stabilize_projection_kind(Some(&collab), ToolProjectionStatus::DeferredCollab);
        assert_eq!(stabilized, collab);

        let returned = ToolProjectionStatus::Collab(running_spawn());
        let ToolProjectionStatus::Collab(returned_projection) = &returned else {
            unreachable!();
        };
        assert!(collab_projection_events(Some(&stabilized), returned_projection).is_empty());

        let mcp = ToolProjectionStatus::Mcp(ProjectionStatus::Completed);
        assert_eq!(stabilize_projection_kind(Some(&mcp), returned), mcp);
    }
}
