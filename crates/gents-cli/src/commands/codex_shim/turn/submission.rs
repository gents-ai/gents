use anyhow::Result;

use super::super::ShimState;
use crate::request_helpers::{
    matching_prepared_receipt, prepare_agent_request, submit_prepared_agent_request_committed,
};
use crate::{RequestSubmitOptions, SubmittedRequest};

pub(super) async fn create_agent_request_with_retry(
    state: &ShimState,
    content: &str,
    session_id: Option<&str>,
    options: RequestSubmitOptions,
) -> Result<SubmittedRequest> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let prepared = prepare_agent_request(
        state.graphql.as_ref(),
        state.agent_did.as_ref(),
        content,
        session_id,
        Some(state.behavior_id.as_ref()),
        Some(request_id.clone()),
        options,
    )
    .await?;
    let result = submit_prepared_agent_request_committed(state.graphql.as_ref(), &prepared).await;
    if result.is_err() {
        // A lost reply can leave committed work. Only the original signed
        // receipt establishes which physical request this submission owns.
        if let Ok(Some(row)) =
            matching_prepared_receipt(state.graphql.as_ref(), &prepared.create).await
        {
            if let Some(physical) = row.doc_id.as_deref() {
                if let Err(error) = gents::interrupt_request_by_doc_id(
                    state.node.as_ref(),
                    physical,
                    &prepared.create.agent_did,
                    row.requester_did.as_deref(),
                )
                .await
                {
                    tracing::debug!(%error, request_id, "Codex shim could not interrupt committed request after submission failure");
                }
            }
        }
    }
    result
}
