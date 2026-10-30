use super::{fake_llm, Attachment, ChatAction};
use anyhow::{ensure, Context, Result};
use gents::{graphql::escape_graphql_string, ConfigAccess};
use gents_protocol::row::AgentRequestRow;
use serde_json::{json, Value};
use std::path::Path;

pub const PROMPT: &str = "Update the recorded review fixture.";
pub const REPLY: &str = "The recorded file is ready for review.";
const CALL: &str = "recorded-file-write";
const FILE: &str = "review.txt";
const LATER: &str = "Later user changes must stay on disk.\n";

fn contents(marker: &str) -> String {
    format!(
        "{marker}\r\n{}\r\nretained final line",
        "unchanged line\r\n".repeat(240)
    )
}

pub fn prepare(root: &Path) -> Result<()> {
    let before = contents("Recorded original");
    let after = contents("Recorded replacement");
    std::fs::write(root.join(FILE), &before)?;
    std::fs::write(
        root.join("editor-review-fixture.json"),
        serde_json::to_vec(&json!({
            "prompt": PROMPT, "reply": REPLY, "file": FILE, "before": before,
            "after": after, "later": LATER,
            "beforeMarker": "Recorded original", "afterMarker": "Recorded replacement",
        }))?,
    )?;
    Ok(())
}

pub fn reply(request: &Value) -> Option<ChatAction> {
    let messages = request["messages"].as_array()?;
    let selected = messages.iter().any(|message| {
        message["role"] == "user"
            && match &message["content"] {
                Value::String(text) => text == PROMPT,
                Value::Array(parts) => parts.iter().any(|part| part["text"] == PROMPT),
                _ => false,
            }
    });
    if !selected {
        return None;
    }
    let completed = messages
        .iter()
        .any(|message| message["role"] == "tool" && message["tool_call_id"] == CALL);
    Some(ChatAction::Sse(if completed {
        fake_llm::completion_text_sse(REPLY)
    } else {
        fake_llm::tool_call_sse_with_id(
            CALL,
            "write_file",
            &json!({
                "path": FILE, "content": contents("Recorded replacement"),
                "overwrite": true, "raw_json": true,
            })
            .to_string(),
        )
    }))
}

#[allow(clippy::too_many_arguments)]
pub async fn verify(
    client: &mut Attachment,
    id: &mut u32,
    access: &ConfigAccess,
    root: &Path,
    node: &str,
    session: &str,
    author: &str,
) -> Result<Value> {
    let ui: Value = serde_json::from_slice(&std::fs::read(root.join("editor-review-ui.json"))?)?;
    ensure!(
        ui["completeDiffOpened"] == true
            && ui["readOnly"] == true
            && ui["laterWorkingCopyPreserved"] == true,
        "editor review acceptance is incomplete"
    );
    let query = format!("{{ AgentRequest(filter: {{ node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}} }}) {{ {} }} }}",
        escape_graphql_string(node), escape_graphql_string(session), gents::SIGNED_REQUEST_FIELDS);
    let rows = access.execute(&query).await?;
    let rows: Vec<AgentRequestRow> = serde_json::from_value(rows["data"]["AgentRequest"].clone())?;
    let matches = rows
        .iter()
        .filter(|row| row.content.as_deref() == Some(PROMPT))
        .collect::<Vec<_>>();
    ensure!(
        matches.len() == 1,
        "review input was not admitted exactly once"
    );
    let request = matches[0];
    gents::verify_request_receipt_signature(request)?;
    ensure!(
        request.requester_did.as_deref() == Some(author),
        "review authority changed"
    );
    let query = format!("{{ AgentToolCall(filter: {{ request_doc_id: {{_eq: \"{}\"}} }}) {{ _docID lifecycle_state }} }}",
        escape_graphql_string(request.doc_id.as_deref().context("physical review request")?));
    let calls = access.execute(&query).await?;
    let calls = calls["data"]["AgentToolCall"]
        .as_array()
        .context("physical file tool records")?;
    ensure!(
        calls.len() == 1 && calls[0]["lifecycle_state"] == "completed",
        "review must use one completed native file mutation"
    );
    let physical = calls[0]["_docID"]
        .as_str()
        .context("physical review tool")?;
    let retained = access.execute(&format!(
        r#"{{ ToolFileChange(filter: {{ tool_call_doc_id: {{ _eq: "{}" }} }}) {{ _docID file_changes }} }}"#,
        escape_graphql_string(physical),
    )).await?;
    let observations = retained["data"]["ToolFileChange"]
        .as_array()
        .context("retained file observations")?;
    ensure!(
        observations.len() == 1,
        "review must retain exactly one file observation"
    );
    let images = client
        .call(
            *id,
            "desktop_tool_file_changes",
            json!({
                "nodeDid": node, "sessionId": session, "requestId": request.request_id,
                "toolCallDocId": physical,
            }),
        )
        .await;
    *id += 1;
    ensure!(
        images == observations[0]["file_changes"],
        "client transport changed retained images"
    );
    ensure!(
        images["changes"][0]["before"]["kind"] == "file"
            && images["changes"][0]["after"]["kind"] == "file",
        "both immutable file versions must be retained"
    );
    ensure!(
        node != author,
        "review must cross enrolled client identities"
    );
    let delivered = access
        .execute(&format!(
            r#"{{ FileImageHydrationRequest(filter: {{ tool_call_doc_id: {{ _eq: "{}" }} }}) {{
            request_key peer_id requester_did node_did session_id request_doc_id tool_call_doc_id
            status observation_doc_id outcome_signer_did outcome_signature
        }} }}"#,
            escape_graphql_string(physical),
        ))
        .await?;
    let receipts = delivered["data"]["FileImageHydrationRequest"]
        .as_array()
        .context("remote review delivery receipts")?;
    ensure!(
        receipts.len() == 1,
        "review demand must be reused after delivery; found {} receipts",
        receipts.len()
    );
    let receipt = &receipts[0];
    let mut demand = gents_protocol::file_image_hydration::FileImageDemand {
        inventory_reference: None,
        admission: gents_protocol::session_hydration::HydrationRequest {
            request_key: String::new(),
            peer_id: receipt["peer_id"].as_str().context("review peer")?.into(),
            requester_did: author.into(),
            node_did: node.into(),
            session_id: session.into(),
        },
        request_doc_id: request
            .doc_id
            .as_deref()
            .context("physical review request")?
            .into(),
        tool_call_doc_id: physical.into(),
    };
    demand.admission.request_key = demand.derive_request_key();
    for (field, expected) in demand.reference_fields() {
        ensure!(
            receipt[field].as_str() == Some(expected),
            "review receipt changed {field}"
        );
    }
    ensure!(
        receipt["status"] == "served"
            && receipt["observation_doc_id"] == observations[0]["_docID"]
            && receipt["outcome_signer_did"] == node
            && receipt["outcome_signature"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
        "review receipt did not identify the delivered physical image"
    );
    ensure!(
        std::fs::read_to_string(root.join(FILE))? == LATER,
        "review overwrote later user changes"
    );
    Ok(
        json!({"sessionId":session, "requestId":request.request_id, "physicalTool":physical,
        "enrolledClientRead":true, "retainedImagesUnchanged":true, "workingCopyPreserved":true,
        "scopedDeliveryReceipt":true, "exactImageIdentity":true, "demandReused":true}),
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn verify_permission_recovery(
    client: &mut Attachment,
    id: &mut u32,
    access: &ConfigAccess,
    root: &Path,
    node_home: &Path,
    author: &str,
    baseline: &Value,
    verifier: &dyn gents::NodeIdentity,
) -> Result<Value> {
    use gents_protocol::file_image_hydration::FileImageDemand;
    use gents_protocol::session_hydration::HydrationRequest;
    use std::time::Duration;

    const CACHED_RETRY: &str = "030ee487-7cca-496d-be85-bd35f686f154";
    const BLOCKED_RETRY: &str = "56eba703-e534-43b5-980c-203f37523811";
    const RESTORED_RETRY: &str = "4be6aa69-c87a-44bb-a946-2452ac3a283b";
    let physical = field(baseline, "physicalTool")?;
    let receipts = review_receipts(access, physical).await?;
    ensure!(receipts.len() == 1, "initial review has multiple demands");
    let original = receipts[0].clone();
    let demand = FileImageDemand {
        inventory_reference: None,
        admission: HydrationRequest {
            request_key: field(&original, "request_key")?.into(),
            peer_id: field(&original, "peer_id")?.into(),
            requester_did: author.into(),
            node_did: field(&original, "node_did")?.into(),
            session_id: field(baseline, "sessionId")?.into(),
        },
        request_doc_id: field(&original, "request_doc_id")?.into(),
        tool_call_doc_id: physical.into(),
    };
    for (name, value) in demand.reference_fields() {
        ensure!(
            original[name].as_str() == Some(value),
            "initial receipt changed {name}"
        );
    }
    let enrollment = access.execute(&format!(
        r#"{{ NetworkEnrollmentRequest(filter: {{ candidate_did: {{ _eq: "{}" }}, owner_agent: {{ _eq: "{}" }} }}) {{ request_id candidate_peer }} }}"#,
        escape_graphql_string(author), escape_graphql_string(&demand.admission.node_did),
    )).await?;
    let enrollments = enrollment["data"]["NetworkEnrollmentRequest"]
        .as_array()
        .context("initial enrollment rows")?;
    ensure!(enrollments.len() == 1, "initial enrollment is ambiguous");
    let old_request = field(&enrollments[0], "request_id")?;
    ensure!(
        enrollments[0]["candidate_peer"] == demand.admission.peer_id,
        "review peer differs from enrollment"
    );
    verify_model_receipt(verifier, &original, &demand).await?;
    let inventory =
        super::editor_inventory_permissions::InventoryPermissions::capture(client, id, &demand)
            .await
            .context("initial inventory permission capture")?;
    let args = json!({
        "nodeDid": demand.admission.node_did, "sessionId": demand.admission.session_id,
        "requestId": baseline["requestId"], "toolCallDocId": physical,
    });
    let mut cached_args = args.clone();
    cached_args["retryId"] = json!(CACHED_RETRY);
    let cached_retry = client
        .call(*id, "desktop_tool_file_changes", cached_args.clone())
        .await;
    *id += 1;
    enrollment_decision(root, node_home, "revoke", old_request).await?;
    wait_review_route(client, id, &demand.admission.node_did, false).await?;

    let cached = client
        .try_call(*id, "desktop_tool_file_changes", args.clone())
        .await?;
    *id += 1;
    ensure!(
        cached["tool_call_doc_id"] == physical
            && cached["version"] == gents_protocol::file_changes::FILE_CHANGES_VERSION,
        "revocation hid an already retained image"
    );
    let replayed_cache = client
        .call(*id, "desktop_tool_file_changes", cached_args)
        .await;
    *id += 1;
    ensure!(
        replayed_cache == cached_retry && replayed_cache == cached,
        "revocation hid a served explicit retry"
    );
    let mut blocked_args = args.clone();
    blocked_args["retryId"] = json!(BLOCKED_RETRY);
    let started = tokio::time::Instant::now();
    let blocked = client
        .try_call_with_timeout(
            *id,
            "desktop_tool_file_changes",
            blocked_args,
            Duration::from_secs(45),
        )
        .await;
    *id += 1;
    let error = blocked.expect_err("revoked enrollment must not serve a fresh review demand");
    let inactive_read_ms = started.elapsed().as_millis();
    ensure!(
        started.elapsed() < Duration::from_secs(5),
        "inactive review waited {inactive_read_ms} ms"
    );
    ensure!(
        error
            .message
            .contains("Reconnect this runtime to load new file versions.")
            || error.message.contains("Runtime rejected file review"),
        "revoked review failed for an unrelated reason: {error}"
    );
    let blocked_key = demand.derive_retry_request_key(BLOCKED_RETRY);
    let denied = review_receipts(access, physical)
        .await?
        .into_iter()
        .find(|row| row["request_key"] == blocked_key);
    if let Some(row) = &denied {
        let mut blocked_demand = demand.clone();
        blocked_demand.admission.request_key = blocked_key.clone();
        verify_model_receipt(verifier, row, &blocked_demand).await?;
        ensure!(
            row["status"] == "rejected",
            "revoked review acquired a terminal delivery: {row}"
        );
        ensure!(
            row["observation_doc_id"].is_null(),
            "revoked review named an image"
        );
    }

    let revoked_inventory = inventory
        .revoked(client, id)
        .await
        .context("retained inventory after revocation")?;

    let status_url = field(
        &serde_json::from_slice::<Value>(&std::fs::read(node_home.join("runtime.json"))?)?,
        "graphql",
    )?
    .to_owned();
    let server = status_url
        .strip_suffix("/api/v0/graphql")
        .context("runtime status base")?;
    let pending = client
        .call(
            *id,
            "desktop_peer_enroll_status",
            json!({"request":{"serverAddress":server}}),
        )
        .await;
    *id += 1;
    let next_request = field(&pending, "requestId")?;
    ensure!(
        next_request != old_request,
        "reenrollment reused the revoked request"
    );
    wait_enrollment_arrival(access, next_request).await?;
    enrollment_decision(root, node_home, "approve", next_request)
        .await
        .with_context(|| {
            format!("reenrollment approval after retained-inventory checks: {revoked_inventory}")
        })?;
    wait_review_route(client, id, &demand.admission.node_did, true).await?;
    let mut restored_args = args.clone();
    restored_args["retryId"] = json!(RESTORED_RETRY);
    let restored = client
        .try_call_with_timeout(
            *id,
            "desktop_tool_file_changes",
            restored_args.clone(),
            Duration::from_secs(45),
        )
        .await?;
    *id += 1;
    ensure!(
        restored == cached,
        "reenrollment changed the retained images"
    );
    let replayed = client
        .call(*id, "desktop_tool_file_changes", restored_args)
        .await;
    *id += 1;
    ensure!(
        replayed == restored,
        "explicit retry replay changed retained images"
    );
    let resumed = client.call(*id, "desktop_tool_file_changes", args).await;
    *id += 1;
    ensure!(
        resumed == restored,
        "normal reopening did not resume verified review"
    );
    let receipts = review_receipts(access, physical).await?;
    let restored_key = demand.derive_retry_request_key(RESTORED_RETRY);
    let matches: Vec<_> = receipts
        .iter()
        .filter(|row| row["request_key"] == restored_key)
        .collect();
    ensure!(
        matches.len() == 1 && matches[0]["status"] == "served",
        "explicit retry was not served exactly once"
    );
    let mut retried = demand.clone();
    retried.admission.request_key = restored_key;
    verify_model_receipt(verifier, matches[0], &retried).await?;
    for (name, value) in retried.reference_fields() {
        ensure!(
            matches[0][name].as_str() == Some(value),
            "restored receipt changed {name}"
        );
    }
    ensure!(
        matches[0]["observation_doc_id"] == original["observation_doc_id"],
        "retry selected another image"
    );
    ensure!(
        receipts.iter().any(|row| row == &original),
        "retry changed the original receipt"
    );
    if let Some(denied) = &denied {
        ensure!(
            receipts.iter().any(|row| row == denied),
            "reenrollment reset a signed refusal"
        );
    }
    ensure!(
        std::fs::read_to_string(root.join(FILE))? == LATER,
        "permission recovery changed the working copy"
    );
    let restored_inventory = inventory
        .restored(client, id, access, verifier)
        .await
        .context("inventory after reenrollment")?;
    Ok(json!({
        "inventoryRevoked":revoked_inventory, "inventoryRestored":restored_inventory,
        "baseline":baseline, "revokedRequest":old_request, "restoredRequest":next_request,
        "revokedFreshReviewRefused":true, "revokedReadError":error.message,
        "cachedImageReadableAfterRevocation":true, "servedRetryReadableAfterRevocation":true,
        "inactiveReadMs":inactive_read_ms, "signedRefusalObserved":denied.is_some(),
        "freshRetryServed":true, "retryReplayReused":true, "normalReopenResumed":true,
        "originalReceiptPreserved":true, "workingCopyPreserved":true,
    }))
}

fn field<'a>(row: &'a Value, name: &str) -> Result<&'a str> {
    row[name]
        .as_str()
        .with_context(|| format!("review fixture omitted {name}"))
}

pub(super) async fn review_receipts(access: &ConfigAccess, physical: &str) -> Result<Vec<Value>> {
    let response = access
        .execute(&format!(
            r#"{{ FileImageHydrationRequest(filter: {{ tool_call_doc_id: {{ _eq: "{}" }} }}) {{
        request_key peer_id requester_did node_did session_id request_doc_id tool_call_doc_id
        status status_detail observation_doc_id processed_at outcome_signer_did outcome_signature
    }} }}"#,
            escape_graphql_string(physical)
        ))
        .await?;
    Ok(response["data"]["FileImageHydrationRequest"]
        .as_array()
        .context("review receipts")?
        .clone())
}

async fn enrollment_decision(root: &Path, home: &Path, action: &str, request: &str) -> Result<()> {
    let cli = super::support::native_binary("gents")?;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new(cli)
            .current_dir(root)
            .args(["p2p", "enrollment", action, request, "--home"])
            .arg(home)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        result.status.success(),
        "enrollment {action} failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}

async fn wait_review_route(
    client: &mut Attachment,
    id: &mut u32,
    node: &str,
    ready: bool,
) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let snapshot = client.snapshot(*id).await;
            *id += 1;
            let deployment = snapshot["client"]["deployments"]
                .as_array()
                .context("client deployments")?
                .iter()
                .find(|row| row["nodeDid"] == node);
            if deployment.is_some_and(|row| row["chatSafe"] == true) == ready {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .context("client did not observe the enrollment change")?
}

async fn wait_enrollment_arrival(access: &ConfigAccess, request: &str) -> Result<()> {
    let query = format!(
        r#"{{ NetworkEnrollmentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
        escape_graphql_string(request)
    );
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let response = access.execute(&query).await?;
            if response["data"]["NetworkEnrollmentRequest"]
                .as_array()
                .is_some_and(|rows| rows.len() == 1)
            {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .context("runtime did not observe reenrollment")?
}

pub(super) async fn verify_model_receipt(
    verifier: &dyn gents::NodeIdentity,
    row: &Value,
    demand: &gents_protocol::file_image_hydration::FileImageDemand,
) -> Result<()> {
    use gents_protocol::file_image_hydration::{
        FileImageHydrationReceipt, FileImageSelection, FILE_IMAGE_RECEIPT_VERSION,
    };
    for (name, expected) in demand.reference_fields() {
        ensure!(
            row[name].as_str() == Some(expected),
            "review receipt changed {name}"
        );
    }
    let selection = match field(row, "status")? {
        "rejected" => {
            ensure!(
                row["observation_doc_id"].is_null(),
                "refusal named an observation"
            );
            FileImageSelection::Rejected
        }
        "served" => match &row["observation_doc_id"] {
            Value::Null => FileImageSelection::Missing,
            Value::String(document) => FileImageSelection::Selected {
                document: document.clone(),
            },
            _ => anyhow::bail!("invalid retained observation identity"),
        },
        status => anyhow::bail!("review receipt is not terminal: {status}"),
    };
    let receipt = FileImageHydrationReceipt {
        version: FILE_IMAGE_RECEIPT_VERSION,
        demand: demand.clone(),
        selection,
        detail: field(row, "status_detail")?.into(),
        processed_at: field(row, "processed_at")?.into(),
        signer_did: field(row, "outcome_signer_did")?.into(),
        signature: bs58::decode(field(row, "outcome_signature")?).into_vec()?,
    };
    gents::tools::call::lifecycle::VerifiedFileImageReceipt::verify(receipt, demand, verifier)
        .await?;
    Ok(())
}
