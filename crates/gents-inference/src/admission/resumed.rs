use gents_protocol::document_query::{fields::inference_call, Filter, Query, Selection};
use gents_protocol::row::AgentRequest;

/// The call sequence a resumed execution continues from
/// (`RequestBackendWake.resumedCallSeq`): its next call is numbered after
/// every call the request recorded before it parked.
fn resumed_call_seq(persisted: impl IntoIterator<Item = u64>) -> u64 {
    persisted.into_iter().max().unwrap_or(0)
}

/// The largest call sequence the request's `InferenceCall` rows record.
pub async fn last_recorded_call_seq(
    access: &gents_store::config_client::ConfigAccess,
    request: &AgentRequest,
) -> anyhow::Result<u64> {
    use anyhow::Context;
    let query = Query::select(Selection::new().field(inference_call::CALL_SEQ))
        .filter(Filter::eq(inference_call::REQUEST_DOC_ID, &request.doc_id))
        .filter(Filter::not_null(inference_call::CALL_SEQ))
        .render();
    let response = access.read(&query).await?;
    let rows = response
        .pointer("/data/InferenceCall")
        .and_then(serde_json::Value::as_array)
        .context("resumed call sequence query omitted rows")?;
    let persisted = rows
        .iter()
        .map(|row| {
            row["call_seq"]
                .as_i64()
                .and_then(|seq| u64::try_from(seq).ok())
                .context("recorded call omitted its sequence")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(resumed_call_seq(persisted))
}

#[cfg(test)]
mod tests {
    #[test]
    fn resumed_call_sequence_matches_the_generated_model() {
        gents_lean_vocab::families::backend_wake_resumed_call_seq::replay(|i| {
            super::resumed_call_seq(i.persisted.iter().copied())
        });
    }
}
