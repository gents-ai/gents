use super::*;

fn hints(parent: &AgentRequest) -> RequestQueue {
    RequestQueue {
        delivery: Default::default(),
        position: None,
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{}", parent.session_id)),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    }
}

