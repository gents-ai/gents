use anyhow::Result;

use crate::cli::args::{ResponseCommand, ResponseShowArgs, ResponseWaitArgs};
use crate::{
    hydrate_materialized_response_content, post_graphql, print_json, resolve_graphql_endpoint,
    resolve_request_id, response_field_is_blank, response_query, wait_for_terminal_response,
};

pub(crate) async fn dispatch(command: ResponseCommand) -> Result<()> {
    match command {
        ResponseCommand::Show(args) => response_show(args).await,
        ResponseCommand::Wait(args) => response_wait(args).await,
    }
}

pub(crate) async fn response_show(args: ResponseShowArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let query = response_query(&request_id);
    let mut response = post_graphql(&graphql, &query).await?;
    if let Some(response_row) = response.pointer_mut("/data/AgentResponse/0") {
        hydrate_materialized_response_content(&graphql, response_row).await?;
        let completed = response_row
            .get("status")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|status| matches!(status, "complete" | "completed"));
        if completed
            && response_field_is_blank(response_row, "content")
            && response_field_is_blank(response_row, "reasoning")
        {
            let sequence = response_row
                .get("materialized_message_sequence")
                .and_then(serde_json::Value::as_i64)
                .map(|sequence| sequence.to_string())
                .unwrap_or_else(|| "<missing>".to_string());
            let session_id = response_row
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("<missing>");
            anyhow::bail!(
                "could not hydrate materialized AgentMessage for request {request_id} \
                 (session_id={session_id}, sequence={sequence}); the referenced message is \
                 missing or invalid"
            );
        }
    }
    print_json(&response)?;
    Ok(())
}

async fn response_wait(args: ResponseWaitArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let response =
        wait_for_terminal_response(&graphql, &request_id, args.timeout_secs, args.poll_secs)
            .await?;
    print_json(&response)?;
    Ok(())
}
