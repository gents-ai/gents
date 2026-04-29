use anyhow::{Context, Result};
use defra_agent::session::{fork, ForkError, ForkParams};
use serde_json::json;

use crate::cli::args::{SessionCommand, SessionForkArgs};
use crate::{default_data_dir, print_json, resolve_agent_did, resolve_home_dir};

pub(crate) async fn dispatch(command: SessionCommand) -> Result<()> {
    match command {
        SessionCommand::Fork(args) => session_fork(args).await,
    }
}

async fn session_fork(args: SessionForkArgs) -> Result<()> {
    // Fork v1 runs in-process against the on-disk data directory; the embedded
    // node holds an exclusive lock on that path. Talking to a remote server via
    // GraphQL is a separate mode (see Open Issues in the design spec). Reject
    // --graphql up front so callers don't silently get in-process behavior.
    if args.graphql.is_some() {
        anyhow::bail!(
            "--graphql is not yet supported by `session fork`; fork currently runs \
             in-process against the local data directory. Stop `defra-agent server` \
             first, then rerun without --graphql."
        );
    }

    // CLI resolves the caller DID from local config unless overridden.
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())
        .context("resolving caller agent_did")?;

    // Fork v1 runs in-process against the on-disk data directory. DefraDB's
    // embedded node holds an exclusive lock on the data path, so this command
    // cannot run while `defra-agent server` is running against the same home.
    // GraphQL-mode fork (remote fork against a running server) is a follow-up
    // (see Open Issues in the design spec).
    let home = resolve_home_dir(args.home.as_deref());
    let data_dir = default_data_dir(&home);

    let node = crate::persistent_node_builder(&data_dir)
        .build()
        .await
        .with_context(|| {
            format!(
                "opening embedded node at {}. If `defra-agent server` is running against \
                 the same home, stop it first — fork holds an exclusive lock on the data \
                 directory. GraphQL-mode fork against a running server is not yet implemented.",
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
    .map_err(|e| match e {
        ForkError::ForkSourceNotFound(_)
        | ForkError::ForkAtUserTurnOutOfRange(_, _)
        | ForkError::ForkBehaviorNotFound(_)
        | ForkError::ForkBehaviorNotOwnedByPrincipal(_, _)
        | ForkError::ForkNotSameAgent
        | ForkError::ForkSourceBusy => anyhow::anyhow!("{e}"),
        ForkError::ForkCopyFailed(inner) => inner.context("fork copy step failed"),
    })?;

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
