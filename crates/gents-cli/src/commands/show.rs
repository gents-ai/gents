use anyhow::Result;

use crate::cli::args::{RuntimeShowArgs, ShowCommand};
use crate::{
    authenticated_graphql_client, print_json, resolve_agent_did, resolve_graphql_endpoint,
    resolve_home_dir,
};

pub(crate) async fn dispatch(command: ShowCommand) -> Result<()> {
    match command {
        ShowCommand::Request(args) => crate::commands::request::request_show(args).await,
        ShowCommand::Response(args) => crate::commands::response::response_show(args).await,
        ShowCommand::Runtime(args) => show_runtime(args).await,
    }
}

async fn show_runtime(args: RuntimeShowArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let graphql =
        authenticated_graphql_client(&resolve_home_dir(args.home.as_deref()), &graphql).await?;
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let output = crate::commands::status::load_runtime_status_output(
        args.home.as_deref(),
        &graphql,
        &agent_did,
    )
    .await?;
    print_json(&output)?;
    Ok(())
}
