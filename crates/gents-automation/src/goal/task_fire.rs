use super::*;

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskGoalFireIdentity {
    pub session_id: String,
    pub request_id: String,
    pub retry_key: String,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TaskGoalRequestBinding {
    #[serde(default)]
    pub node_did: String,
    #[serde(default)]
    pub agent_did: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub retry_key: String,
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskGoalFireRecoveryDisposition {
    Absent,
    Recovered,
    Conflict,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskGoalFireRecoveryDecision {
    pub disposition: TaskGoalFireRecoveryDisposition,
    pub recovered_request_id: Option<String>,
    pub checkpointable: bool,
}

#[doc(hidden)]
pub fn decide_task_goal_fire_recovery(
    expected: &TaskGoalRequestBinding,
    observed: Option<&TaskGoalRequestBinding>,
) -> TaskGoalFireRecoveryDecision {
    match observed {
        None => TaskGoalFireRecoveryDecision {
            disposition: TaskGoalFireRecoveryDisposition::Absent,
            recovered_request_id: None,
            checkpointable: false,
        },
        Some(observed) if observed == expected => TaskGoalFireRecoveryDecision {
            disposition: TaskGoalFireRecoveryDisposition::Recovered,
            recovered_request_id: Some(observed.request_id.clone()),
            checkpointable: true,
        },
        Some(_) => TaskGoalFireRecoveryDecision {
            disposition: TaskGoalFireRecoveryDisposition::Conflict,
            recovered_request_id: None,
            checkpointable: false,
        },
    }
}

/// Length-prefixing matches `GoalAutomation.taskFireIdentity`.
#[doc(hidden)]
pub fn task_goal_fire_identity(
    node_did: &str,
    task_id: &str,
    fire_key: &str,
) -> TaskGoalFireIdentity {
    let scope = format!(
        "{}:{node_did}:{}:{task_id}:{}:{fire_key}",
        node_did.chars().count(),
        task_id.chars().count(),
        fire_key.chars().count()
    );
    TaskGoalFireIdentity {
        session_id: format!("task-goal-session:{scope}"),
        request_id: format!("task-goal-request:{scope}"),
        retry_key: format!("task-goal-retry:{scope}"),
    }
}

pub(crate) fn task_goal_fire_key<'a>(request_id: &'a str, expected_owner: &str) -> Option<&'a str> {
    fn component(input: &str) -> Option<(&str, &str)> {
        let (length, rest) = input.split_once(':')?;
        let length = length.parse::<usize>().ok()?;
        let end = rest
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(rest.len()))
            .nth(length)?;
        Some((&rest[..end], &rest[end..]))
    }
    let scope = request_id.strip_prefix("task-goal-request:")?;
    let (owner, tail) = component(scope)?;
    let (task, tail) = component(tail.strip_prefix(':')?)?;
    let (fire, tail) = component(tail.strip_prefix(':')?)?;
    if owner != expected_owner
        || !tail.is_empty()
        || task_goal_fire_identity(owner, task, fire).request_id != request_id
    {
        return None;
    }
    Some(fire)
}
