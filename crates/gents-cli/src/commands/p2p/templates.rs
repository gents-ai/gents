//! `p2p templates` subcommands: list.
//!
//! The handler reads directly from the static catalog in
//! `gents::agent::p2p_reconcile::templates` — no node or GraphQL
//! connection required.

use std::io::{self, Write};

use anyhow::{Context, Result};
use gents::agent::p2p_reconcile::templates::{builtin_templates, Delivery, Scope};
use serde::Serialize;
use serde_json::json;

use crate::cli::args::P2pTemplatesListArgs;
use crate::cli::output_format::OutputFormat;
use crate::print_json;

// ---------------------------------------------------------------------------
// Row type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TemplateRow {
    id: String,
    collections: String,
    scope: String,
    delivery: String,
}

fn delivery_str(d: &Delivery) -> &'static str {
    match d {
        Delivery::Push => "push",
        Delivery::Replicate => "replicate",
    }
}

fn scope_str(s: &Scope) -> String {
    match s {
        Scope::PeerDid { field } => field.to_string(),
        Scope::Unscoped => "unscoped".to_string(),
        Scope::PerCollection(_) => "per-collection".to_string(),
    }
}

fn template_rows() -> Vec<TemplateRow> {
    builtin_templates()
        .iter()
        .map(|t| TemplateRow {
            id: t.id.to_string(),
            collections: t.collections.join(","),
            scope: scope_str(&t.scope),
            delivery: delivery_str(&t.delivery).to_string(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// p2p templates list
// ---------------------------------------------------------------------------

pub(super) async fn p2p_templates_list(args: P2pTemplatesListArgs) -> Result<()> {
    let rows = template_rows();

    match args.output.ensure_supported(
        "p2p templates list",
        &[OutputFormat::Json, OutputFormat::Table],
    )? {
        OutputFormat::Json => print_json(&json!({
            "templates": rows,
            "count": rows.len(),
        })),
        OutputFormat::Table => print_templates_table(&rows),
        _ => unreachable!("ensure_supported restricts p2p templates list output formats"),
    }
}

// ---------------------------------------------------------------------------
// Table rendering
// ---------------------------------------------------------------------------

fn print_templates_table(rows: &[TemplateRow]) -> Result<()> {
    let headers = [
        "ID".to_string(),
        "SCOPE".to_string(),
        "DELIVERY".to_string(),
        "COLLECTIONS".to_string(),
    ];
    let mut widths = headers.clone().map(|h| h.len());
    let table_rows: Vec<[String; 4]> = rows
        .iter()
        .map(|r| {
            [
                r.id.clone(),
                r.scope.clone(),
                r.delivery.clone(),
                r.collections.clone(),
            ]
        })
        .collect();
    for row in &table_rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(cell.len());
        }
    }
    let mut stdout = io::stdout();
    print_table_row(&mut stdout, &headers, &widths)?;
    print_table_row(&mut stdout, &widths.map(|w| "-".repeat(w)), &widths)?;
    for row in &table_rows {
        print_table_row(&mut stdout, row, &widths)?;
    }
    stdout.flush().context("flushing p2p templates table")?;
    Ok(())
}

fn print_table_row<const N: usize>(
    writer: &mut impl Write,
    cells: &[String; N],
    widths: &[usize; N],
) -> Result<()> {
    let line = cells
        .iter()
        .zip(widths.iter())
        .map(|(cell, width)| format!("{cell:<width$}"))
        .collect::<Vec<_>>()
        .join("  ");
    writeln!(writer, "{line}").context("writing p2p templates table row")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::Cli;
    use clap::Parser;

    // ---- parse tests ----

    #[test]
    fn p2p_templates_list_json_parses() {
        let cli = Cli::try_parse_from(["gents", "p2p", "templates", "list", "--output", "json"])
            .expect("p2p templates list --output json should parse");
        match cli.command {
            crate::cli::args::Command::P2p {
                command: crate::cli::args::P2pCommand::Templates { command },
            } => match command {
                crate::cli::args::P2pTemplatesCommand::List(args) => {
                    assert_eq!(args.output, OutputFormat::Json);
                }
            },
            _ => panic!("expected p2p templates"),
        }
    }

    #[test]
    fn p2p_templates_list_default_output_is_table() {
        let cli = Cli::try_parse_from(["gents", "p2p", "templates", "list"])
            .expect("p2p templates list should parse");
        match cli.command {
            crate::cli::args::Command::P2p {
                command: crate::cli::args::P2pCommand::Templates { command },
            } => match command {
                crate::cli::args::P2pTemplatesCommand::List(args) => {
                    assert_eq!(args.output, OutputFormat::Table);
                }
            },
            _ => panic!("expected p2p templates"),
        }
    }

    // ---- render tests ----

    #[test]
    fn template_rows_matches_builtin_catalog() {
        let rows = template_rows();
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "conversation",
                "machine",
                "agent-config",
                "backup",
                "discovery",
                "network-control",
                "subagent-coordinator",
                "subagent-host",
                "app-collections",
            ]
        );
    }

    #[test]
    fn conversation_row_is_push_agent_did() {
        let rows = template_rows();
        let row = rows.iter().find(|r| r.id == "conversation").unwrap();
        assert_eq!(row.delivery, "push");
        assert_eq!(row.scope, "agent_did");
        // 8 collections comma-separated
        assert_eq!(row.collections.split(',').count(), 8);
    }

    #[test]
    fn agent_config_row_is_replicate_unscoped() {
        let rows = template_rows();
        let row = rows.iter().find(|r| r.id == "agent-config").unwrap();
        assert_eq!(row.delivery, "replicate");
        assert_eq!(row.scope, "unscoped");
    }

    #[test]
    fn backup_row_is_replicate_unscoped() {
        let rows = template_rows();
        let row = rows.iter().find(|r| r.id == "backup").unwrap();
        assert_eq!(row.delivery, "replicate");
        assert_eq!(row.scope, "unscoped");
    }

    #[test]
    fn app_collections_row_is_replicate_unscoped_with_no_fixed_collections() {
        let rows = template_rows();
        let row = rows.iter().find(|r| r.id == "app-collections").unwrap();
        assert_eq!(row.delivery, "replicate");
        assert_eq!(row.scope, "unscoped");
        assert_eq!(row.collections, "");
    }
}
