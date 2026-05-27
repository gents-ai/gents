use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use defra_agent::graphql::escape_graphql_string;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::protocol::{absolute_path, thread_json};
use super::store::query_node_json;
use super::ShimState;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(super) struct CodexThreadRecord {
    pub(super) session_id: String,
    pub(super) cwd: PathBuf,
    pub(super) archived: bool,
    pub(super) loaded: bool,
    pub(super) memory_mode: String,
    pub(super) name: String,
    pub(super) settings_json: String,
    pub(super) goal_json: String,
    pub(super) git_info_json: String,
    pub(super) conversation: Option<ConversationRow>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub(super) struct ConversationRow {
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) preview_text: String,
    #[serde(default)]
    pub(super) status: String,
    #[serde(default)]
    pub(super) created_at: Option<String>,
    #[serde(default)]
    pub(super) updated_at: Option<String>,
    #[serde(default)]
    pub(super) latest_request_id: String,
    #[serde(default)]
    pub(super) forked_from_session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectionRow {
    session_id: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    loaded: bool,
    #[serde(default = "default_memory_mode")]
    memory_mode: String,
    #[serde(default)]
    name: String,
    #[serde(default = "empty_json_object")]
    settings_json: String,
    #[serde(default = "empty_json_object")]
    goal_json: String,
    #[serde(default = "empty_json_object")]
    git_info_json: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredGoal {
    thread_id: String,
    objective: String,
    status: codex::ThreadGoalStatus,
    token_budget: Option<i64>,
    tokens_used: i64,
    time_used_seconds: i64,
    created_at: i64,
    updated_at: i64,
}

pub(super) async fn create_codex_thread(
    state: &ShimState,
    thread_id: &str,
    cwd: &Path,
) -> Result<CodexThreadRecord> {
    ensure_agent_session(state, thread_id).await?;
    upsert_projection(
        state,
        &ProjectionUpdate {
            session_id: thread_id,
            cwd,
            archived: false,
            loaded: true,
            memory_mode: "enabled",
            name: "",
            settings_json: "{}",
            goal_json: "{}",
            rollback_user_turn: -1,
            git_info_json: "{}",
        },
    )
    .await?;
    load_codex_thread(state, thread_id)
        .await?
        .with_context(|| format!("loading newly-created Codex thread {thread_id}"))
}

pub(super) async fn resume_codex_thread(
    state: &ShimState,
    thread_id: &str,
    cwd_override: Option<&str>,
) -> Result<CodexThreadRecord> {
    let cwd = cwd_override
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| state.cwd.clone());

    if load_projection(state, thread_id).await?.is_none()
        && load_conversation(state, thread_id).await?.is_none()
    {
        ensure_agent_session(state, thread_id).await?;
    }

    match load_projection(state, thread_id).await? {
        Some(existing) => {
            let cwd = cwd_override
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(existing.cwd));
            update_projection_loaded_cwd(state, thread_id, true, &cwd).await?;
        }
        None => {
            upsert_projection(
                state,
                &ProjectionUpdate {
                    session_id: thread_id,
                    cwd: &cwd,
                    archived: false,
                    loaded: true,
                    memory_mode: "enabled",
                    name: "",
                    settings_json: "{}",
                    goal_json: "{}",
                    rollback_user_turn: -1,
                    git_info_json: "{}",
                },
            )
            .await?;
        }
    }

    load_codex_thread(state, thread_id)
        .await?
        .with_context(|| format!("loading resumed Codex thread {thread_id}"))
}

pub(super) async fn load_codex_thread(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<CodexThreadRecord>> {
    let conversation = load_conversation(state, thread_id).await?;
    let projection = load_projection(state, thread_id).await?;

    match (conversation, projection) {
        (None, None) => Ok(None),
        (conversation, Some(projection)) => Ok(Some(CodexThreadRecord {
            session_id: projection.session_id,
            cwd: PathBuf::from(projection.cwd),
            archived: projection.archived,
            loaded: projection.loaded,
            memory_mode: projection.memory_mode,
            name: projection.name,
            settings_json: projection.settings_json,
            goal_json: projection.goal_json,
            git_info_json: projection.git_info_json,
            conversation,
        })),
        (Some(conversation), None) => {
            upsert_projection(
                state,
                &ProjectionUpdate {
                    session_id: thread_id,
                    cwd: &state.cwd,
                    archived: false,
                    loaded: false,
                    memory_mode: "enabled",
                    name: "",
                    settings_json: "{}",
                    goal_json: "{}",
                    rollback_user_turn: -1,
                    git_info_json: "{}",
                },
            )
            .await?;
            Ok(Some(CodexThreadRecord {
                session_id: thread_id.to_string(),
                cwd: state.cwd.clone(),
                archived: false,
                loaded: false,
                memory_mode: "enabled".to_string(),
                name: String::new(),
                settings_json: "{}".to_string(),
                goal_json: "{}".to_string(),
                git_info_json: "{}".to_string(),
                conversation: Some(conversation),
            }))
        }
    }
}

pub(super) async fn list_codex_threads(state: &ShimState) -> Result<Vec<CodexThreadRecord>> {
    list_codex_threads_by_archived(state, false).await
}

pub(super) async fn list_codex_threads_by_archived(
    state: &ShimState,
    archived: bool,
) -> Result<Vec<CodexThreadRecord>> {
    let response = query_node_json(
        &state.node,
        &format!(
            r#"{{
            CodexThreadProjection(
                filter: {{ archived: {{ _eq: {archived} }} }},
                order: {{ updated_at: DESC }}
            ) {{
                session_id cwd archived loaded memory_mode name settings_json goal_json git_info_json
            }}
        }}"#
        ),
    )
    .await?;
    let rows = response
        .pointer("/data/CodexThreadProjection")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut records = Vec::with_capacity(rows.len());
    for row in rows {
        let projection: ProjectionRow = serde_json::from_value(row)
            .context("decoding CodexThreadProjection row for thread list")?;
        let conversation = load_conversation(state, &projection.session_id).await?;
        records.push(CodexThreadRecord {
            session_id: projection.session_id,
            cwd: PathBuf::from(projection.cwd),
            archived: projection.archived,
            loaded: projection.loaded,
            memory_mode: projection.memory_mode,
            name: projection.name,
            settings_json: projection.settings_json,
            goal_json: projection.goal_json,
            git_info_json: projection.git_info_json,
            conversation,
        });
    }

    Ok(records)
}

pub(super) async fn store_forked_codex_thread(
    state: &ShimState,
    source: &CodexThreadRecord,
    child_session_id: &str,
    cwd: &Path,
) -> Result<CodexThreadRecord> {
    upsert_projection(
        state,
        &ProjectionUpdate {
            session_id: child_session_id,
            cwd,
            archived: false,
            loaded: true,
            memory_mode: &source.memory_mode,
            name: "",
            settings_json: &source.settings_json,
            goal_json: "{}",
            rollback_user_turn: -1,
            git_info_json: &source.git_info_json,
        },
    )
    .await?;
    load_codex_thread(state, child_session_id)
        .await?
        .with_context(|| format!("loading forked Codex thread {child_session_id}"))
}

pub(super) async fn loaded_codex_thread_ids(state: &ShimState) -> Result<Vec<String>> {
    let response = query_node_json(
        &state.node,
        r#"{
            CodexThreadProjection(
                filter: { loaded: { _eq: true }, archived: { _eq: false } },
                order: { updated_at: DESC }
            ) { session_id }
        }"#,
    )
    .await?;
    Ok(response
        .pointer("/data/CodexThreadProjection")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            row.get("session_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect())
}

pub(super) async fn set_codex_thread_loaded(
    state: &ShimState,
    thread_id: &str,
    loaded: bool,
) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ loaded: {loaded}, updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

pub(super) async fn set_codex_thread_archived(
    state: &ShimState,
    thread_id: &str,
    archived: bool,
) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ archived: {archived}, loaded: false, updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

pub(super) async fn set_codex_thread_name(
    state: &ShimState,
    thread_id: &str,
    name: &str,
) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let escaped_name = escape_graphql_string(name.trim());
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ name: "{escaped_name}", updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

pub(super) async fn set_codex_thread_memory_mode(
    state: &ShimState,
    thread_id: &str,
    mode: codex::ThreadMemoryMode,
) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let escaped_mode = escape_graphql_string(mode.as_str());
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ memory_mode: "{escaped_mode}", updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

pub(super) async fn set_codex_thread_settings(
    state: &ShimState,
    thread_id: &str,
    settings: &codex::ThreadSettingsUpdateParams,
) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let settings_json =
        serde_json::to_string(settings).context("encoding Codex thread settings")?;
    let escaped_settings = escape_graphql_string(&settings_json);
    let cwd_update = settings
        .cwd
        .as_deref()
        .map(|cwd| {
            let cwd = if cwd.is_absolute() {
                cwd.to_path_buf()
            } else {
                state.cwd.join(cwd)
            };
            format!(
                r#", cwd: "{}""#,
                escape_graphql_string(&absolute_path(&cwd))
            )
        })
        .unwrap_or_default();
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ settings_json: "{escaped_settings}"{cwd_update}, updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

pub(super) async fn set_codex_thread_git_info(
    state: &ShimState,
    thread_id: &str,
    git_info: &Option<codex::ThreadMetadataGitInfoUpdateParams>,
) -> Result<Option<CodexThreadRecord>> {
    let git_info_json = serde_json::to_string(git_info).context("encoding Codex git metadata")?;
    let escaped_thread_id = escape_graphql_string(thread_id);
    let escaped_git_info = escape_graphql_string(&git_info_json);
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ git_info_json: "{escaped_git_info}", updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    load_codex_thread(state, thread_id).await
}

pub(super) async fn set_codex_thread_goal(
    state: &ShimState,
    params: &codex::ThreadGoalSetParams,
) -> Result<codex::ThreadGoal> {
    let existing = load_projection(state, &params.thread_id)
        .await?
        .and_then(|projection| decode_stored_goal(&projection.goal_json).ok().flatten());
    let now = now_seconds_i64();
    let status = params
        .status
        .as_ref()
        .copied()
        .or_else(|| existing.as_ref().map(|goal| goal.status.clone()))
        .unwrap_or(codex::ThreadGoalStatus::Active);
    let token_budget = match &params.token_budget {
        Some(value) => *value,
        None => existing.as_ref().and_then(|goal| goal.token_budget),
    };
    let created_at = existing.as_ref().map(|goal| goal.created_at).unwrap_or(now);
    let goal = StoredGoal {
        thread_id: params.thread_id.clone(),
        objective: params
            .objective
            .clone()
            .or_else(|| existing.as_ref().map(|goal| goal.objective.clone()))
            .unwrap_or_default(),
        status,
        token_budget,
        tokens_used: existing.as_ref().map(|goal| goal.tokens_used).unwrap_or(0),
        time_used_seconds: existing
            .as_ref()
            .map(|goal| goal.time_used_seconds)
            .unwrap_or(0),
        created_at,
        updated_at: now,
    };
    let goal_json = serde_json::to_string(&goal).context("encoding Codex thread goal")?;
    update_goal_json(state, &params.thread_id, &goal_json).await?;
    Ok(goal.into_codex())
}

pub(super) async fn get_codex_thread_goal(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<codex::ThreadGoal>> {
    let projection = load_projection(state, thread_id).await?;
    projection
        .map(|projection| decode_stored_goal(&projection.goal_json))
        .transpose()
        .map(Option::flatten)
        .map(|goal| goal.map(StoredGoal::into_codex))
}

pub(super) async fn clear_codex_thread_goal(state: &ShimState, thread_id: &str) -> Result<bool> {
    let had_goal = get_codex_thread_goal(state, thread_id).await?.is_some();
    update_goal_json(state, thread_id, "{}").await?;
    Ok(had_goal)
}

pub(super) fn codex_thread_json(record: &CodexThreadRecord, _include_turns: bool) -> Value {
    codex_thread_json_with_turns(record, Vec::new())
}

pub(super) fn codex_thread_json_with_turns(
    record: &CodexThreadRecord,
    turns: Vec<codex::Turn>,
) -> Value {
    let conversation = record.conversation.as_ref();
    let preview = conversation.and_then(|conversation| {
        let preview = conversation.preview_text.trim();
        (!preview.is_empty()).then_some(preview)
    });
    let mut thread = thread_json(
        &record.cwd,
        &record.session_id,
        preview,
        codex::ThreadStatus::Idle,
        turns,
    );
    let object = thread
        .as_object_mut()
        .expect("thread_json returns an object");
    if !record.name.trim().is_empty() {
        object.insert("name".to_string(), Value::String(record.name.clone()));
    }
    if let Some(conversation) = conversation {
        if record.name.trim().is_empty() && !conversation.title.trim().is_empty() {
            object.insert(
                "name".to_string(),
                Value::String(conversation.title.clone()),
            );
        }
        if preview.is_none() && !conversation.title.trim().is_empty() {
            object.insert(
                "preview".to_string(),
                Value::String(conversation.title.clone()),
            );
        }
        if let Some(parent) = conversation
            .forked_from_session_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            object.insert(
                "forkedFromId".to_string(),
                Value::String(parent.to_string()),
            );
        }
    }
    if let Some(git_info) = codex_git_info_json(&record.git_info_json) {
        object.insert("gitInfo".to_string(), git_info);
    }
    thread
}

pub(super) fn thread_start_response_json(state: &ShimState, record: &CodexThreadRecord) -> Value {
    thread_response_json(state, record, codex_thread_json(record, false))
}

pub(super) fn thread_response_json(
    state: &ShimState,
    record: &CodexThreadRecord,
    thread: Value,
) -> Value {
    json!({
        "thread": thread,
        "model": state.model.as_ref(),
        "modelProvider": "defra",
        "serviceTier": null,
        "cwd": absolute_path(&record.cwd),
        "runtimeWorkspaceRoots": [],
        "instructionSources": [],
        "approvalPolicy": "never",
        "approvalsReviewer": "user",
        "sandbox": { "type": "dangerFullAccess" },
        "activePermissionProfile": null,
        "reasoningEffort": null
    })
}

pub(super) fn thread_resume_response_json(state: &ShimState, record: &CodexThreadRecord) -> Value {
    thread_start_response_json(state, record)
}

struct ProjectionUpdate<'a> {
    session_id: &'a str,
    cwd: &'a Path,
    archived: bool,
    loaded: bool,
    memory_mode: &'a str,
    name: &'a str,
    settings_json: &'a str,
    goal_json: &'a str,
    rollback_user_turn: i32,
    git_info_json: &'a str,
}

async fn ensure_agent_session(state: &ShimState, session_id: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(session_id);
    let agent_name = agent_name(state);
    let behavior_id = behavior_id(state);
    let escaped_agent_name = escape_graphql_string(&agent_name);
    let escaped_behavior_id = escape_graphql_string(&behavior_id);
    let mutation = format!(
        r#"mutation {{
            upsert_AgentSession(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                add: {{
                    session_id: "{escaped_session_id}",
                    agent_name: "{escaped_agent_name}",
                    behavior_id: "{escaped_behavior_id}",
                    started: "{now}",
                    status: "active"
                }},
                update: {{
                    agent_name: "{escaped_agent_name}",
                    behavior_id: "{escaped_behavior_id}",
                    status: "active"
                }}
            ) {{ _docID }}
        }}"#
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

async fn upsert_projection(state: &ShimState, update: &ProjectionUpdate<'_>) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(update.session_id);
    let escaped_cwd = escape_graphql_string(&absolute_path(update.cwd));
    let escaped_memory_mode = escape_graphql_string(update.memory_mode);
    let escaped_name = escape_graphql_string(update.name);
    let escaped_settings_json = escape_graphql_string(update.settings_json);
    let escaped_goal_json = escape_graphql_string(update.goal_json);
    let escaped_git_info_json = escape_graphql_string(update.git_info_json);
    let mutation = format!(
        r#"mutation {{
            upsert_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                add: {{
                    session_id: "{escaped_session_id}",
                    cwd: "{escaped_cwd}",
                    archived: {archived},
                    loaded: {loaded},
                    memory_mode: "{escaped_memory_mode}",
                    name: "{escaped_name}",
                    settings_json: "{escaped_settings_json}",
                    goal_json: "{escaped_goal_json}",
                    rollback_user_turn: {rollback_user_turn},
                    git_info_json: "{escaped_git_info_json}",
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    cwd: "{escaped_cwd}",
                    archived: {archived},
                    loaded: {loaded},
                    memory_mode: "{escaped_memory_mode}",
                    name: "{escaped_name}",
                    settings_json: "{escaped_settings_json}",
                    goal_json: "{escaped_goal_json}",
                    rollback_user_turn: {rollback_user_turn},
                    git_info_json: "{escaped_git_info_json}",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#,
        archived = update.archived,
        loaded = update.loaded,
        rollback_user_turn = update.rollback_user_turn,
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

async fn update_projection_loaded_cwd(
    state: &ShimState,
    thread_id: &str,
    loaded: bool,
    cwd: &Path,
) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let escaped_cwd = escape_graphql_string(&absolute_path(cwd));
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ loaded: {loaded}, cwd: "{escaped_cwd}", updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

async fn load_projection(state: &ShimState, thread_id: &str) -> Result<Option<ProjectionRow>> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let query = format!(
        r#"{{
            CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                limit: 1
            ) {{
                session_id cwd archived loaded memory_mode name settings_json goal_json git_info_json
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    response
        .pointer("/data/CodexThreadProjection")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("decoding CodexThreadProjection row")
}

async fn load_conversation(state: &ShimState, thread_id: &str) -> Result<Option<ConversationRow>> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                limit: 1
            ) {{
                title preview_text status created_at updated_at latest_request_id forked_from_session_id
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    response
        .pointer("/data/AgentConversation")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("decoding AgentConversation row")
}

async fn update_goal_json(state: &ShimState, thread_id: &str, goal_json: &str) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let escaped_goal_json = escape_graphql_string(goal_json);
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ goal_json: "{escaped_goal_json}", updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

impl StoredGoal {
    fn into_codex(self) -> codex::ThreadGoal {
        codex::ThreadGoal {
            thread_id: self.thread_id,
            objective: self.objective,
            status: self.status,
            token_budget: self.token_budget,
            tokens_used: self.tokens_used,
            time_used_seconds: self.time_used_seconds,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

fn decode_stored_goal(raw: &str) -> Result<Option<StoredGoal>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "{}" || trimmed == "null" {
        return Ok(None);
    }
    let goal =
        serde_json::from_str::<StoredGoal>(trimmed).context("decoding stored Codex thread goal")?;
    Ok(Some(goal))
}

fn codex_git_info_json(raw: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    let object = value.as_object()?;
    if object.is_empty() {
        return None;
    }
    let string_field = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    };
    Some(json!({
        "sha": string_field("sha"),
        "branch": string_field("branch"),
        "originUrl": string_field("originUrl").or_else(|| string_field("origin_url")),
    }))
}

fn behavior_id(state: &ShimState) -> String {
    state
        .behavior_id
        .as_deref()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}:default", state.agent_did.as_ref()))
}

fn agent_name(state: &ShimState) -> String {
    behavior_id(state)
}

fn default_memory_mode() -> String {
    "enabled".to_string()
}

fn empty_json_object() -> String {
    "{}".to_string()
}

fn now_seconds_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}
