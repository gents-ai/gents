use std::collections::BTreeSet;

use gents::config_client::ConfigRead;
use gents_protocol::row::AgentRequestRow;

use crate::types::{DesktopSessionSnapshot, RenderedTimelineItem};

pub(crate) async fn attach(
    access: &dyn ConfigRead,
    requests: &[AgentRequestRow],
    snapshot: &mut DesktopSessionSnapshot,
) {
    let mut read = BTreeSet::new();
    let mut candidates = Vec::new();
    for request in requests {
        if !request
            .input
            .as_ref()
            .and_then(|input| input.goal_assignment.as_ref())
            .is_some_and(|assignment| assignment.root_request_doc_id.is_none())
        {
            continue;
        }
        let Some(doc) = request.doc_id.as_deref() else {
            continue;
        };
        let owner = gents::lifecycle::input_message_owner(request);
        let has_message = owner.as_ref().is_some_and(|(_, key)| {
            snapshot.messages.iter().any(|message| {
                message.message_key == *key
                    && message.input_request_id.as_deref() == Some(&request.request_id)
            })
        });
        let has_pending = snapshot
            .pending_turn
            .iter()
            .chain(snapshot.queued_turns.iter())
            .any(|turn| turn.request_doc_id.as_deref() == Some(doc));
        if !(has_message || has_pending) || !read.insert(doc) {
            continue;
        }
        candidates.push(request);
    }
    let confirmed = match gents::goal::load_applied_opening_goal_ids(access, &candidates).await {
        Ok(confirmed) => confirmed,
        Err(error) => {
            tracing::warn!(error = %error, "opening Goal provenance unavailable");
            return;
        }
    };
    for request in candidates {
        let Some(doc) = request.doc_id.as_deref() else {
            continue;
        };
        let Some(goal_id) = confirmed.get(doc) else {
            continue;
        };
        let owner = gents::lifecycle::input_message_owner(request);
        let has_pending = snapshot
            .pending_turn
            .iter()
            .chain(snapshot.queued_turns.iter())
            .any(|turn| turn.request_doc_id.as_deref() == Some(doc));
        for turn in snapshot
            .pending_turn
            .iter_mut()
            .chain(snapshot.queued_turns.iter_mut())
        {
            if turn.request_doc_id.as_deref() == Some(doc) {
                turn.started_goal_id = Some(goal_id.clone());
            }
        }
        for message in &mut snapshot.messages {
            if owner
                .as_ref()
                .is_some_and(|(_, key)| message.message_key == *key)
                && message.input_request_id.as_deref() == Some(&request.request_id)
            {
                message.started_goal_id = Some(goal_id.clone());
            }
        }
        for item in &mut snapshot.timeline_items {
            match item {
                RenderedTimelineItem::UserMessage {
                    item_key,
                    input_request_id,
                    started_goal_id,
                    ..
                } if owner.as_ref().is_some_and(|(_, key)| item_key == key)
                    && input_request_id.as_deref() == Some(&request.request_id) =>
                {
                    *started_goal_id = Some(goal_id.clone());
                }
                RenderedTimelineItem::PendingUserTurn {
                    request_id,
                    started_goal_id,
                    ..
                } if has_pending && request_id == &request.request_id => {
                    *started_goal_id = Some(goal_id.clone());
                }
                _ => {}
            }
        }
    }
}
