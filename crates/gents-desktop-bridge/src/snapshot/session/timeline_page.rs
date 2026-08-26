use super::*;

pub const DEFAULT_SESSION_TIMELINE_PAGE_SIZE: usize = 40;
pub const MAX_SESSION_TIMELINE_PAGE_SIZE: usize = 80;

fn rendered_timeline_item_key(item: &crate::types::RenderedTimelineItem) -> &str {
    use crate::types::RenderedTimelineItem;

    match item {
        RenderedTimelineItem::UserMessage { item_key, .. }
        | RenderedTimelineItem::AssistantMessage { item_key, .. }
        | RenderedTimelineItem::ToolGroup { item_key, .. }
        | RenderedTimelineItem::PendingUserTurn { item_key, .. }
        | RenderedTimelineItem::LiveAssistant { item_key, .. } => item_key,
    }
}

fn rendered_timeline_durable_sequence(
    item: &crate::types::RenderedTimelineItem,
) -> Option<Option<i64>> {
    use crate::types::RenderedTimelineItem;

    match item {
        RenderedTimelineItem::UserMessage { sequence, .. }
        | RenderedTimelineItem::AssistantMessage { sequence, .. } => Some(*sequence),
        RenderedTimelineItem::ToolGroup {
            message_sequence, ..
        } => Some(*message_sequence),
        RenderedTimelineItem::PendingUserTurn { .. }
        | RenderedTimelineItem::LiveAssistant { .. } => None,
    }
}

/// Bound the bridge-visible transcript while retaining an opaque, durable-row
/// cursor for explicit older-page requests. The full database-backed snapshot
/// remains authoritative inside the bridge; only IPC materialization is
/// windowed here.
pub fn apply_session_timeline_page(
    snapshot: &mut DesktopSessionSnapshot,
    before_item_key: Option<&str>,
    requested_limit: Option<usize>,
) -> Result<(), String> {
    apply_session_timeline_page_with_query(snapshot, before_item_key, requested_limit, None)
}

pub fn apply_session_timeline_page_with_query(
    snapshot: &mut DesktopSessionSnapshot,
    before_item_key: Option<&str>,
    requested_limit: Option<usize>,
    query_page: Option<&SessionTranscriptQueryPage>,
) -> Result<(), String> {
    let total_items = snapshot.timeline_items.len();
    let limit = requested_limit
        .unwrap_or(DEFAULT_SESSION_TIMELINE_PAGE_SIZE)
        .clamp(1, MAX_SESSION_TIMELINE_PAGE_SIZE);

    let (page, has_older, has_newer, oldest_item_key) = if let Some(query_page) = query_page {
        let mut sequence_counts = BTreeMap::<i64, usize>::new();
        let mut non_durable_items = 0_usize;
        for item in &snapshot.timeline_items {
            match rendered_timeline_durable_sequence(item) {
                Some(Some(sequence)) => {
                    *sequence_counts.entry(sequence).or_default() += 1;
                }
                Some(None) => {
                    return Err(
                        "queried session timeline contains an item without a durable sequence"
                            .to_string(),
                    );
                }
                None => non_durable_items += 1,
            }
        }
        if non_durable_items > limit {
            return Err(format!(
                "session timeline has {non_durable_items} live items but the visible page budget is {limit}"
            ));
        }

        let mut remaining = limit - non_durable_items;
        let mut selected_sequences = HashSet::new();
        for (&sequence, &item_count) in sequence_counts.iter().rev() {
            if item_count > remaining {
                if selected_sequences.is_empty() && non_durable_items == 0 {
                    return Err(format!(
                        "session timeline sequence group {sequence} exceeds the visible page budget of {limit}"
                    ));
                }
                break;
            }
            selected_sequences.insert(sequence);
            remaining -= item_count;
        }

        let page = snapshot
            .timeline_items
            .iter()
            .filter(|item| match rendered_timeline_durable_sequence(item) {
                Some(Some(sequence)) => selected_sequences.contains(&sequence),
                Some(None) => false,
                None => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        let oldest_sequence = selected_sequences.iter().copied().min();
        let has_unselected_sequences = selected_sequences.len() < sequence_counts.len();
        let oldest_item_key = oldest_sequence
            .and_then(|oldest_sequence| {
                page.iter().find(|item| {
                    rendered_timeline_durable_sequence(item) == Some(Some(oldest_sequence))
                })
            })
            .map(rendered_timeline_item_key)
            .map(str::to_owned)
            .or_else(|| {
                if query_page.source_exhausted && !has_unselected_sequences {
                    return None;
                }
                query_page
                    .store
                    .messages
                    .iter()
                    .filter_map(|row| row.sequence)
                    .chain(
                        query_page
                            .store
                            .tool_calls
                            .iter()
                            .filter_map(|row| row.message_sequence),
                    )
                    .min()
                    .map(|sequence| format!("tools-{sequence}"))
            });
        (
            page,
            has_unselected_sequences || !query_page.source_exhausted,
            query_page.has_newer,
            oldest_item_key,
        )
    } else {
        let end = match before_item_key {
            Some(cursor) => snapshot
                .timeline_items
                .iter()
                .position(|item| rendered_timeline_item_key(item) == cursor)
                .ok_or_else(|| format!("session timeline cursor is no longer present: {cursor}"))?,
            None => total_items,
        };
        let mut start = end.saturating_sub(limit);
        if start > 0 {
            if let Some(boundary_sequence) =
                rendered_timeline_durable_sequence(&snapshot.timeline_items[start])
            {
                if rendered_timeline_durable_sequence(&snapshot.timeline_items[start - 1])
                    == Some(boundary_sequence)
                {
                    while start < end
                        && rendered_timeline_durable_sequence(&snapshot.timeline_items[start])
                            == Some(boundary_sequence)
                    {
                        start += 1;
                    }
                    if start == end {
                        return Err(format!(
                            "session timeline sequence group {boundary_sequence:?} exceeds the visible page budget of {limit}"
                        ));
                    }
                }
            }
        }
        let page = snapshot.timeline_items[start..end].to_vec();
        let oldest_item_key = page
            .first()
            .map(rendered_timeline_item_key)
            .map(str::to_owned);
        (page, start > 0, end < total_items, oldest_item_key)
    };
    let newest_item_key = page
        .last()
        .map(rendered_timeline_item_key)
        .map(str::to_owned);
    snapshot.timeline_items = page;
    snapshot.timeline_page = Some(SessionTimelinePageView {
        total_items: query_page.map_or_else(|| usize_to_i64(total_items), |_| -1),
        total_items_exact: query_page.map(|_| false),
        page_items: usize_to_i64(snapshot.timeline_items.len()),
        has_older,
        has_newer,
        oldest_item_key,
        newest_item_key,
        query_count: query_page.map(|page| page.query_count as i64),
        queried_rows: query_page.map(|page| usize_to_i64(page.queried_rows)),
        message_query_limit: query_page.map(|page| usize_to_i64(page.message_query_limit)),
        tool_call_query_limit: query_page.map(|page| usize_to_i64(page.tool_call_query_limit)),
    });
    Ok(())
}
