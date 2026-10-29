use gents_desktop_core::client::{ClientCore, InferenceActivity};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

pub(crate) fn active_generation<'a>(
    request: &'a AgentRequestRow,
    node: &str,
    session: &str,
    request_id: &str,
) -> Option<&'a str> {
    if request.request_id != request_id
        || request.node_did.as_deref() != Some(node)
        || request.session_id.as_deref() != Some(session)
        || !matches!(
            request.lifecycle_state,
            Some(RequestLifecycleState::Claimed | RequestLifecycleState::Processing)
        )
    {
        return None;
    }
    request.execution_generation.as_deref()
}

pub(crate) async fn read_activity(
    core: &ClientCore,
    request: &AgentRequestRow,
    node: &str,
    session: &str,
    request_id: &str,
) -> Option<String> {
    if request.request_id != request_id
        || request.node_did.as_deref() != Some(node)
        || request.session_id.as_deref() != Some(session)
        || !matches!(
            request.lifecycle_state,
            Some(RequestLifecycleState::Claimed | RequestLifecycleState::Processing)
        )
    {
        return None;
    }
    let activity = match active_generation(request, node, session, request_id) {
        Some(generation) => {
            let key = gents::inference_observation::InferenceKey {
                node: node.to_owned(),
                session: session.to_owned(),
                request: request_id.to_owned(),
                generation: generation.to_owned(),
            };
            match core.read_inference_activity(&key).await {
                Ok(Some(InferenceActivity::Working)) => "working",
                Ok(Some(InferenceActivity::Waiting)) => "waiting",
                Ok(Some(InferenceActivity::Retrying)) => "retrying",
                Ok(Some(InferenceActivity::AwaitingProvider)) => "awaiting_provider",
                Ok(None) => return None,
                _ => "unavailable",
            }
        }
        None => return None,
    };
    Some(activity.to_owned())
}
