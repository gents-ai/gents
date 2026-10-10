use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::session::{fork, fork_via_http, ForkError, ForkOutcome, ForkParams};
use serde_json::{json, Value};

use crate::cli::args::{ConfigListArgs, ConfigShowArgs, SessionCommand, SessionForkArgs};
use crate::cli::output_format::OutputFormat;
use crate::config_writes::ConfigAccess;
use crate::request_helpers::resolve_dual_id;
use crate::{
    default_data_dir, graphql_diagnostic_hint, graphql_rows, graphql_string_list_literal,
    print_json, resolve_config_access, resolve_home_dir, resolve_node_did,
};

const SESSION_FIELDS: &str = "session_id node_did requester_did agent_id created_at closed_at \
title tags provenance observation";

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
    let node_did = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())
        .context("resolving caller node_did")?;

    if let Some(graphql) = args.graphql.as_deref() {
        let endpoint = crate::resolve_graphql_endpoint(Some(graphql), args.home.as_deref())?;
        let outcome = fork_via_http(
            &endpoint,
            ForkParams {
                source_session_id: &args.from,
                fork_at_user_turn: args.at_user_turn,
                caller_node_did: &node_did,
                caller_requester_did: args.requester_did.as_deref(),
                target_agent_id: args.agent.as_deref(),
            },
        )
        .await
        .map_err(|error| map_graphql_fork_error(error, graphql))?;

        print_fork_outcome(&args, outcome)?;
        return Ok(());
    }

    // Fork v1 runs in-process against the on-disk data directory, under the
    // same store claim as every other opener, so a concurrent runtime or
    // overwrite is refused before the backend's own lock is reached.
    let home = resolve_home_dir(args.home.as_deref());
    let data_dir = default_data_dir(&home);

    let store = open_offline_fork_store(&home, &data_dir).await?;
    gents::ensure_runtime_schemas(&store.node)
        .await
        .context("ensuring runtime schemas")?;
    gents::store_key::upgrade::finish(&data_dir)?;

    let outcome = fork(
        &store.node,
        ForkParams {
            source_session_id: &args.from,
            fork_at_user_turn: args.at_user_turn,
            caller_node_did: &node_did,
            caller_requester_did: args.requester_did.as_deref(),
            target_agent_id: args.agent.as_deref(),
        },
    )
    .await
    .map_err(map_fork_error)?;

    print_fork_outcome(&args, outcome)?;
    Ok(())
}

/// Fork v1's offline store: the home's data directory opened as an embedded
/// node under the store claim `gents init` and `gents server` take, so an
/// overwrite cannot wipe a store mid-fork. Fields drop in declaration order:
/// the node closes before the claim is released.
struct OfflineForkStore {
    node: EmbeddedNode,
    _claim: gents::home::StoreLock,
}

/// Initialization is checked before claiming; opening or upgrading the store
/// requires the claim to remain held until the node closes.
async fn open_offline_fork_store(home: &Path, data_dir: &Path) -> Result<OfflineForkStore> {
    anyhow::ensure!(
        crate::read_init_config(home)?.is_some(),
        "gents home {} is not initialized; run `gents init --home {}` first",
        home.display(),
        home.display(),
    );
    fs::create_dir_all(data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;
    // Only the held-store refusal carries the escape: an unconditional context
    // would report an unrelated lock failure (a symlinked lock file, an
    // unopenable path) as a store in use. `.context` keeps the inner error
    // downcastable -- anyhow's chain downcast checks the context then the
    // wrapped error -- so the typed refusal survives the hint.
    let claim = gents::home::lock_store(home, data_dir).map_err(|error| {
        match error.downcast_ref::<gents::home::StoreLockHeld>() {
            Some(_) => error.context(
                "the home's store is in use; to fork against the running runtime, rerun with --graphql",
            ),
            None => error,
        }
    })?;
    let builder = crate::persistent_node_builder_with_stored_identity(home, data_dir).await?;
    let node = builder
        .build()
        .await
        .with_context(|| format!("opening embedded node at {}", data_dir.display()))?;
    Ok(OfflineForkStore {
        node,
        _claim: claim,
    })
}

async fn query_sessions(access: &ConfigAccess, session_id: Option<&str>) -> Result<Vec<Value>> {
    let args = session_id
        .map(|id| {
            format!(
                r#"(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1)"#,
                escape_graphql_string(id)
            )
        })
        .unwrap_or_default();
    let query = format!(
        r#"{{
            AgentSession{args} {{
                {SESSION_FIELDS}
            }}
        }}"#
    );
    graphql_rows(access, "AgentSession", &query).await
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
    let scope =
        gents::session::public_request_filter(&format!("session_id: {{ _in: {sessions} }}"));
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ {scope} }}) {{
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
    let headers = ["SESSION_ID", "STATE", "REQUESTS", "CREATED_AT"];
    let rendered = rows
        .iter()
        .map(|row| {
            [
                string_cell(row, "session_id"),
                Some(if row.get("closed_at").is_some_and(Value::is_string) {
                    "closed".to_string()
                } else {
                    "open".to_string()
                }),
                count_cell(row, "request_count"),
                string_cell(row, "created_at"),
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
        "copied_compaction_entries": outcome.copied_compaction_entries,
    }))?;
    Ok(())
}

fn map_fork_error(error: ForkError) -> anyhow::Error {
    match error {
        ForkError::ForkSourceNotFound(_)
        | ForkError::ForkAtUserTurnOutOfRange(_, _)
        | ForkError::ForkAgentNotFound(_)
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
    use super::*;
    use std::path::PathBuf;

    use crate::shared::StoredInitConfig;
    use crate::{default_key_path, write_init_config, ToolCeilingArg};
    use gents::NodeIdentity as _;

    /// An initialized home: the signing key and `init.json` every
    /// embedded-node entry point requires before it will open the store.
    fn initialized_home(temp: &Path) -> PathBuf {
        let home = temp.join("home");
        let key_path = default_key_path(&home, "default");
        fs::create_dir_all(key_path.parent().unwrap()).unwrap();
        let identity = gents::KeyIdentity::load_or_create(&key_path, None).unwrap();
        write_init_config(
            &home,
            &StoredInitConfig {
                home: home.to_string_lossy().to_string(),
                node_name: "default".to_string(),
                node_did: identity.did().to_string(),
                key_path: Some(key_path.to_string_lossy().to_string()),
                identity_backend: None,
                keychain_label: None,
                secure_enclave_label: None,
                tool_package: None,
                tool_ceiling: ToolCeilingArg::Readonly,
                tool_root: None,
                store_encryption: Some(
                    gents::store_key::StoreEncryption::prepare(
                        gents::store_key::StoreKeyCustodyChoice::File,
                        &gents::store_key::home_key_file(&home),
                        &default_data_dir(&home),
                    )
                    .unwrap(),
                ),
            },
        )
        .unwrap();
        home
    }

    #[tokio::test]
    async fn fork_v1_claims_the_store_it_opens_until_the_node_is_released() {
        let temp = tempfile::tempdir().unwrap();
        let home = initialized_home(temp.path());
        let data_dir = default_data_dir(&home);

        let store = open_offline_fork_store(&home, &data_dir)
            .await
            .expect("an initialized home opens its store");

        let error = gents::home::lock_store(&home, &data_dir)
            .expect_err("a store fork v1 has open excludes another runtime");
        assert!(
            error.downcast_ref::<gents::home::StoreLockHeld>().is_some(),
            "{error:#}"
        );
        assert!(
            fs::canonicalize(&home)
                .expect("the home exists")
                .join("data.lock")
                .is_file(),
            "the claim sits at the canonical store lock every other opener takes"
        );

        drop(store);
        // The backend refuses a store still open in this process, so reopening
        // proves the node closed as well as the claim released.
        open_offline_fork_store(&home, &data_dir)
            .await
            .expect("a dropped fork store releases the store");
    }

    #[tokio::test]
    async fn an_overwrite_cannot_wipe_a_store_fork_v1_holds_open() {
        let temp = tempfile::tempdir().unwrap();
        let home = initialized_home(temp.path());
        let data_dir = default_data_dir(&home);
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(data_dir.join("fixture"), "store").unwrap();
        let user_home = temp.path().join("user");
        fs::create_dir_all(&user_home).unwrap();

        let store = open_offline_fork_store(&home, &data_dir)
            .await
            .expect("an initialized home opens its store");
        let error = crate::commands::init::lock_init_store_for_user(
            &home,
            &data_dir,
            true,
            Some(&user_home),
        )
        .expect_err("a store fork v1 has open is not wiped");
        assert!(
            error.downcast_ref::<gents::home::StoreLockHeld>().is_some(),
            "{error:#}"
        );
        assert!(data_dir.join("fixture").is_file());
        assert!(home.join("init.json").is_file());
        drop(store);

        let held = crate::commands::init::lock_init_store_for_user(
            &home,
            &data_dir,
            true,
            Some(&user_home),
        )
        .expect("an idle home is overwritten");
        assert!(!data_dir.join("fixture").exists());
        drop(held);
    }

    #[tokio::test]
    async fn fork_v1_names_the_holder_of_a_claimed_store_and_the_graphql_escape() {
        let temp = tempfile::tempdir().unwrap();
        let home = initialized_home(temp.path());
        let _held = gents::home::lock_home_store(&home).unwrap();

        let error = match open_offline_fork_store(&home, &default_data_dir(&home)).await {
            Ok(_) => panic!("a claimed store must not be opened a second time"),
            Err(error) => error,
        };

        assert!(
            error.downcast_ref::<gents::home::StoreLockHeld>().is_some(),
            "{error:#}"
        );
        let text = format!("{error:#}");
        assert!(text.contains("already using"), "{text}");
        assert!(text.contains("--graphql"), "{text}");
    }

    #[tokio::test]
    async fn an_uninitialized_home_is_refused_before_fork_claims_a_store() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();

        let error = match open_offline_fork_store(&home, &default_data_dir(&home)).await {
            Ok(_) => panic!("an uninitialized home does not open a store"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("run `gents init --home"),
            "{error:#}"
        );
        assert!(
            !home.join("data").exists(),
            "nothing is created before the home is validated"
        );
        assert!(
            gents::home::lock_home_store(&home).is_ok(),
            "the refusal took no claim"
        );
    }
}
