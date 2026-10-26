use std::collections::BTreeSet;

use gents::config_client::ConfigRead;
use gents::session::canonical_rows::TranscriptMessageRow;
use gents_desktop_core::client::CanonicalTranscriptDependencies;
use gents_protocol::output::{
    origin::{resolve_origin, ObservedMessage},
    MessagePublication, MessageRole,
};
use gents_protocol::row::AgentRequestRow;

use crate::types::{DesktopSessionSnapshot, ReconstructionState, RenderedTimelineItem};

pub(crate) async fn attach(
    access: &dyn ConfigRead,
    requests: &[AgentRequestRow],
    headers: &[TranscriptMessageRow],
    dependencies: Option<&CanonicalTranscriptDependencies>,
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
    let observed = headers
        .iter()
        .chain(
            dependencies
                .into_iter()
                .flat_map(|deps| &deps.origin_headers),
        )
        .map(|row| ObservedMessage {
            doc_id: &row.doc_id,
            message: &row.message,
        })
        .collect::<Vec<_>>();
    let denied = dependencies.map_or(&[][..], |deps| deps.denied_header_doc_ids.as_slice());
    let copied = headers
        .iter()
        .filter(|row| {
            row.message.session_id == snapshot.session_id
                && snapshot.node_did.as_deref() == Some(row.message.node_did.as_str())
                && row.message.role == MessageRole::User
                && matches!(row.message.publication, MessagePublication::Fork { .. })
                && snapshot.messages.iter().any(|message| {
                    message.message_key == row.message.message_key
                        && message.reconstruction_state == ReconstructionState::Ready
                })
        })
        .filter_map(|row| {
            let origin = resolve_origin(
                &observed,
                denied,
                &row.doc_id,
                &row.message.node_did,
                row.message.requester_did.as_deref(),
            )
            .ok()?;
            Some((row.message.message_key.as_str(), origin.message))
        })
        .collect::<Vec<_>>();
    let origins = copied
        .iter()
        .map(|(_, message)| *message)
        .collect::<Vec<_>>();
    let confirmed = match gents::goal::load_applied_opening_goal_provenance(
        access,
        &candidates,
        &origins,
    )
    .await
    {
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
        let Some(goal_id) = confirmed.by_request_doc_id.get(doc) else {
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
    for (key, origin) in copied {
        let Some(goal_id) = confirmed.by_message_key.get(&origin.message_key) else {
            continue;
        };
        for message in &mut snapshot.messages {
            if message.message_key == key {
                message.started_goal_id = Some(goal_id.clone());
            }
        }
        for item in &mut snapshot.timeline_items {
            if let RenderedTimelineItem::UserMessage {
                item_key,
                started_goal_id,
                ..
            } = item
            {
                if item_key == key {
                    *started_goal_id = Some(goal_id.clone());
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "goal_provenance/tests.rs"]
pub(crate) mod tests;
