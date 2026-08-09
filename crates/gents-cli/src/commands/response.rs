use anyhow::Result;

use crate::cli::args::{ResponseCommand, ResponseShowArgs, ResponseWaitArgs};
use crate::{
    authenticated_graphql_client, post_graphql, print_json, resolve_graphql_endpoint,
    resolve_home_dir, resolve_request_id, response_query, wait_for_terminal_response,
};

pub(crate) async fn dispatch(command: ResponseCommand) -> Result<()> {
    match command {
        ResponseCommand::Show(args) => response_show(args).await,
        ResponseCommand::Wait(args) => response_wait(args).await,
    }
}

pub(crate) async fn response_show(args: ResponseShowArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let graphql =
        authenticated_graphql_client(&resolve_home_dir(args.home.as_deref()), &graphql).await?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let query = response_query(&request_id);
    let response = post_graphql(&graphql, &query).await?;
    print_json(&response)?;
    Ok(())
}

async fn response_wait(args: ResponseWaitArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let graphql =
        authenticated_graphql_client(&resolve_home_dir(args.home.as_deref()), &graphql).await?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let response =
        wait_for_terminal_response(&graphql, &request_id, args.timeout_secs, args.poll_secs)
            .await?;
    print_json(&response)?;
    Ok(())
}
