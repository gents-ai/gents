use anyhow::{Context, Result};
use gents::ConfigAccess;

use crate::cli::args::{
    RequestEditPendingArgs, RequestPendingArgs, RequestPendingMutationArgs,
    RequestRemovePendingArgs, RequestReorderPendingArgs,
};
use crate::cli::output_format::OutputFormat;
use crate::{print_json, resolve_graphql_endpoint, resolve_node_did, resolve_request_content};

async fn pending_scope(
    args: &RequestPendingArgs,
) -> Result<(
    ConfigAccess,
    String,
    std::sync::Arc<dyn gents::NodeIdentity>,
)> {
    anyhow::ensure!(
        !args.session_id.trim().is_empty(),
        "--session-id must not be empty"
    );
    let endpoint = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let node = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())?;
    let home = crate::resolve_home_dir(args.home.as_deref());
    let config = crate::read_init_config(&home)?
        .context("An initialized --home is required to identify the message author")?;
    let signer = crate::load_initialized_home_identity(&home, &config)?;
    Ok((ConfigAccess::Graphql(endpoint), node, signer))
}

pub(super) async fn request_pending(args: RequestPendingArgs) -> Result<()> {
    let (access, node, signer) = pending_scope(&args).await?;
    let snapshot =
        gents::lifecycle::pending_user_queue(&access, &node, &args.session_id, signer.did())
            .await?;
    match args
        .output
        .ensure_supported("request pending", &[OutputFormat::Text, OutputFormat::Json])?
    {
        OutputFormat::Json => print_json(&serde_json::to_value(&snapshot)?)?,
        OutputFormat::Text => {
            let mut output = std::io::stdout().lock();
            use std::io::Write;
            if snapshot.entries.is_empty() {
                writeln!(output, "No pending user messages.")?;
            }
            for entry in snapshot.entries {
                writeln!(
                    output,
                    "{}  {}  {}",
                    entry.request_doc_id,
                    if entry.editable {
                        "editable"
                    } else {
                        "read-only"
                    },
                    entry.content
                )?;
            }
        }
        _ => unreachable!("validated output format"),
    }
    Ok(())
}

fn selected_queue_ids(expected: &[String], requested: &[String]) -> Vec<String> {
    let mut selected = requested.to_vec();
    selected.sort_by_key(|id| expected.iter().position(|entry| entry == id));
    selected
}

async fn apply_pending_edit(
    args: RequestPendingMutationArgs,
    selected: Vec<String>,
    messages: Vec<gents::lifecycle::PendingMessageEdit>,
) -> Result<()> {
    let output = args.scope.output.ensure_supported(
        "pending message edit",
        &[OutputFormat::Text, OutputFormat::Json],
    )?;
    let (access, node, signer) = pending_scope(&args.scope).await?;
    let receipt = gents::lifecycle::submit_pending_user_edit(
        &access,
        signer.as_ref(),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(signer.did()),
        None,
        &node,
        &args.scope.session_id,
        gents::lifecycle::PendingQueueEdit {
            expected_request_doc_ids: args.expected_request_doc_ids,
            selected_request_doc_ids: selected,
            messages,
        },
    )
    .await?;
    match output {
        OutputFormat::Json => print_json(&serde_json::to_value(&receipt)?)?,
        OutputFormat::Text => {
            use std::io::Write;
            let mut output = std::io::stdout().lock();
            if receipt.request_doc_ids.is_empty() {
                writeln!(output, "Pending messages removed.")?;
            }
            for (request, physical) in receipt.request_ids.iter().zip(&receipt.request_doc_ids) {
                writeln!(output, "{physical}  {request}")?;
            }
        }
        _ => unreachable!("validated output format"),
    }
    Ok(())
}

pub(super) async fn request_edit_pending(args: RequestEditPendingArgs) -> Result<()> {
    let content = resolve_request_content(args.content.as_deref(), args.content_file.as_deref())?;
    let selected = vec![args.request_doc_id.clone()];
    let message = gents::lifecycle::PendingMessageEdit {
        authored_input: None,
        request_doc_id: args.request_doc_id,
        content,
    };
    apply_pending_edit(args.queue, selected, vec![message]).await
}

pub(super) async fn request_remove_pending(args: RequestRemovePendingArgs) -> Result<()> {
    let selected = selected_queue_ids(&args.queue.expected_request_doc_ids, &args.request_doc_ids);
    apply_pending_edit(args.queue, selected, Vec::new()).await
}

pub(super) async fn request_reorder_pending(args: RequestReorderPendingArgs) -> Result<()> {
    let selected = selected_queue_ids(&args.queue.expected_request_doc_ids, &args.request_doc_ids);
    let (access, node, signer) = pending_scope(&args.queue.scope).await?;
    let snapshot = gents::lifecycle::pending_user_queue(
        &access,
        &node,
        &args.queue.scope.session_id,
        signer.did(),
    )
    .await?;
    let messages = args
        .request_doc_ids
        .into_iter()
        .map(|id| {
            let entry = snapshot
                .entries
                .iter()
                .find(|entry| entry.request_doc_id == id)
                .context("Selected message is no longer pending")?;
            Ok(gents::lifecycle::PendingMessageEdit {
                authored_input: None,
                request_doc_id: id,
                content: entry.content.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    apply_pending_edit(args.queue, selected, messages).await
}
