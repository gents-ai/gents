use anyhow::{Context, Result};

/// The physical identity scopes live invalidations only; authority remains in
/// the native scoped read, and feed overflow still requires a durable reread.
pub struct ScopedInterruptObservation {
    pub request_doc_id: Option<String>,
    pub interrupted_at: Option<String>,
}

/// The scoped request and when the session's interrupt passed its claim.
pub async fn observe_request_interrupted_at_with_access(
    access: &gents_store::config_client::ConfigAccess,
    request_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<ScopedInterruptObservation> {
    let filter = super::scoped_request_filter(request_id, node_did, requester_did)?;
    let query = format!(
        r#"{{ AgentRequest(filter: {{{filter}}}, limit: 2) {{ _docID node_did session_id requester_did lifecycle_state interrupt_generation }} }}"#
    );
    access
        .transact_readonly("interrupt.observe_scoped_request", |txn| {
            let query = &query;
            Box::pin(async move {
                let response = txn.execute(query).await?;
                let rows = response["data"]["AgentRequest"]
                    .as_array()
                    .context("interrupt request fetch omitted rows")?;
                anyhow::ensure!(
                    rows.len() <= 1,
                    "interrupt request fetch is ambiguous within node scope"
                );
                let Some(row) = rows.first() else {
                    return Ok(ScopedInterruptObservation {
                        request_doc_id: None,
                        interrupted_at: None,
                    });
                };
                let request_doc_id = row["_docID"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
                    .context("scoped interrupt observation omitted physical request identity")?;
                let interrupted_at = super::observed_interrupted_at_in_txn(
                    txn,
                    row["node_did"].as_str().unwrap_or_default(),
                    row["session_id"].as_str().unwrap_or_default(),
                    row["requester_did"].as_str(),
                    gents_protocol::request_lifecycle::RequestLifecycleState::parse_opt(
                        row["lifecycle_state"].as_str(),
                    ),
                    row["interrupt_generation"].as_i64(),
                )
                .await?;
                Ok(ScopedInterruptObservation {
                    request_doc_id: Some(request_doc_id),
                    interrupted_at,
                })
            })
        })
        .await
        .with_context(|| format!("observe_request_interrupted_at({request_id})"))
}
