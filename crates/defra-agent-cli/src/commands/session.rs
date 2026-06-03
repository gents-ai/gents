use anyhow::{Context, Result};
use defra_agent::session::{fork, fork_via_http, ForkError, ForkOutcome, ForkParams};
use serde_json::json;

use crate::cli::args::{SessionCommand, SessionForkArgs};
use crate::{
    default_data_dir, graphql_diagnostic_hint, print_json, resolve_agent_did, resolve_home_dir,
};

pub(crate) async fn dispatch(command: SessionCommand) -> Result<()> {
    match command {
        SessionCommand::Fork(args) => session_fork(args).await,
    }
}

async fn session_fork(args: SessionForkArgs) -> Result<()> {
    // CLI resolves the caller DID from local config unless overridden.
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())
        .context("resolving caller agent_did")?;

    if let Some(graphql) = args.graphql.as_deref() {
        let outcome = fork_via_http(
            graphql,
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
    // cannot run while `defra-agent server` is running against the same home.
    let home = resolve_home_dir(args.home.as_deref());
    let data_dir = default_data_dir(&home);

    let node = crate::persistent_node_builder(&data_dir)
        .build()
        .await
        .with_context(|| {
            format!(
                "opening embedded node at {}. If `defra-agent server` is running against \
                 the same home, stop it first — fork holds an exclusive lock on the data \
                 directory. To fork against the running server, rerun with --graphql.",
                data_dir.display()
            )
        })?;
    defra_agent::ensure_runtime_schemas(&node)
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
