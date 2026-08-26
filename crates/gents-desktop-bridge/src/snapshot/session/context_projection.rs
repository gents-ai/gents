use super::*;

pub(super) fn usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(super) fn build_session_context_view(
    store: &gents_desktop_core::client::ClientStore,
    context_store: &gents_desktop_core::client::ClientStore,
    agent_did: Option<&str>,
    behavior_id: Option<&str>,
    session_id: &str,
    durable_messages: Vec<(Option<i64>, Message)>,
    durable_message_count: usize,
    transcript_totals_exact: bool,
) -> SessionContextView {
    let behavior = behavior_id.and_then(|behavior_id| {
        store.behaviors.iter().find(|row| {
            row.behavior_id == behavior_id
                && agent_did.is_none_or(|agent_did| row.agent_did.as_deref() == Some(agent_did))
        })
    });
    let inference_profile = behavior
        .and_then(|behavior| behavior.inference_profile_id.as_deref())
        .and_then(|profile_id| {
            store
                .inference_profiles
                .iter()
                .find(|row| row.profile_id == profile_id)
        });
    let context_window = inference_profile
        .and_then(|profile| profile.context_window)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(gents::config::DEFAULT_CONTEXT_WINDOW);
    let compaction_threshold = behavior
        .and_then(|behavior| behavior.compaction_threshold)
        .unwrap_or(gents::config::DEFAULT_COMPACTION_THRESHOLD);
    let compaction_strategy = behavior
        .and_then(|behavior| normalize_optional(behavior.compaction_strategy.as_deref()))
        .unwrap_or_else(|| {
            gents::compaction::CompactionStrategy::default()
                .as_str()
                .to_string()
        });

    let mut compaction_rows = context_store
        .compaction_entries
        .iter()
        .enumerate()
        .filter(|(index, row)| {
            row.session_id.as_deref() == Some(session_id)
                && agent_did.is_none_or(|agent_did| {
                    source_matches_agent(
                        &context_store.compaction_entry_source_agent_dids,
                        *index,
                        agent_did,
                        false,
                    )
                })
        })
        .map(|(_, row)| row)
        .collect::<Vec<_>>();
    compaction_rows.sort_by(|left, right| {
        left.sequence
            .unwrap_or_default()
            .cmp(&right.sequence.unwrap_or_default())
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.compaction_key.cmp(&right.compaction_key))
    });

    let durable_message_refs = durable_messages
        .iter()
        .map(|(_, message)| message)
        .collect::<Vec<_>>();
    let estimated_durable_tokens = gents::compaction::estimate_tokens(
        &serde_json::to_string(&durable_message_refs).unwrap_or_default(),
    );
    let total_compacted_messages = compaction_rows.iter().fold(0_usize, |total, row| {
        total.saturating_add(
            row.messages_compacted
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or_default(),
        )
    });
    let compacted_through_sequence = compaction_rows
        .last()
        .and_then(|row| row.compacted_through_sequence);
    // Sequence is nillable for legacy/partially replicated rows. The cursor
    // projection is valid only when every durable message participates in the
    // same nonnegative ordered sequence space; otherwise preserve the proven
    // full-provider-view plus cumulative-count fallback.
    let usable_cursor = compacted_through_sequence.filter(|cursor| {
        *cursor >= 0
            && durable_messages
                .iter()
                .all(|(sequence, _)| sequence.is_some_and(|sequence| sequence >= 0))
    });
    let provider_input = durable_messages
        .into_iter()
        .filter(|(sequence, _)| {
            usable_cursor.is_none_or(|cursor| sequence.is_some_and(|sequence| sequence > cursor))
        })
        .map(|(_, message)| message)
        .collect::<Vec<_>>();
    let (provider_messages, _) = gents::compaction::provider_view(provider_input);
    let active_provider_messages = if usable_cursor.is_some() {
        provider_messages
    } else {
        gents::compaction::active_provider_history(provider_messages, total_compacted_messages)
    };
    let summaries = compaction_rows
        .iter()
        .filter_map(|row| row.summary.clone())
        .map(gents::compaction::bounded_summary)
        .collect::<Vec<_>>();
    let estimated_conversation_tokens =
        gents::compaction::estimate_message_tokens(&active_provider_messages).saturating_add(
            gents::prompt::estimate_compaction_summary_tokens(&summaries),
        );
    let compactions = compaction_rows
        .into_iter()
        .map(|row| SessionCompactionView {
            compaction_key: row.compaction_key.clone(),
            sequence: row.sequence,
            messages_compacted: row.messages_compacted.unwrap_or_default().max(0),
            original_tokens: row.original_tokens,
            compacted_tokens: row.compacted_tokens,
            created_at: normalize_optional(row.created_at.as_deref()),
        })
        .collect();

    SessionContextView {
        transcript_totals_exact: Some(transcript_totals_exact),
        estimated_durable_tokens: usize_to_i64(estimated_durable_tokens),
        estimated_conversation_tokens: usize_to_i64(estimated_conversation_tokens),
        context_window: usize_to_i64(context_window),
        compaction_threshold,
        compaction_threshold_tokens: usize_to_i64(gents::compaction::threshold_budget(
            context_window,
            compaction_threshold,
        )),
        compaction_strategy,
        durable_message_count: usize_to_i64(durable_message_count),
        provider_message_count: usize_to_i64(active_provider_messages.len()),
        total_compacted_messages: usize_to_i64(total_compacted_messages),
        compactions,
        last_request: None,
    }
}

pub(super) fn build_session_context_from_stores(
    store: &ClientStore,
    context_store: &ClientStore,
    agent_did: Option<&str>,
    behavior_id: Option<&str>,
    session_id: &str,
    transcript_totals_exact: bool,
) -> SessionContextView {
    let transcript = agent_did.map_or_else(
        || context_store.transcript(session_id),
        |agent_did| context_store.transcript_for_agent(session_id, agent_did),
    );
    let durable_message_count = transcript.messages.len();
    let durable_messages = transcript
        .messages
        .into_iter()
        .filter_map(|row| {
            row.role
                .as_deref()
                .zip(row.content.as_deref())
                .map(|(role, content)| {
                    (
                        row.sequence,
                        gents_protocol::transcript::decode_persisted_message(role, content),
                    )
                })
        })
        .collect::<Vec<_>>();
    build_session_context_view(
        store,
        context_store,
        agent_did,
        behavior_id,
        session_id,
        durable_messages,
        durable_message_count,
        transcript_totals_exact,
    )
}

pub fn attach_last_request_context(
    snapshot: &mut DesktopSessionSnapshot,
    request_id: String,
    call_id: String,
    call_sequence: i64,
    accounting: gents_protocol::rendered_request::ContextAccounting,
) {
    use crate::types::{SessionContextComponentsView, SessionRequestContextView};

    let reason = serde_json::to_value(accounting.compaction_reason)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_string());
    snapshot.context.last_request = Some(SessionRequestContextView {
        request_id,
        call_id,
        call_sequence,
        turn_index: usize_to_i64(accounting.turn_index),
        attempt: i64::from(accounting.attempt),
        estimator: accounting.estimator,
        estimated_input_tokens: usize_to_i64(accounting.estimated_input_tokens),
        context_window: usize_to_i64(accounting.context_window),
        compaction_threshold_tokens: usize_to_i64(accounting.compaction_threshold_tokens),
        configured_max_output_tokens: accounting
            .configured_max_output_tokens
            .and_then(|value| i64::try_from(value).ok()),
        effective_max_output_tokens: accounting
            .effective_max_output_tokens
            .and_then(|value| i64::try_from(value).ok()),
        compaction_reason: reason,
        pre_compaction_input_tokens: accounting.pre_compaction_input_tokens.map(usize_to_i64),
        components: SessionContextComponentsView {
            messages: usize_to_i64(accounting.components.messages),
            documents: usize_to_i64(accounting.components.documents),
            tool_schemas: usize_to_i64(accounting.components.tool_schemas),
            additional_parameters: usize_to_i64(accounting.components.additional_parameters),
            output_schema: usize_to_i64(accounting.components.output_schema),
        },
    });
}
