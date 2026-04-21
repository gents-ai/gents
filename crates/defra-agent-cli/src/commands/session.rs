use anyhow::{Context, Result};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::session::{fork, ForkError, ForkParams};
use serde_json::json;

use crate::cli::args::{SessionCommand, SessionForkArgs};
use crate::{print_json, resolve_agent_did, resolve_graphql_endpoint, resolve_home_dir};

pub(crate) async fn dispatch(command: SessionCommand) -> Result<()> {
    match command {
        SessionCommand::Fork(args) => session_fork(args).await,
    }
}

async fn session_fork(args: SessionForkArgs) -> Result<()> {
    // CLI resolves the caller DID from local config unless overridden.
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())
        .context("resolving caller agent_did")?;

    // Fork needs a handle to the embedded node (not the GraphQL HTTP surface),
    // because it performs multiple correlated mutations and is currently
    // implemented against `EmbeddedNode`. For v1 we open the agent's local
    // data path and run the fork in-process. GraphQL remote mode is a
    // follow-up (see Open Issues in the spec).
    let home = resolve_home_dir(args.home.as_deref());
    let _graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;

    let node = EmbeddedNode::builder()
        .data_path(home.as_path())
        .build()
        .await
        .context("opening embedded node at home path")?;
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
