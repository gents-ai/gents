use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use gents::graphql::escape_graphql_string;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::cli::args::{BackgroundCommand, BackgroundListArgs};
use crate::cli::output_format::OutputFormat;
use crate::config_writes::ConfigAccess;
use crate::request_helpers::parse_duration_suffix;
use crate::{
    graphql_rows_or_empty_if_collection_missing, normalize_optional_string, print_json,
    resolve_config_access,
};

pub(crate) async fn dispatch(command: BackgroundCommand) -> Result<()> {
    match command {
        BackgroundCommand::List(args) => background_list(args).await,
    }
}

async fn background_list(args: BackgroundListArgs) -> Result<()> {
    let now = Utc::now();
    let age_cutoff = age_cutoff(args.age_gt.as_deref(), now)?;
    let (access, _home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), false).await?;
    let tool_calls = load_background_tool_calls(&access, &args, age_cutoff).await?;
    let liveness = match &access {
        ConfigAccess::Graphql(graphql) => {
            crate::commands::status::load_liveness_value(graphql, "").await
        }
        ConfigAccess::Local(_) => Value::Null,
    };
    let liveness = RuntimeLivenessView::from_value(&liveness);
    let rows = build_output_rows(now, tool_calls, &liveness);
    let output = BackgroundListOutput {
        generated_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        filters: BackgroundListFilters {
            request_id: normalize_optional_string(args.request_id.as_deref()),
            state: normalize_optional_string(args.state.as_deref()),
            age_gt: normalize_optional_string(args.age_gt.as_deref()),
        },
        active_native_executors_available: liveness.active_native_executors_available,
        count: rows.len(),
        items: rows,
    };

    match args.output.ensure_supported(
        "background list",
        &[OutputFormat::Table, OutputFormat::Json],
    )? {
        OutputFormat::Json => print_json(&serde_json::to_value(output)?),
        OutputFormat::Table => {
            print_background_table(&output.items, output.active_native_executors_available);
            Ok(())
        }
        _ => unreachable!("ensure_supported restricts background list output formats"),
    }
}

async fn load_background_tool_calls(
    access: &ConfigAccess,
    args: &BackgroundListArgs,
    age_cutoff: Option<DateTime<Utc>>,
) -> Result<Vec<BackgroundToolCallRow>> {
    let mut filters = vec![r#"{ await_mode: { _eq: "background" } }"#.to_string()];
    if let Some(request_id) = normalize_optional_string(args.request_id.as_deref()) {
        filters.push(format!(
            r#"{{ request_id: {{ _eq: "{}" }} }}"#,
            escape_graphql_string(&request_id)
        ));
    }
    if let Some(cutoff) = age_cutoff {
        let cutoff = cutoff.to_rfc3339_opts(SecondsFormat::Secs, true);
        filters.push(format!(
            r#"{{ started_at: {{ _lt: "{}" }} }}"#,
            escape_graphql_string(&cutoff)
        ));
    }

    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ _and: [{}] }},
                order: {{ started_at: DESC }}
            ) {{
                request_id
                tool_call_id
                tool_name
                status
                lifecycle_state
                await_mode
                started_at
            }}
        }}"#,
        filters.join(", ")
    );
    let mut rows = load_rows::<BackgroundToolCallRow>(access, "AgentToolCall", &query).await?;
    if let Some(state) = normalize_optional_string(args.state.as_deref()) {
        rows.retain(|row| row_state(row).as_deref() == Some(state.as_str()));
    }
    Ok(rows)
}

fn build_output_rows(
    now: DateTime<Utc>,
    tool_calls: Vec<BackgroundToolCallRow>,
    liveness: &RuntimeLivenessView,
) -> Vec<BackgroundToolCallOutput> {
    let active_tool_call_ids = liveness
        .active_tool_calls
        .iter()
        .map(|row| row.tool_call_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut native_by_tool_name: BTreeMap<&str, Vec<NativeExecutorView>> = BTreeMap::new();
    // Native executor liveness currently identifies the executable/tool, not
    // the specific AgentToolCall that spawned it. Treat these as tool-name
    // matches only.
    for executor in &liveness.active_native_executors {
        if let Some(tool_name) = executor.tool_name.as_deref() {
            native_by_tool_name
                .entry(tool_name)
                .or_default()
                .push(executor.clone());
        }
    }

    tool_calls
        .into_iter()
        .map(|row| {
            let started_at = normalize_optional_string(row.started_at.as_deref());
            let age_ms = started_at
                .as_deref()
                .and_then(parse_rfc3339_utc)
                .map(|started| now.signed_duration_since(started).num_milliseconds().max(0));
            let tool_name = normalize_optional_string(row.tool_name.as_deref());
            let native_executors = tool_name
                .as_deref()
                .and_then(|name| native_by_tool_name.get(name))
                .cloned()
                .unwrap_or_default();
            let state = row_state(&row).unwrap_or_else(|| "-".to_string());
            let tool_call_id = row.tool_call_id.unwrap_or_default();

            BackgroundToolCallOutput {
                tool_call_id: tool_call_id.clone(),
                parent_request_id: row.request_id.unwrap_or_default(),
                state,
                await_mode: normalize_optional_string(row.await_mode.as_deref())
                    .unwrap_or_else(|| "-".to_string()),
                started_at,
                age_ms,
                age: age_ms.map(format_age_ms),
                tool_name,
                active_tool_call: !tool_call_id.is_empty()
                    && active_tool_call_ids.contains(tool_call_id.as_str()),
                native_executor_tool_name_match_count: native_executors.len(),
                native_executor_tool_name_matches: native_executors,
            }
        })
        .collect()
}

fn row_state(row: &BackgroundToolCallRow) -> Option<String> {
    normalize_optional_string(row.lifecycle_state.as_deref())
        .or_else(|| normalize_optional_string(row.status.as_deref()))
}

fn age_cutoff(raw: Option<&str>, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
    let Some(raw) = normalize_optional_string(raw) else {
        return Ok(None);
    };
    let duration = parse_duration_suffix(&raw)?;
    let secs = i64::try_from(duration.as_secs()).context("duration too large")?;
    Ok(Some(now - chrono::Duration::seconds(secs)))
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn format_age_ms(age_ms: i64) -> String {
    if age_ms < 1_000 {
        return format!("{age_ms}ms");
    }
    let total_secs = age_ms / 1_000;
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3_600;
    let minutes = (total_secs % 3_600) / 60;
    let seconds = total_secs % 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn print_background_table(rows: &[BackgroundToolCallOutput], native_liveness_available: bool) {
    let headers = [
        "TOOL_CALL_ID",
        "PARENT_REQUEST",
        "STATE",
        "AWAIT_MODE",
        "STARTED_AT",
        "AGE",
        "NATIVE_TOOL",
    ];
    let rendered_rows = rows
        .iter()
        .map(|row| {
            [
                display_cell(&row.tool_call_id),
                display_cell(&row.parent_request_id),
                display_cell(&row.state),
                display_cell(&row.await_mode),
                row.started_at.clone().unwrap_or_else(|| "-".to_string()),
                row.age.clone().unwrap_or_else(|| "-".to_string()),
                if !native_liveness_available {
                    "unknown".to_string()
                } else if row.native_executor_tool_name_match_count > 0 {
                    "yes".to_string()
                } else {
                    "no".to_string()
                },
            ]
        })
        .collect::<Vec<_>>();
    let mut widths = headers.map(str::len);
    for row in &rendered_rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(cell.len());
        }
    }

    print_table_row(&headers.map(|header| header.to_string()), &widths);
    print_table_row(&widths.map(|width| "-".repeat(width)), &widths);
    for row in &rendered_rows {
        print_table_row(row, &widths);
    }
}

fn display_cell(value: &str) -> String {
    if value.trim().is_empty() {
        "-".to_string()
    } else {
        value.to_string()
    }
}

fn print_table_row<const N: usize>(cells: &[String; N], widths: &[usize; N]) {
    let line = cells
        .iter()
        .enumerate()
        .map(|(idx, cell)| format!("{cell:<width$}", width = widths[idx]))
        .collect::<Vec<_>>()
        .join("  ");
    println!("{line}");
}

async fn load_rows<T>(access: &ConfigAccess, collection: &str, query: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    graphql_rows_or_empty_if_collection_missing(access, collection, query)
        .await?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("decoding {collection} rows"))
}

#[derive(Debug, Deserialize)]
struct BackgroundToolCallRow {
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    await_mode: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct BackgroundListOutput {
    generated_at: String,
    filters: BackgroundListFilters,
    active_native_executors_available: bool,
    count: usize,
    items: Vec<BackgroundToolCallOutput>,
}

#[derive(Debug, Serialize)]
struct BackgroundListFilters {
    request_id: Option<String>,
    state: Option<String>,
    age_gt: Option<String>,
}

#[derive(Debug, Serialize)]
struct BackgroundToolCallOutput {
    tool_call_id: String,
    parent_request_id: String,
    state: String,
    await_mode: String,
    started_at: Option<String>,
    age_ms: Option<i64>,
    age: Option<String>,
    tool_name: Option<String>,
    active_tool_call: bool,
    native_executor_tool_name_match_count: usize,
    native_executor_tool_name_matches: Vec<NativeExecutorView>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct NativeExecutorView {
    id: u64,
    pid: i32,
    argv0: String,
    tool_name: Option<String>,
    started_at: String,
    age_ms: i64,
}

#[derive(Debug, Deserialize)]
struct ActiveToolCallView {
    tool_call_id: String,
}

#[derive(Debug, Default)]
struct RuntimeLivenessView {
    active_native_executors_available: bool,
    active_tool_calls: Vec<ActiveToolCallView>,
    active_native_executors: Vec<NativeExecutorView>,
}

impl RuntimeLivenessView {
    fn from_value(value: &Value) -> Self {
        Self {
            active_native_executors_available: value
                .get("active_native_executors_available")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            active_tool_calls: parse_array(value, "active_tool_calls"),
            active_native_executors: parse_array(value, "active_native_executors"),
        }
    }
}

fn parse_array<T>(value: &Value, field: &str) -> Vec<T>
where
    T: DeserializeOwned,
{
    let Some(array) = value.get(field).and_then(Value::as_array) else {
        return Vec::new();
    };
    match serde_json::from_value(Value::Array(array.clone())) {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(
                field,
                error = %err,
                "failed to decode runtime liveness array"
            );
            Vec::new()
        }
    }
}
