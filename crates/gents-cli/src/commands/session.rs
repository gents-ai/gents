use anyhow::{Context, Result};
use gents_protocol::operations::session::{
    SessionEntry, SessionForkBoundary, SessionForkInput, SessionListInput, SessionRef,
};
use serde_json::json;

use crate::cli::args::{ConfigShowArgs, SessionCommand, SessionForkArgs, SessionListArgs};
use crate::cli::output_format::OutputFormat;
use crate::operation_failure::OperationFailure;
use crate::request_helpers::resolve_dual_id;
use crate::{print_json, resolve_config_access};
use gents_service::home_state::resolve_node_did;

pub(crate) async fn dispatch(command: SessionCommand) -> Result<()> {
    match command {
        SessionCommand::List(args) => session_list(args).await,
        SessionCommand::Show(args) => session_show(args).await,
        SessionCommand::Fork(args) => session_fork(args).await,
    }
}

async fn session_list(args: SessionListArgs) -> Result<()> {
    let output = args
        .list
        .output
        .ensure_supported("sessions list", &[OutputFormat::Table, OutputFormat::Json])?;
    let (access, _) =
        resolve_config_access(args.list.home.as_deref(), args.list.graphql.as_deref())
            .await
            .context("resolving access for session list")?;
    let input = SessionListInput {
        include_archived: args.include_archived,
        ..Default::default()
    };
    let entries = gents_session::operations::session::list(&access, &input)
        .await
        .map_err(OperationFailure::from)?;

    match output {
        OutputFormat::Json => print_json(&json!({
            "collection": "AgentSession",
            "count": entries.len(),
            "items": entries,
        })),
        OutputFormat::Table => {
            print_session_table(&entries);
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
        .ensure_supported("sessions show", &[OutputFormat::Json])?;
    let node_did = resolve_node_did(args.home.as_deref(), None)?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref())
        .await
        .context("resolving access for session show")?;
    let reference = SessionRef {
        node_did,
        session_id: id,
        requester_did: None,
    };
    let entry = gents_session::operations::session::get(&access, &reference)
        .await
        .map_err(OperationFailure::from)?;

    match output {
        OutputFormat::Json => print_json(&entry),
        _ => unreachable!("ensure_supported restricts session show output formats"),
    }
}

async fn session_fork(args: SessionForkArgs) -> Result<()> {
    let node_did = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())
        .context("resolving caller node_did")?;

    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref())
        .await
        .context("resolving authenticated access for session fork")?;

    let input = SessionForkInput {
        source: SessionRef {
            node_did,
            session_id: args.from.clone(),
            requester_did: args.requester_did.clone(),
        },
        boundary: SessionForkBoundary::UserTurn(args.at_user_turn),
        target_agent_did: args.agent.clone(),
    };
    let receipt = gents_session::operations::session::fork(&access, &input)
        .await
        .map_err(OperationFailure::from)?;
    let copied = receipt.copied;
    print_json(&json!({
        "session_id": receipt.session_id,
        "source_session_id": receipt.source_session_id,
        "fork_at_user_turn": args.at_user_turn,
        "copied_messages": copied.map(|copied| copied.messages),
        "copied_tool_calls": copied.map(|copied| copied.tool_calls),
        "copied_tool_results": copied.map(|copied| copied.tool_results),
        "copied_compaction_entries": copied.map(|copied| copied.compaction_entries),
    }))
}

fn print_session_table(entries: &[SessionEntry]) {
    let headers = ["SESSION_ID", "STATE", "REQUESTS", "CREATED_AT"];
    let rendered = entries
        .iter()
        .map(|entry| {
            [
                Some(entry.session.session_id.clone()),
                Some(entry.state.as_str().to_owned()),
                Some(entry.request_count.to_string()),
                Some(entry.session.created_at.clone()),
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
