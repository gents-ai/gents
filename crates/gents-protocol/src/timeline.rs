//! Presentation-neutral timeline ordering (#608 parity).
//!
//! Every client shell — desktop today, mobile next — must render a session's
//! transcript in the **same order**, interleaving assistant/user messages with
//! their tool groups, placing the pending turn, appending orphan tool groups,
//! and the live-assistant overlay last. The *order and the message↔tool-group
//! partition* are semantics; only the pixels are presentation.
//!
//! That ordering used to live only in the desktop Tauri bridge
//! (`build_rendered_timeline`), unshared and unfenced — the single biggest
//! parity risk, because a second shell that re-interleaves will drift on order
//! and on which tool group is an orphan. This module is the shared, Lean-fenced
//! skeleton (`proofs/Proofs/ClientShell/Timeline.lean`): shells compute the slot
//! order here, then map each neutral slot to their own rich item.

/// A message's role, reduced to what the ordering cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineMessageInput {
    pub key: String,
    /// Ordering key and tool-group attach key. `None` sorts first, matching the
    /// `BTreeMap<Option<i64>, _>` grouping the desktop bridge uses.
    pub sequence: Option<i64>,
    pub role: TimelineRole,
    pub emits_item: bool,
    pub dedup_token: Option<String>,
}

/// The live-assistant overlay's ordering-relevant state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayInput {
    /// True when the overlay's content equals the trailing assistant message
    /// already in the timeline — in which case it must NOT be re-emitted.
    pub matches_trailing_assistant: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineSlot {
    Message {
        key: String,
        sequence: Option<i64>,
        role: TimelineRole,
    },
    ToolGroup {
        message_sequence: Option<i64>,
    },
    Pending,
    Overlay,
}

/// The BTreeMap ordering the desktop bridge relies on: `None` first, then
/// ascending. Kept explicit so the ordering is a stated contract, not an
/// accident of a collection type.
fn sequence_lt(left: Option<i64>, right: Option<i64>) -> bool {
    match (left, right) {
        (None, None) => false,
        (None, Some(_)) => true,
        (Some(_), None) => false,
        (Some(l), Some(r)) => l < r,
    }
}

pub fn build_timeline_order(
    messages: &[TimelineMessageInput],
    group_sequences: &[Option<i64>],
    has_pending: bool,
    overlay: Option<OverlayInput>,
) -> Vec<TimelineSlot> {
    let mut ordered: Vec<&TimelineMessageInput> = messages.iter().collect();
    ordered.sort_by(|left, right| {
        if sequence_lt(left.sequence, right.sequence) {
            std::cmp::Ordering::Less
        } else if sequence_lt(right.sequence, left.sequence) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });

    let mut slots = Vec::new();
    let mut seen_keys = std::collections::BTreeSet::new();
    let mut seen_tokens = std::collections::BTreeSet::new();
    let mut attached = std::collections::BTreeSet::new();

    for message in ordered {
        if !seen_keys.insert(message.key.clone()) {
            continue;
        }
        if let Some(token) = &message.dedup_token {
            if !seen_tokens.insert(token.clone()) {
                continue;
            }
        }
        if message.emits_item {
            slots.push(TimelineSlot::Message {
                key: message.key.clone(),
                sequence: message.sequence,
                role: message.role,
            });
        }
        if group_sequences.contains(&message.sequence) && attached.insert(message.sequence) {
            slots.push(TimelineSlot::ToolGroup {
                message_sequence: message.sequence,
            });
        }
    }

    if has_pending {
        slots.push(TimelineSlot::Pending);
    }

    let mut orphans: Vec<Option<i64>> = group_sequences
        .iter()
        .copied()
        .filter(|sequence| !attached.contains(sequence))
        .collect();
    orphans.sort_by(|left, right| {
        if sequence_lt(*left, *right) {
            std::cmp::Ordering::Less
        } else if sequence_lt(*right, *left) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    orphans.dedup();
    for sequence in orphans {
        slots.push(TimelineSlot::ToolGroup {
            message_sequence: sequence,
        });
    }

    if let Some(overlay) = overlay {
        if !overlay.matches_trailing_assistant {
            slots.push(TimelineSlot::Overlay);
        }
    }

    slots
}

#[cfg(test)]
mod tests {
    //! Conformance for the timeline ordering skeleton. Each test drives the real
    //! `build_timeline_order` through a witness and asserts a property the Lean
    //! model proves (`proofs/Proofs/ClientShell/Timeline.lean`). If the Rust
    //! ordering drifts from the fenced discipline, the matching test fails —
    //! which is what stops a second shell from silently diverging.

    use super::*;

    fn msg(key: &str, seq: i64, role: TimelineRole) -> TimelineMessageInput {
        TimelineMessageInput {
            key: key.to_string(),
            sequence: Some(seq),
            role,
            emits_item: true,
            dedup_token: None,
        }
    }

    fn count_group(slots: &[TimelineSlot], seq: Option<i64>) -> usize {
        slots
            .iter()
            .filter(|s| matches!(s, TimelineSlot::ToolGroup { message_sequence } if *message_sequence == seq))
            .count()
    }

    /// Lean `group_attached_or_orphan` + `group_not_both`: every tool group is
    /// placed exactly once — attached to its owner or as an orphan, never both,
    /// never dropped.
    #[test]
    fn every_tool_group_is_placed_exactly_once() {
        let messages = vec![
            msg("a", 0, TimelineRole::User),
            msg("b", 2, TimelineRole::Assistant),
        ];
        // seq 0 is owned by message "a"; seq 5 is an orphan (no message owns it).
        let groups = vec![Some(0), Some(5)];
        let slots = build_timeline_order(&messages, &groups, false, None);

        assert_eq!(
            count_group(&slots, Some(0)),
            1,
            "attached group placed once"
        );
        assert_eq!(count_group(&slots, Some(5)), 1, "orphan group placed once");
        // The attached group immediately follows its owner message.
        let a_pos = slots
            .iter()
            .position(|s| matches!(s, TimelineSlot::Message { key, .. } if key == "a"))
            .unwrap();
        assert!(
            matches!(
                &slots[a_pos + 1],
                TimelineSlot::ToolGroup {
                    message_sequence: Some(0)
                }
            ),
            "attached group must immediately follow its owner: {slots:?}"
        );
    }

    /// Lean `overlay_shown_iff` + `overlay_is_last`: the overlay is emitted iff
    /// present and not a duplicate of the trailing assistant, and it is last.
    #[test]
    fn overlay_is_shown_conditionally_and_last() {
        let messages = vec![msg("a", 0, TimelineRole::Assistant)];
        let groups = vec![Some(0)];

        let shown = build_timeline_order(
            &messages,
            &groups,
            true,
            Some(OverlayInput {
                matches_trailing_assistant: false,
            }),
        );
        assert_eq!(
            shown.last(),
            Some(&TimelineSlot::Overlay),
            "overlay must be last: {shown:?}"
        );

        let hidden = build_timeline_order(
            &messages,
            &groups,
            true,
            Some(OverlayInput {
                matches_trailing_assistant: true,
            }),
        );
        assert!(
            !hidden.contains(&TimelineSlot::Overlay),
            "a duplicate overlay must not be shown: {hidden:?}"
        );

        let absent = build_timeline_order(&messages, &groups, true, None);
        assert!(!absent.contains(&TimelineSlot::Overlay));
    }

    /// Lean `pending_shown_iff`: the pending turn appears iff a turn is pending,
    /// and (structurally) after the body, before orphan groups.
    #[test]
    fn pending_turn_shown_iff_and_before_orphans() {
        let messages = vec![msg("a", 0, TimelineRole::User)];
        let groups = vec![Some(9)]; // orphan
        let slots = build_timeline_order(&messages, &groups, true, None);

        let pending_pos = slots
            .iter()
            .position(|s| matches!(s, TimelineSlot::Pending));
        let orphan_pos = slots.iter().position(|s| {
            matches!(
                s,
                TimelineSlot::ToolGroup {
                    message_sequence: Some(9)
                }
            )
        });
        assert!(pending_pos.is_some(), "pending must be shown");
        assert!(
            pending_pos < orphan_pos,
            "pending must precede orphan groups: {slots:?}"
        );

        let no_pending = build_timeline_order(&messages, &groups, false, None);
        assert!(!no_pending
            .iter()
            .any(|s| matches!(s, TimelineSlot::Pending)));
    }

    /// Lean `kept_keys_nodup`: first-wins dedup by key — a repeated message key
    /// yields a single message slot.
    #[test]
    fn duplicate_message_keys_are_deduped_first_wins() {
        let messages = vec![
            msg("dup", 0, TimelineRole::Assistant),
            msg("dup", 1, TimelineRole::Assistant),
            msg("other", 2, TimelineRole::Assistant),
        ];
        let slots = build_timeline_order(&messages, &[], false, None);
        let dup_count = slots
            .iter()
            .filter(|s| matches!(s, TimelineSlot::Message { key, .. } if key == "dup"))
            .count();
        assert_eq!(
            dup_count, 1,
            "a repeated message key must render once: {slots:?}"
        );
    }

    /// Presentation-token dedup collapses re-presentations AND suppresses the
    /// second message's group attach (the desktop `continue`-before-attach
    /// nuance a naive shell would miss).
    #[test]
    fn presentation_token_dedup_also_drops_the_second_group_attach() {
        let mut first = msg("m1", 0, TimelineRole::Assistant);
        first.dedup_token = Some("same".to_string());
        let mut second = msg("m2", 1, TimelineRole::Assistant);
        second.dedup_token = Some("same".to_string());

        // Both message sequences own a group; the second message is dropped by
        // presentation dedup, so its group becomes an orphan (placed in the tail),
        // not attached.
        let slots = build_timeline_order(&[first, second], &[Some(0), Some(1)], false, None);

        let m2_shown = slots
            .iter()
            .any(|s| matches!(s, TimelineSlot::Message { key, .. } if key == "m2"));
        assert!(
            !m2_shown,
            "presentation-deduped message must not render: {slots:?}"
        );
        // Group 1 still appears exactly once — as an orphan.
        assert_eq!(count_group(&slots, Some(1)), 1);
    }

    /// `None` sequences sort first (matching the desktop `BTreeMap<Option<i64>>`).
    #[test]
    fn none_sequence_sorts_before_some() {
        let mut none_msg = msg("none", 0, TimelineRole::Assistant);
        none_msg.sequence = None;
        let some_msg = msg("some", 5, TimelineRole::Assistant);
        let slots = build_timeline_order(&[some_msg, none_msg], &[], false, None);
        let none_pos = slots
            .iter()
            .position(|s| matches!(s, TimelineSlot::Message { key, .. } if key == "none"));
        let some_pos = slots
            .iter()
            .position(|s| matches!(s, TimelineSlot::Message { key, .. } if key == "some"));
        assert!(
            none_pos < some_pos,
            "None sequence must sort first: {slots:?}"
        );
    }
}
