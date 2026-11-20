use super::TerminalJobProgress;
use anyhow::{Context, Result};
use gents_store::config_client::ConfigApplyTxn;

/// Retained images share the accepted tool's DID and request binding and commit
/// only after its terminal compare wins. They are not part of provider output.
pub(super) async fn retain_file_changes(
    txn: &ConfigApplyTxn<'_>,
    tool_call_doc_id: &str,
    request_doc_id: &str,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    changes: &gents_session::file_contents::FileChanges,
) -> Result<()> {
    anyhow::ensure!(
        changes.belongs_to(tool_call_doc_id),
        "file changes belong to another physical tool"
    );
    let binding = {
        use gents_protocol::document_query::{
            fields::agent_tool_call as fields, session_scope, Filter, Query, Selection,
        };
        let rendered = Query::<fields::Doc>::select(Selection::doc_id())
            .doc_id(tool_call_doc_id)
            .filter(Filter::eq(fields::REQUEST_DOC_ID, request_doc_id))
            .filter(session_scope(node_did, session_id, requester_did))
            .render();
        txn.run(&rendered).await?
    };
    anyhow::ensure!(
        binding["data"]["AgentToolCall"].as_array().is_some_and(
            |rows| rows.len() == 1 && rows[0]["_docID"].as_str() == Some(tool_call_doc_id)
        ),
        "file changes lost their physical tool binding"
    );
    let session_scope = gents_protocol::session::SessionScope {
        node_did: node_did.to_owned(),
        session_id: session_id.to_owned(),
        requester_did: requester_did.map(str::to_owned),
    };
    let key = serde_json::json!({
        "tool_call_doc_id": tool_call_doc_id, "request_doc_id": request_doc_id,
        "node_did": node_did, "session_id": session_id,
        "requester_did": requester_did,
    });
    let existing = {
        use gents_protocol::document_query::{
            fields::tool_file_change as fields, Filter, Query, Selection,
        };
        let rendered = Query::<fields::Doc>::select(
            Selection::doc_id()
                .field(fields::TOOL_CALL_DOC_ID)
                .field(fields::REQUEST_DOC_ID)
                .field(fields::NODE_DID)
                .field(fields::SESSION_ID)
                .field(fields::REQUESTER_DID)
                .field(fields::FILE_CHANGES),
        )
        .filter(Filter::eq(fields::TOOL_CALL_DOC_ID, tool_call_doc_id))
        .limit(2)
        .render();
        txn.run(&rendered).await?
    };
    let rows = existing["data"]["ToolFileChange"]
        .as_array()
        .context("file change lookup omitted observations")?;
    let observation_doc_id = if let [row] = rows.as_slice() {
        let mut observed = row.clone();
        let fields = observed
            .as_object_mut()
            .context("file observation is not an object")?;
        let id = fields
            .remove("_docID")
            .context("file observation omitted physical identity")?;
        let stored = fields
            .remove("file_changes")
            .context("file observation omitted its changes")?;
        let stored: gents_protocol::file_changes::StoredFileChanges =
            serde_json::from_value(stored).context("file observation has unknown changes")?;
        anyhow::ensure!(
            observed == key
                && gents_session::file_contents::same_observation(&stored, changes.batch()),
            "file changes conflict with an existing observation"
        );
        id.as_str()
            .context("file observation identity is not a string")?
            .to_owned()
    } else {
        anyhow::ensure!(rows.is_empty(), "file changes have multiple observations");
        let stored =
            gents_session::file_contents::store(txn, &session_scope, changes.batch()).await?;
        let mut input = key;
        input["file_changes"] = serde_json::to_value(stored)?;
        let result = txn.execute_with_variables(
            "mutation($input: ToolFileChangeMutationInputArg!) { create_ToolFileChange(input: $input) { _docID } }",
            &serde_json::json!({"input": input}),
        ).await?;
        let rows = gents_store::graphql::mutation_documents(&result, "create_ToolFileChange")?
            .context("file change observation was not retained")?;
        anyhow::ensure!(
            rows.len() == 1,
            "file capture must retain exactly one observation"
        );
        rows[0]["_docID"]
            .as_str()
            .context("file capture omitted its observation identity")?
            .to_owned()
    };
    crate::call::lifecycle::file_inventory::retain_inventory(
        txn,
        crate::call::lifecycle::file_inventory::Metadata::Images {
            observation_doc_id: &observation_doc_id,
            changes,
        },
        tool_call_doc_id,
        request_doc_id,
        &session_scope,
    )
    .await?;
    Ok(())
}

/// Writes a terminal stamp that never fails the settle: a stamp over the
/// progress cap drops its event, and one that still does not fit is skipped.
pub(super) async fn write_terminal_progress(
    txn: &ConfigApplyTxn<'_>,
    doc_id: &str,
    progress: TerminalJobProgress,
) -> Result<()> {
    use crate::background::job::records::{write_progress_in_txn, RecordError};
    let too_large = |result: &Result<_>| {
        result.as_ref().is_err_and(|error| {
            matches!(
                RecordError::in_error(error),
                Some(RecordError::TooLarge { .. })
            )
        })
    };
    let TerminalJobProgress {
        expected_revision,
        guest,
        host,
    } = progress;
    let written =
        write_progress_in_txn(txn, doc_id, expected_revision, guest.clone(), host.clone()).await;
    if !too_large(&written) {
        return written.map(drop);
    }
    let cut = gents_protocol::job::JobProgress {
        last_event: String::new(),
        ..guest
    };
    let written = write_progress_in_txn(txn, doc_id, expected_revision, cut, host).await;
    if too_large(&written) {
        tracing::warn!(
            doc_id,
            "the terminal job progress stamp is over the cap; skipped"
        );
        return Ok(());
    }
    written.map(drop)
}
