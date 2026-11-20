use gents_protocol::request_input::QueueSource;
use gents_protocol::row::AgentRequestRow;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Facts {
    pub user_role: bool,
    pub output_obligation: bool,
    pub completion_notification: bool,
    pub goal_continuation: bool,
    pub completion_wake: bool,
    pub parent_tool: bool,
    pub trigger: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Person,
    SessionMessage,
    Trigger,
    GoalContinuation,
    BackgroundWake,
    BackgroundResult,
    RuntimeControl,
    OtherRole,
}

pub fn classify(facts: Facts) -> Kind {
    if !facts.user_role {
        Kind::OtherRole
    } else if facts.output_obligation {
        Kind::RuntimeControl
    } else if facts.completion_notification {
        Kind::BackgroundResult
    } else if facts.goal_continuation {
        Kind::GoalContinuation
    } else if facts.completion_wake {
        Kind::BackgroundWake
    } else if facts.parent_tool {
        Kind::SessionMessage
    } else if facts.trigger {
        Kind::Trigger
    } else {
        Kind::Person
    }
}

pub fn request_facts(row: &AgentRequestRow) -> Facts {
    let input = row.input.as_ref();
    let source = input
        .and_then(|input| input.queue.as_ref())
        .map(|queue| queue.source);
    let present =
        |value: &Option<String>| value.as_deref().is_some_and(|text| !text.trim().is_empty());
    Facts {
        user_role: true,
        output_obligation: false,
        completion_notification: false,
        goal_continuation: input.is_some_and(|input| input.goal_continuation.is_some())
            || source == Some(QueueSource::Goal),
        completion_wake: source == Some(QueueSource::BackgroundCompletion),
        parent_tool: present(&row.caused_by_parent_tool_call_doc_id),
        trigger: present(&row.caused_by_trigger_id),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn transcript_input_origin_matches_lean() {
        gents_lean_vocab::families::transcript_input_origin_classification::replay(|input| {
            super::classify(super::Facts {
                user_role: input.user_role,
                output_obligation: input.output_obligation,
                completion_notification: input.completion_notification,
                goal_continuation: input.goal_continuation,
                completion_wake: input.completion_wake,
                parent_tool: input.parent_tool,
                trigger: input.trigger,
            })
        });
    }
}
