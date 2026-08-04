use std::path::PathBuf;

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents::session::{fork, ForkError, ForkParams};
use gents_codex_protocol as codex;
use serde_json::{json, Value};

use super::bound_behavior::load_bound_model_selection_id_for_state;
use super::history_projection::load_thread_turns;
use super::protocol::absolute_path;
use super::store::query_node_json;
use super::thread_projection::{
    codex_thread_json, codex_thread_json_with_turns, list_codex_threads_for_sources,
    load_codex_thread, store_forked_codex_thread, thread_response_json, CodexThreadRecord,
};
use super::{ShimState, JSONRPC_INTERNAL_ERROR, JSONRPC_INVALID_PARAMS};

#[derive(Debug)]
pub(super) struct ThreadRouteError {
    pub(super) code: i64,
    pub(super) message: String,
}

pub(super) async fn fork_thread_response(
    state: &ShimState,
    params: codex::ThreadForkParams,
) -> std::result::Result<(CodexThreadRecord, Value), ThreadRouteError> {
    if params.path.is_some() {
        return Err(invalid_params(
            "thread/fork by rollout path is unavailable for GENTS-backed Codex threads",
        ));
    }
    if params.sandbox.is_some() && params.permissions.is_some() {
        return Err(invalid_params(
            "`permissions` cannot be combined with `sandbox`",
        ));
    }

    let source = load_codex_thread(state, &params.thread_id)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| invalid_params(format!("unknown Codex thread `{}`", params.thread_id)))?;
    let fork_at_user_turn = count_user_messages(state, &params.thread_id)
        .await
        .map_err(internal_error)?;
    let outcome = fork(
        &state.node,
        ForkParams {
            source_session_id: &params.thread_id,
            fork_at_user_turn,
            caller_agent_did: state.agent_did.as_ref(),
            target_behavior_id: None,
        },
    )
    .await
    .map_err(map_fork_error)?;

    let cwd = params
        .cwd
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|cwd| resolve_cwd(&source.cwd, cwd))
        .unwrap_or_else(|| source.cwd.clone());
    let record = store_forked_codex_thread(state, &source, &outcome.session_id, &cwd)
        .await
        .map_err(internal_error)?;
    let turns = if params.exclude_turns {
        Vec::new()
    } else {
        load_thread_turns(state, &record)
            .await
            .map_err(internal_error)?
    };
    let thread = codex_thread_json_with_turns(&record, turns);
    let bound_model_id =
        load_bound_model_selection_id_for_state(state.node.as_ref(), &state.behavior_id)
            .await
            .map_err(internal_error)?;
    let response = thread_response_json(&record, thread, &bound_model_id);
    Ok((record, response))
}

pub(super) async fn list_threads_response(
    state: &ShimState,
    params: codex::ThreadListParams,
) -> std::result::Result<Value, ThreadRouteError> {
    let include_cli = source_filter_allows_cli(params.source_kinds.as_deref());
    let include_subagents = source_filter_allows_spawned_subagent(params.source_kinds.as_deref());
    if (!include_cli && !include_subagents)
        || !model_provider_filter_allows_gents(params.model_providers.as_deref())
    {
        return Ok(json!({
            "data": [],
            "nextCursor": null,
            "backwardsCursor": null
        }));
    }

    let archived = params.archived.unwrap_or(false);
    let mut records =
        list_codex_threads_for_sources(state, archived, include_cli, include_subagents)
            .await
            .map_err(internal_error)?;
    if let Some(cwd_filter) = params.cwd.as_ref() {
        let allowed = cwd_filter_values(cwd_filter);
        records.retain(|record| allowed.iter().any(|cwd| cwd_matches_record(cwd, record)));
    }
    if let Some(search_term) = params
        .search_term
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        records.retain(|record| record_snippet(record, search_term).is_some());
    }

    sort_thread_records(&mut records, params.sort_key, params.sort_direction);

    let page_size = params.limit.unwrap_or(50).clamp(1, 200) as usize;
    let start_index = params
        .cursor
        .as_deref()
        .and_then(|cursor| {
            records
                .iter()
                .position(|record| record.session_id == cursor)
                .map(|index| index + 1)
        })
        .unwrap_or(0);
    let remaining = records.len().saturating_sub(start_index);
    let page_len = remaining.min(page_size);
    let has_more = remaining > page_size;
    let page = records.into_iter().skip(start_index).take(page_len);

    let mut data = Vec::with_capacity(page_len);
    let mut first_id = None::<String>;
    let mut last_id = None::<String>;
    for record in page {
        first_id.get_or_insert_with(|| record.session_id.clone());
        last_id = Some(record.session_id.clone());
        data.push(codex_thread_json(&record, false));
    }

    Ok(json!({
        "data": data,
        "nextCursor": if has_more { last_id } else { None },
        "backwardsCursor": first_id
    }))
}

pub(super) async fn search_threads_response(
    state: &ShimState,
    params: codex::ThreadSearchParams,
) -> std::result::Result<Value, ThreadRouteError> {
    let search_term = params.search_term.trim();
    if search_term.is_empty() {
        return Err(invalid_params(
            "thread/search requires a non-empty searchTerm",
        ));
    }
    let include_cli = source_filter_allows_cli(params.source_kinds.as_deref());
    let include_subagents = source_filter_allows_spawned_subagent(params.source_kinds.as_deref());
    if !include_cli && !include_subagents {
        return Ok(json!({
            "data": [],
            "nextCursor": null,
            "backwardsCursor": null
        }));
    }

    let mut matches = Vec::<(CodexThreadRecord, String)>::new();
    let archived = params.archived.unwrap_or(false);
    let records = list_codex_threads_for_sources(state, archived, include_cli, include_subagents)
        .await
        .map_err(internal_error)?;
    for record in records {
        if let Some(snippet) = record_snippet(&record, search_term) {
            matches.push((record, snippet));
            continue;
        }

        let turns = load_thread_turns(state, &record)
            .await
            .map_err(internal_error)?;
        if let Some(snippet) = turns_snippet(&turns, search_term) {
            matches.push((record, snippet));
        }
    }

    sort_thread_matches(&mut matches, params.sort_key, params.sort_direction);

    let page_size = params.limit.unwrap_or(50).clamp(1, 200) as usize;
    let start_index = params
        .cursor
        .as_deref()
        .and_then(|cursor| {
            matches
                .iter()
                .position(|(record, _)| record.session_id == cursor)
                .map(|index| index + 1)
        })
        .unwrap_or(0);
    let remaining = matches.len().saturating_sub(start_index);
    let page_len = remaining.min(page_size);
    let has_more = remaining > page_size;
    let page = matches.into_iter().skip(start_index).take(page_len);

    let mut data = Vec::with_capacity(page_len);
    let mut first_id = None::<String>;
    let mut last_id = None::<String>;
    for (record, snippet) in page {
        first_id.get_or_insert_with(|| record.session_id.clone());
        last_id = Some(record.session_id.clone());
        data.push(json!({
            "thread": codex_thread_json(&record, false),
            "snippet": snippet
        }));
    }

    Ok(json!({
        "data": data,
        "nextCursor": if has_more { last_id } else { None },
        "backwardsCursor": first_id
    }))
}

fn model_provider_filter_allows_gents(model_providers: Option<&[String]>) -> bool {
    model_providers
        .filter(|providers| !providers.is_empty())
        .is_none_or(|providers| providers.iter().any(|provider| provider == "gents"))
}

fn cwd_filter_values(filter: &codex::ThreadListCwdFilter) -> Vec<&str> {
    match filter {
        codex::ThreadListCwdFilter::One(cwd) => vec![cwd.as_str()],
        codex::ThreadListCwdFilter::Many(cwds) => cwds.iter().map(String::as_str).collect(),
    }
}

fn cwd_matches_record(cwd: &str, record: &CodexThreadRecord) -> bool {
    let cwd = cwd.trim();
    !cwd.is_empty() && cwd == absolute_path(&record.cwd)
}

fn sort_thread_records(
    records: &mut [CodexThreadRecord],
    sort_key: Option<codex::ThreadSortKey>,
    sort_direction: Option<codex::SortDirection>,
) {
    let sort_key = sort_key.unwrap_or(codex::ThreadSortKey::CreatedAt);
    let sort_direction = sort_direction.unwrap_or(codex::SortDirection::Desc);
    records.sort_by(|left, right| compare_thread_records(left, right, sort_key, sort_direction));
}

fn sort_thread_matches(
    matches: &mut [(CodexThreadRecord, String)],
    sort_key: Option<codex::ThreadSortKey>,
    sort_direction: Option<codex::SortDirection>,
) {
    let sort_key = sort_key.unwrap_or(codex::ThreadSortKey::CreatedAt);
    let sort_direction = sort_direction.unwrap_or(codex::SortDirection::Desc);
    matches.sort_by(|(left, _), (right, _)| {
        compare_thread_records(left, right, sort_key, sort_direction)
    });
}

fn compare_thread_records(
    left: &CodexThreadRecord,
    right: &CodexThreadRecord,
    sort_key: codex::ThreadSortKey,
    sort_direction: codex::SortDirection,
) -> std::cmp::Ordering {
    let left_key = thread_sort_timestamp(left, sort_key);
    let right_key = thread_sort_timestamp(right, sort_key);
    let ordering = match sort_direction {
        codex::SortDirection::Asc => left_key
            .cmp(&right_key)
            .then_with(|| left.session_id.cmp(&right.session_id)),
        codex::SortDirection::Desc => right_key
            .cmp(&left_key)
            .then_with(|| right.session_id.cmp(&left.session_id)),
    };
    ordering
}

fn thread_sort_timestamp(record: &CodexThreadRecord, sort_key: codex::ThreadSortKey) -> String {
    let conversation = record.conversation.as_ref();
    match sort_key {
        codex::ThreadSortKey::CreatedAt => conversation
            .and_then(|conversation| conversation.created_at.clone())
            .or_else(|| record.projection_started.clone()),
        codex::ThreadSortKey::UpdatedAt => conversation
            .and_then(|conversation| conversation.updated_at.clone())
            .or_else(|| conversation.and_then(|conversation| conversation.created_at.clone()))
            .or_else(|| record.projection_started.clone()),
    }
    .unwrap_or_default()
}

async fn count_user_messages(state: &ShimState, thread_id: &str) -> Result<u32> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{escaped_thread_id}" }},
                    role: {{ _eq: "user" }}
                }}
            ) {{ sequence }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    let count = response
        .pointer("/data/AgentMessage")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    u32::try_from(count).context("user message count exceeds u32")
}

fn resolve_cwd(source_cwd: &std::path::Path, cwd: &str) -> PathBuf {
    let cwd = PathBuf::from(cwd);
    if cwd.is_absolute() {
        cwd
    } else {
        source_cwd.join(cwd)
    }
}

fn source_filter_allows_cli(source_kinds: Option<&[codex::ThreadSourceKind]>) -> bool {
    source_kinds
        .filter(|kinds| !kinds.is_empty())
        .is_none_or(|kinds| kinds.contains(&codex::ThreadSourceKind::Cli))
}

fn source_filter_allows_spawned_subagent(source_kinds: Option<&[codex::ThreadSourceKind]>) -> bool {
    source_kinds.is_some_and(|kinds| {
        kinds.iter().any(|kind| {
            matches!(
                kind,
                codex::ThreadSourceKind::SubAgent | codex::ThreadSourceKind::SubAgentThreadSpawn
            )
        })
    })
}

fn record_snippet(record: &CodexThreadRecord, needle: &str) -> Option<String> {
    field_snippet(&record.name, needle)
        .or_else(|| {
            record
                .conversation
                .as_ref()
                .and_then(|conversation| field_snippet(&conversation.title, needle))
        })
        .or_else(|| {
            record
                .conversation
                .as_ref()
                .and_then(|conversation| field_snippet(&conversation.preview_text, needle))
        })
}

fn turns_snippet(turns: &[codex::Turn], needle: &str) -> Option<String> {
    turns
        .iter()
        .flat_map(|turn| turn.items.iter())
        .flat_map(item_snippets)
        .find_map(|snippet| field_snippet(&snippet, needle))
}

fn item_snippets(item: &codex::ThreadItem) -> Vec<String> {
    match item {
        codex::ThreadItem::UserMessage { content, .. } => content
            .iter()
            .filter_map(|input| match input {
                codex::UserInput::Text { text, .. } => Some(text.clone()),
                codex::UserInput::Image { url, .. } => Some(url.clone()),
                codex::UserInput::LocalImage { path, .. }
                | codex::UserInput::Skill { path, .. } => Some(path.display().to_string()),
                codex::UserInput::Mention { name, path } => Some(format!("{name} {path}")),
            })
            .collect(),
        codex::ThreadItem::AgentMessage { text, .. } => vec![text.clone()],
        codex::ThreadItem::Reasoning {
            summary, content, ..
        } => summary.iter().chain(content.iter()).cloned().collect(),
        codex::ThreadItem::CommandExecution {
            command,
            aggregated_output,
            ..
        } => {
            let mut snippets = vec![command.clone()];
            if let Some(output) = aggregated_output {
                snippets.push(output.clone());
            }
            snippets
        }
        codex::ThreadItem::McpToolCall {
            server,
            tool,
            arguments,
            ..
        } => vec![format!("{server} {tool} {arguments}")],
        codex::ThreadItem::DynamicToolCall {
            namespace,
            tool,
            arguments,
            ..
        } => vec![format!(
            "{} {tool} {arguments}",
            namespace.as_deref().unwrap_or("")
        )],
        codex::ThreadItem::Plan { text, .. } => vec![text.clone()],
        codex::ThreadItem::HookPrompt { fragments, .. } => fragments
            .iter()
            .map(|fragment| fragment.text.clone())
            .collect(),
        codex::ThreadItem::WebSearch { query, .. } => vec![query.clone()],
        codex::ThreadItem::EnteredReviewMode { review, .. }
        | codex::ThreadItem::ExitedReviewMode { review, .. } => vec![review.clone()],
        _ => Vec::new(),
    }
}

fn field_snippet(value: &str, needle: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || !contains_case_insensitive(value, needle) {
        return None;
    }
    Some(value.chars().take(240).collect())
}

fn contains_case_insensitive(value: &str, needle: &str) -> bool {
    value.to_lowercase().contains(&needle.to_lowercase())
}

fn map_fork_error(err: ForkError) -> ThreadRouteError {
    let code = match err {
        ForkError::ForkCopyFailed(_) => JSONRPC_INTERNAL_ERROR,
        ForkError::ForkSourceNotFound(_)
        | ForkError::ForkNotSameAgent
        | ForkError::ForkSourceBusy
        | ForkError::ForkAtUserTurnOutOfRange(_, _)
        | ForkError::ForkBehaviorNotFound(_)
        | ForkError::ForkBehaviorNotOwnedByPrincipal(_, _) => JSONRPC_INVALID_PARAMS,
    };
    ThreadRouteError {
        code,
        message: err.to_string(),
    }
}

fn invalid_params(message: impl Into<String>) -> ThreadRouteError {
    ThreadRouteError {
        code: JSONRPC_INVALID_PARAMS,
        message: message.into(),
    }
}

fn internal_error(err: anyhow::Error) -> ThreadRouteError {
    ThreadRouteError {
        code: JSONRPC_INTERNAL_ERROR,
        message: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_filters_classify_gents_spawned_children() {
        assert!(source_filter_allows_cli(None));
        assert!(source_filter_allows_cli(Some(&[])));
        assert!(!source_filter_allows_spawned_subagent(None));
        assert!(!source_filter_allows_spawned_subagent(Some(&[])));

        assert!(source_filter_allows_spawned_subagent(Some(&[
            codex::ThreadSourceKind::SubAgent,
        ])));
        assert!(source_filter_allows_spawned_subagent(Some(&[
            codex::ThreadSourceKind::SubAgentThreadSpawn,
        ])));
        assert!(!source_filter_allows_spawned_subagent(Some(&[
            codex::ThreadSourceKind::Cli,
            codex::ThreadSourceKind::SubAgentReview,
        ])));
    }
}
