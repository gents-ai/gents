#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ExecutionIdentity {
    pub request: String,
    pub generation: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
    Unknown,
    Queued,
    Generating,
    ToolWait,
    ProviderRetry,
    Unavailable,
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ExecutionObservation {
    pub identity: ExecutionIdentity,
    pub sequence: u64,
    pub turn: u64,
    pub attempt: u64,
    pub observed_at: u64,
    pub phase: ExecutionPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDisplay {
    Unknown,
    Queued,
    Working,
    Waiting,
    Retrying,
    Unavailable,
    Terminal,
}

pub fn failure_execution_phase(will_retry: bool, unreachable: bool) -> ExecutionPhase {
    if will_retry {
        ExecutionPhase::ProviderRetry
    } else if unreachable {
        ExecutionPhase::Unavailable
    } else {
        ExecutionPhase::Unknown
    }
}

pub fn accept_execution_observation(
    active: &ExecutionIdentity,
    current: Option<ExecutionObservation>,
    incoming: ExecutionObservation,
) -> Option<ExecutionObservation> {
    if &incoming.identity != active {
        return current;
    }
    if current.as_ref().is_none_or(|previous| {
        &previous.identity != active
            || (incoming.sequence > previous.sequence
                && (incoming.turn > previous.turn
                    || (incoming.turn == previous.turn && incoming.attempt >= previous.attempt)))
    }) {
        Some(incoming)
    } else {
        current
    }
}

pub fn project_execution_activity(
    active: &ExecutionIdentity,
    terminal: bool,
    now: u64,
    max_age: u64,
    observation: Option<&ExecutionObservation>,
) -> ExecutionDisplay {
    if terminal {
        return ExecutionDisplay::Terminal;
    }
    let Some(observation) = observation else {
        return ExecutionDisplay::Unknown;
    };
    if &observation.identity != active
        || observation.observed_at > now
        || now - observation.observed_at > max_age
    {
        return ExecutionDisplay::Unknown;
    }
    match observation.phase {
        ExecutionPhase::Unknown => ExecutionDisplay::Unknown,
        ExecutionPhase::Queued => ExecutionDisplay::Queued,
        ExecutionPhase::Generating => ExecutionDisplay::Working,
        ExecutionPhase::ToolWait => ExecutionDisplay::Waiting,
        ExecutionPhase::ProviderRetry => ExecutionDisplay::Retrying,
        ExecutionPhase::Unavailable => ExecutionDisplay::Unavailable,
        ExecutionPhase::Terminal => ExecutionDisplay::Unknown,
    }
}

/// Bind an observation to the authoritative request and its current execution
/// generation before showing it to a client. A terminal lifecycle wins even
/// when the request has no current generation.
pub fn project_request_execution_activity(
    request: &crate::row::AgentRequestRow,
    now: u64,
    max_age: u64,
    observation: Option<&ExecutionObservation>,
) -> ExecutionDisplay {
    if request
        .lifecycle_state
        .is_some_and(|state| state.is_terminal())
    {
        return ExecutionDisplay::Terminal;
    }
    if request.lifecycle_state == Some(crate::request_lifecycle::RequestLifecycleState::Pending) {
        return ExecutionDisplay::Queued;
    }
    let (Some(request_doc_id), Some(generation)) = (
        request.doc_id.as_ref(),
        request.execution_generation.as_ref(),
    ) else {
        return ExecutionDisplay::Unknown;
    };
    let identity = ExecutionIdentity {
        request: request_doc_id.clone(),
        generation: generation.clone(),
    };
    project_execution_activity(&identity, false, now, max_age, observation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_lifecycle::RequestLifecycleState;

    #[test]
    fn request_projection_requires_the_current_document_and_generation() {
        let mut request = crate::row::AgentRequestRow {
            doc_id: Some("request".into()),
            execution_generation: Some("generation".into()),
            lifecycle_state: Some(RequestLifecycleState::Processing),
            ..Default::default()
        };
        let observation = ExecutionObservation {
            identity: ExecutionIdentity {
                request: "request".into(),
                generation: "generation".into(),
            },
            sequence: 1,
            turn: 0,
            attempt: 0,
            observed_at: 10,
            phase: ExecutionPhase::ProviderRetry,
        };
        assert_eq!(
            project_request_execution_activity(&request, 10, 1, Some(&observation)),
            ExecutionDisplay::Retrying
        );
        request.execution_generation = Some("new-generation".into());
        assert_eq!(
            project_request_execution_activity(&request, 10, 1, Some(&observation)),
            ExecutionDisplay::Unknown
        );
        request.execution_generation = None;
        request.lifecycle_state = Some(RequestLifecycleState::Pending);
        assert_eq!(
            project_request_execution_activity(&request, 10, 1, Some(&observation)),
            ExecutionDisplay::Queued
        );
        request.lifecycle_state = Some(RequestLifecycleState::Completed);
        assert_eq!(
            project_request_execution_activity(&request, 10, 1, Some(&observation)),
            ExecutionDisplay::Terminal
        );
    }
}
