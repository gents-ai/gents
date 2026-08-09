use std::collections::BTreeMap;

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents::session::{fork, fork_via_http, ForkError, ForkOutcome, ForkParams};
use serde_json::{json, Value};

use crate::cli::args::{ConfigListArgs, ConfigShowArgs, SessionCommand, SessionForkArgs};
use crate::cli::output_format::OutputFormat;
use crate::config_writes::ConfigAccess;
use crate::request_helpers::resolve_dual_id;
use crate::{
    default_data_dir, graphql_diagnostic_hint, graphql_rows, graphql_string_list_literal,
    load_config_identity, print_json, resolve_agent_did, resolve_config_access, resolve_home_dir,
};

const SESSION_FIELDS: &str = "session_id agent_name behavior_id started ended status";

pub(crate) async fn dispatch(command: SessionCommand) -> Result<()> {
    match command {
        SessionCommand::List(args) => session_list(args).await,
        SessionCommand::Show(args) => session_show(args).await,
        SessionCommand::Fork(args) => session_fork(args).await,
    }
}

async fn session_list(args: ConfigListArgs) -> Result<()> {
    let output = args
        .output
        .ensure_supported("session list", &[OutputFormat::Table, OutputFormat::Json])?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref())
        .await
        .context("resolving access for session list")?;
    let mut rows = query_sessions(&access, None).await?;
    sort_sessions(&mut rows);
    add_request_counts(&access, &mut rows).await?;

    match output {
        OutputFormat::Json => print_json(&json!({
            "collection": "AgentSession",
            "count": rows.len(),
            "items": rows,
        })),
        OutputFormat::Table => {
            print_session_table(&rows);
            Ok(())
        }
        _ => unreachable!("ensure_supported restricts session list output formats"),
    }
}

async fn session_show(args: ConfigShowArgs) -> Result<()> {
    let id = resolve_dual_id(
        "session",
        "--id",
        args.id.as_deref(),
        args.id_flag.as_deref(),
    )?;
    let output = args
        .output
        .ensure_supported("session show", &[OutputFormat::Json])?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref())
        .await
        .context("resolving access for session show")?;
    let mut rows = query_sessions(&access, Some(&id)).await?;
    let mut row = rows.pop().ok_or_else(|| {
        anyhow::anyhow!("not found: no AgentSession document with session_id {id}")
    })?;
    add_request_count(&access, &mut row).await?;

    match output {
        OutputFormat::Json => print_json(&row),
        _ => unreachable!("ensure_supported restricts session show output formats"),
    }
}

async fn session_fork(args: SessionForkArgs) -> Result<()> {
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())
        .context("resolving caller agent_did")?;

    if let Some(graphql) = args.graphql.as_deref() {
        let home = resolve_home_dir(args.home.as_deref());
        let access = crate::authenticated_graphql_access(&home, graphql).await?;
        let ConfigAccess::Graphql(access) = access else {
            unreachable!("authenticated GraphQL constructor returned local access")
        };
        let outcome = fork_via_http(
            access,
            ForkParams {
                source_session_id: &args.from,
                fork_at_user_turn: args.at_user_turn,
                caller_agent_did: &agent_did,
                target_behavior_id: args.behavior.as_deref(),
            },
        )
        .await
        .map_err(|error| map_graphql_fork_error(error, graphql))?;

        print_fork_outcome(&args, outcome)?;
        return Ok(());
    }

    // Fork v1 runs in-process against the on-disk data directory. DefraDB's
    // embedded node holds an exclusive lock on the data path, so this command
    // cannot run while `gents server` is running against the same home.
    let home = resolve_home_dir(args.home.as_deref());
    let data_dir = default_data_dir(&home);
    let node = open_offline_fork_node(&home, &data_dir).await?;
    gents::ensure_runtime_schemas(&node)
        .await
        .context("ensuring runtime schemas")?;

    let outcome = fork(
        &node,
        ForkParams {
            source_session_id: &args.from,
            fork_at_user_turn: args.at_user_turn,
            caller_agent_did: &agent_did,
            target_behavior_id: args.behavior.as_deref(),
        },
    )
    .await
    .map_err(map_fork_error)?;

    print_fork_outcome(&args, outcome)?;
    Ok(())
}

async fn open_offline_fork_node(
    home: &std::path::Path,
    data_dir: &std::path::Path,
) -> Result<gents::defra_node::EmbeddedNode> {
    // Loading the stored identity also registers its signing material with
    // DefraDB. Binding that DID to the node ensures fork reads and writes are
    // authenticated and their commits carry the same principal as the home.
    let identity =
        load_config_identity(home).context("loading signing identity for offline session fork")?;
    crate::persistent_node_builder(data_dir)
        .with_node_identity_did(identity.did())
        .build()
        .await
        .with_context(|| {
            format!(
                "opening embedded node at {}. If `gents server` is running against \
                 the same home, stop it first — fork holds an exclusive lock on the data \
                 directory. To fork against the running server, rerun with --graphql.",
                data_dir.display()
            )
        })
}

async fn query_sessions(access: &ConfigAccess, session_id: Option<&str>) -> Result<Vec<Value>> {
    let args = session_id
        .map(|id| {
            format!(
                r#"(filter: {{ session_id: {{ _eq: "{}" }} }})"#,
                escape_graphql_string(id)
            )
        })
        .unwrap_or_default();
    let fields = if session_id.is_some() {
        format!("_docID agent_did requester_did {SESSION_FIELDS}")
    } else {
        SESSION_FIELDS.to_string()
    };
    let query = format!(
        r#"{{
            AgentSession{args} {{
                {fields}
            }}
        }}"#
    );
    let rows = graphql_rows(access, "AgentSession", &query).await?;
    let Some(session_id) = session_id else {
        // Listing is presentation-only and deliberately exposes the complete
        // result set; it does not select one row as authority.
        return Ok(rows);
    };
    for row in &rows {
        if row.get("session_id").and_then(Value::as_str) != Some(session_id) {
            anyhow::bail!(
                "AgentSession logical key mismatch while showing session_id={session_id}: {row}"
            );
        }
    }
    let selected = gents::session::resolve_exact_logical_match(
        "AgentSession",
        "session_id",
        session_id,
        rows,
        |row| {
            row.get("_docID")
                .and_then(Value::as_str)
                .unwrap_or_default()
        },
    )?;
    Ok(selected
        .into_iter()
        .map(|mut row| {
            if let Some(object) = row.as_object_mut() {
                object.remove("_docID");
                object.remove("agent_did");
                object.remove("requester_did");
            }
            row
        })
        .collect())
}

async fn add_request_count(access: &ConfigAccess, row: &mut Value) -> Result<()> {
    let Some(session_id) = row.get("session_id").and_then(Value::as_str) else {
        return Ok(());
    };
    let counts = request_counts_by_session(access, &[session_id.to_string()]).await?;
    set_request_count(row, counts.get(session_id).copied().unwrap_or(0));
    Ok(())
}

async fn add_request_counts(access: &ConfigAccess, rows: &mut [Value]) -> Result<()> {
    let session_ids = rows
        .iter()
        .filter_map(|row| row.get("session_id").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let counts = request_counts_by_session(access, &session_ids).await?;
    for row in rows {
        let count = row
            .get("session_id")
            .and_then(Value::as_str)
            .and_then(|id| counts.get(id).copied())
            .unwrap_or(0);
        set_request_count(row, count);
    }
    Ok(())
}

async fn request_counts_by_session(
    access: &ConfigAccess,
    session_ids: &[String],
) -> Result<BTreeMap<String, u64>> {
    if session_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let sessions = graphql_string_list_literal(session_ids);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ session_id: {{ _in: {sessions} }} }}) {{
                session_id
            }}
        }}"#
    );
    let rows = graphql_rows(access, "AgentRequest", &query).await?;
    let mut counts = BTreeMap::new();
    for row in rows {
        if let Some(session_id) = row.get("session_id").and_then(Value::as_str) {
            *counts.entry(session_id.to_string()).or_insert(0) += 1;
        }
    }
    Ok(counts)
}

fn set_request_count(row: &mut Value, count: u64) {
    if let Some(object) = row.as_object_mut() {
        object.insert("request_count".to_string(), json!(count));
    }
}

fn sort_sessions(rows: &mut [Value]) {
    rows.sort_by(|a, b| {
        a.get("session_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(
                b.get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
    });
}

fn print_session_table(rows: &[Value]) {
    let headers = ["SESSION_ID", "STATUS", "REQUESTS", "STARTED"];
    let rendered = rows
        .iter()
        .map(|row| {
            [
                string_cell(row, "session_id"),
                string_cell(row, "status"),
                count_cell(row, "request_count"),
                string_cell(row, "started"),
            ]
        })
        .collect::<Vec<_>>();
    let widths = column_widths(&headers, &rendered);
    print_table_row(&headers, &widths);
    let separators = widths.map(|width| "-".repeat(width));
    let separator_cells = [
        separators[0].as_str(),
        separators[1].as_str(),
        separators[2].as_str(),
        separators[3].as_str(),
    ];
    print_table_row(&separator_cells, &widths);
    for row in rendered {
        let cells = [
            row[0].as_deref().unwrap_or(""),
            row[1].as_deref().unwrap_or(""),
            row[2].as_deref().unwrap_or(""),
            row[3].as_deref().unwrap_or(""),
        ];
        print_table_row(&cells, &widths);
    }
}

fn string_cell(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn count_cell(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_u64)
        .map(|count| count.to_string())
}

fn column_widths<const N: usize>(headers: &[&str; N], rows: &[[Option<String>; N]]) -> [usize; N] {
    std::array::from_fn(|index| {
        rows.iter()
            .filter_map(|row| row[index].as_ref().map(String::len))
            .chain(std::iter::once(headers[index].len()))
            .max()
            .unwrap_or(0)
    })
}

fn print_table_row<const N: usize>(cells: &[&str; N], widths: &[usize; N]) {
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            print!("  ");
        }
        print!("{cell:<width$}", width = widths[index]);
    }
    println!();
}

fn print_fork_outcome(args: &SessionForkArgs, outcome: ForkOutcome) -> Result<()> {
    print_json(&json!({
        "session_id": outcome.session_id,
        "source_session_id": args.from,
        "fork_at_user_turn": args.at_user_turn,
        "copied_messages": outcome.copied_messages,
        "copied_tool_calls": outcome.copied_tool_calls,
        "copied_tool_results": outcome.copied_tool_results,
        "copied_tool_approvals": outcome.copied_tool_approvals,
        "copied_compaction_entries": outcome.copied_compaction_entries,
    }))?;
    Ok(())
}

fn map_fork_error(error: ForkError) -> anyhow::Error {
    match error {
        ForkError::ForkSourceNotFound(_)
        | ForkError::ForkAtUserTurnOutOfRange(_, _)
        | ForkError::ForkBehaviorNotFound(_)
        | ForkError::ForkBehaviorNotOwnedByPrincipal(_, _)
        | ForkError::ForkNotSameAgent
        | ForkError::ForkSourceBusy => anyhow::anyhow!("{error}"),
        ForkError::ForkCopyFailed(inner) => inner.context("fork copy step failed"),
    }
}

fn map_graphql_fork_error(error: ForkError, graphql: &str) -> anyhow::Error {
    match error {
        ForkError::ForkCopyFailed(inner) => anyhow::anyhow!(
            "{}\n{}",
            inner.context("fork copy step failed"),
            graphql_diagnostic_hint(graphql)
        ),
        other => map_fork_error(other),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gents::{AgentIdentity, KeyIdentity};

    use super::*;
    use crate::cli::args::ToolCeilingArg;
    use crate::{default_key_path, write_init_config, StoredInitConfig};

    #[tokio::test]
    async fn offline_fork_node_uses_initialized_home_signer() {
        let home = tempfile::tempdir().unwrap();
        let key_path = default_key_path(home.path(), "fork-agent");
        std::fs::create_dir_all(key_path.parent().unwrap()).unwrap();
        let identity = KeyIdentity::load_or_create(&key_path, None).unwrap();
        write_init_config(
            home.path(),
            &StoredInitConfig {
                home: home.path().to_string_lossy().to_string(),
                agent_name: "fork-agent".to_string(),
                agent_did: identity.did().to_string(),
                key_path: Some(key_path.to_string_lossy().to_string()),
                identity_backend: Some("file".to_string()),
                keychain_label: None,
                secure_enclave_label: None,
                tool_package: None,
                tool_ceiling: ToolCeilingArg::Readonly,
                tool_root: None,
            },
        )
        .unwrap();

        let data_dir = default_data_dir(home.path());
        let node = open_offline_fork_node(home.path(), &data_dir)
            .await
            .unwrap();

        assert_eq!(node.node_identity_did(), Some(identity.did()));
    }

    #[tokio::test]
    async fn offline_fork_node_refuses_home_without_signer() {
        let home = tempfile::tempdir().unwrap();
        let data_dir = default_data_dir(home.path());

        let error = match open_offline_fork_node(home.path(), &data_dir).await {
            Ok(_) => panic!("offline fork node must require an initialized signer"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("loading signing identity for offline session fork"),
            "{error:#}"
        );
        assert!(!data_dir.exists());
    }

    #[tokio::test]
    async fn session_show_query_rejects_complete_logical_twin_set() {
        let node = gents::defra_node::EmbeddedNode::builder()
            .build()
            .await
            .unwrap();
        node.add_schema(
            r#"
            type AgentSession {
                session_id: String
                agent_name: String
                agent_did: String
                requester_did: String
                behavior_id: String
                started: DateTime
                ended: DateTime
                status: String
            }
            "#,
        )
        .await
        .unwrap();
        for owner in ["did:key:z-owner-a", "did:key:z-owner-b"] {
            let response = node
                .execute(&format!(
                    r#"mutation {{
                        create_AgentSession(input: {{
                            session_id: "duplicate-session"
                            agent_name: "agent"
                            agent_did: "{owner}"
                            behavior_id: "behavior"
                            started: "2026-08-08T00:00:00Z"
                            status: "active"
                        }}) {{ _docID }}
                    }}"#
                ))
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }
        let access = ConfigAccess::Local(Arc::new(node));
        let error = query_sessions(&access, Some("duplicate-session"))
            .await
            .expect_err("session show must not select a logical twin");
        assert!(
            error
                .downcast_ref::<gents::session::LogicalDocumentResolutionError>()
                .is_some_and(|error| matches!(
                    error,
                    gents::session::LogicalDocumentResolutionError::Conflict(_)
                )),
            "expected typed AgentSession conflict, got {error:#}"
        );
    }
}
