//! Fenced by `proofs/Proofs/Clients/ClientShell/Timeline.lean`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineMessageInput {
    /// Stable identity for first-wins dedup.
    pub key: String,
    /// Ordering key and tool-group attach key. `None` sorts first.
    pub sequence: Option<i64>,
    pub role: TimelineRole,
    /// Separate from dedup: a message can survive dedup, emit no slot, and
    /// still own a tool group.
    pub emits_item: bool,
    /// Opaque secondary dedup token; compared only for equality.
    pub dedup_token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurableUserOwnerInput<'a> {
    pub request_id: Option<&'a str>,
    pub is_user: bool,
    pub has_visible_content: bool,
    pub runtime_control: bool,
}

pub fn has_durable_user_owner(messages: &[DurableUserOwnerInput<'_>], request_id: &str) -> bool {
    messages.iter().any(|message| {
        message.request_id == Some(request_id)
            && message.is_user
            && message.has_visible_content
            && !message.runtime_control
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayPlacement {
    /// Append the overlay after every orphan tool group.
    Tail,
    /// Insert the overlay immediately before the orphan group belonging to the
    /// active, still-unmaterialized assistant turn.
    BeforeOrphan { message_sequence: Option<i64> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayInput {
    /// True when a durable assistant turn from the same request already owns
    /// the overlay content, in which case it must not be re-emitted.
    pub has_durable_owner: bool,
    pub placement: OverlayPlacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingPlacement {
    Tail,
    BeforeMessage { message_sequence: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInput {
    /// Exact physical request identity; equal prompt content is not identity.
    pub key: String,
    pub placement: PendingPlacement,
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
    Pending {
        key: String,
    },
    Overlay,
}

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
    pending: &[PendingInput],
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
    let mut seen_pending = std::collections::BTreeSet::new();
    let pending: Vec<_> = pending
        .iter()
        .filter(|input| seen_pending.insert(input.key.as_str()))
        .collect();
    let mut pending_placed = std::collections::BTreeSet::new();

    for message in ordered {
        if !seen_keys.insert(message.key.clone()) {
            continue;
        }
        if let Some(token) = &message.dedup_token {
            if !seen_tokens.insert(token.clone()) {
                continue;
            }
        }
        for input in &pending {
            if matches!(input.placement, PendingPlacement::BeforeMessage { message_sequence }
                if message.sequence == Some(message_sequence))
                && !pending_placed.contains(input.key.as_str())
                && (message.emits_item || group_sequences.contains(&message.sequence))
            {
                slots.push(TimelineSlot::Pending {
                    key: input.key.clone(),
                });
                pending_placed.insert(input.key.as_str());
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

    for input in &pending {
        if !pending_placed.contains(input.key.as_str()) {
            slots.push(TimelineSlot::Pending {
                key: input.key.clone(),
            });
        }
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
    let show_overlay = overlay.is_some_and(|overlay| !overlay.has_durable_owner);
    let target_orphan = overlay.and_then(|overlay| match overlay.placement {
        OverlayPlacement::Tail => None,
        OverlayPlacement::BeforeOrphan { message_sequence } => Some(message_sequence),
    });
    let mut overlay_placed = false;
    for sequence in orphans {
        if show_overlay && !overlay_placed && target_orphan == Some(sequence) {
            slots.push(TimelineSlot::Overlay);
            overlay_placed = true;
        }
        slots.push(TimelineSlot::ToolGroup {
            message_sequence: sequence,
        });
    }

    if show_overlay && !overlay_placed {
        slots.push(TimelineSlot::Overlay);
    }

    slots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_pending_timeline_cases_preserve_request_identities_and_order() {
        gents_lean_vocab::families::contracts_pending_timeline::replay(|i| {
            let role = |role: &str| match role {
                "user" => TimelineRole::User,
                "assistant" => TimelineRole::Assistant,
                role => panic!("unknown modeled role {role}"),
            };
            let role_name = |role: TimelineRole| match role {
                TimelineRole::User => "user".to_owned(),
                TimelineRole::Assistant => "assistant".to_owned(),
            };
            let messages: Vec<_> = i
                .messages
                .iter()
                .map(|row| TimelineMessageInput {
                    key: row.key.to_string(),
                    sequence: Some(row.sequence),
                    role: role(&row.role),
                    emits_item: row.emits_item,
                    dedup_token: row.token.map(|key| key.to_string()),
                })
                .collect();
            let pending: Vec<_> = i
                .pending
                .iter()
                .map(|row| PendingInput {
                    key: row.key.to_string(),
                    placement: row
                        .before_sequence
                        .map_or(PendingPlacement::Tail, |message_sequence| {
                            PendingPlacement::BeforeMessage { message_sequence }
                        }),
                })
                .collect();
            let groups: Vec<_> = i.groups.iter().map(|sequence| Some(*sequence)).collect();
            let overlay = i.overlay.as_ref().map(|overlay| OverlayInput {
                has_durable_owner: overlay.has_durable_owner,
                placement: overlay
                    .before_sequence
                    .map_or(OverlayPlacement::Tail, |sequence| {
                        OverlayPlacement::BeforeOrphan {
                            message_sequence: Some(sequence),
                        }
                    }),
            });
            let slot =
                |kind: &str, key: Option<u64>, sequence: Option<i64>, role: Option<String>| {
                    gents_lean_vocab::families::contracts_pending_timeline::TimelineSlot {
                        kind: kind.to_owned(),
                        key,
                        sequence,
                        role,
                    }
                };
            gents_lean_vocab::families::contracts_pending_timeline::TimelineOutput {
                slots: build_timeline_order(&messages, &groups, &pending, overlay)
                    .into_iter()
                    .map(|timeline_slot| match timeline_slot {
                        TimelineSlot::Message {
                            key,
                            sequence,
                            role,
                        } => slot(
                            "message",
                            Some(key.parse().unwrap()),
                            Some(sequence.unwrap()),
                            Some(role_name(role)),
                        ),
                        TimelineSlot::ToolGroup { message_sequence } => {
                            slot("tool_group", None, Some(message_sequence.unwrap()), None)
                        }
                        TimelineSlot::Pending { key } => {
                            slot("pending", Some(key.parse().unwrap()), None, None)
                        }
                        TimelineSlot::Overlay => slot("overlay", None, None, None),
                    })
                    .collect(),
            }
        });
    }

    fn msg(key: &str, sequence: Option<i64>, dedup_token: Option<&str>) -> TimelineMessageInput {
        TimelineMessageInput {
            key: key.to_string(),
            sequence,
            role: TimelineRole::Assistant,
            emits_item: true,
            dedup_token: dedup_token.map(str::to_string),
        }
    }

    #[test]
    fn durable_user_owner_requires_the_exact_request_id() {
        let messages = [
            DurableUserOwnerInput {
                request_id: None,
                is_user: true,
                has_visible_content: true,
                runtime_control: false,
            },
            DurableUserOwnerInput {
                request_id: Some("unrelated"),
                is_user: true,
                has_visible_content: true,
                runtime_control: false,
            },
        ];
        assert!(!has_durable_user_owner(&messages, "request-under-test"));
    }

    #[test]
    fn dedup_and_unsequenced_message_ordering() {
        let dup_slots = build_timeline_order(
            &[
                msg("dup", Some(0), None),
                msg("dup", Some(1), None),
                msg("other", Some(2), None),
            ],
            &[],
            &[],
            None,
        );
        assert_eq!(
            dup_slots
                .iter()
                .filter(|s| matches!(s, TimelineSlot::Message { key, .. } if key == "dup"))
                .count(),
            1
        );

        let token_slots = build_timeline_order(
            &[
                msg("m1", Some(0), Some("same")),
                msg("m2", Some(1), Some("same")),
            ],
            &[Some(0), Some(1)],
            &[],
            None,
        );
        assert!(!token_slots
            .iter()
            .any(|s| matches!(s, TimelineSlot::Message { key, .. } if key == "m2")));
        assert_eq!(
            token_slots
                .iter()
                .filter(|s| matches!(
                    s,
                    TimelineSlot::ToolGroup {
                        message_sequence: Some(1)
                    }
                ))
                .count(),
            1
        );

        let pending = [PendingInput {
            key: "request".into(),
            placement: PendingPlacement::Tail,
        }];
        let none_slots = build_timeline_order(
            &[msg("some", Some(5), None), msg("none", None, None)],
            &[],
            &pending,
            None,
        );
        assert!(matches!(
            none_slots.as_slice(),
            [
                TimelineSlot::Message { key: first, .. },
                TimelineSlot::Message { key: second, .. },
                TimelineSlot::Pending { .. },
            ] if first == "none" && second == "some"
        ));
    }
}
