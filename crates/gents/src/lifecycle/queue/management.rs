use super::intake::pending_in_txn;
use super::*;
use crate::config_client::ConfigAccess;
use crate::identity::NodeIdentity;
use crate::lifecycle::{RequestIdentity, RequestSigner, RequestSpec, WorkspaceLineage};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{QueueDelivery, QueuePosition};
use serde::{Deserialize, Serialize};

pub const REPLACED_REASON: &str = "pending message replaced";
pub const REMOVED_REASON: &str = "Pending message removed by its author";

pub use gents_protocol::session_input_edit::{
    PendingMessageEdit, PendingQueueEdit, PendingQueueReceipt,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PendingQueueSnapshot {
    pub entries: Vec<PendingQueueEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PendingQueueEntry {
    pub request_doc_id: String,
    pub request_id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captured_context: Vec<gents_protocol::request_input::CapturedContext>,
    pub editable: bool,
    pub steerable: bool,
    pub edit_group: Option<String>,
}

pub async fn pending_user_queue(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: &str,
) -> Result<PendingQueueSnapshot> {
    Ok(
        pending_user_queue_projection(access, node_did, session_id, requester_did, &[])
            .await?
            .pending,
    )
}

pub struct PendingQueueProjection {
    pub pending: PendingQueueSnapshot,
    pub arrival_order: Vec<String>,
}

/// Pending controls and receipt-validated arrival order share one read snapshot.
/// Arrival evidence includes requests a concurrent claim already took out of
/// the pending listing; their earlier client observations still need ordering.
pub async fn pending_user_queue_projection(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: &str,
    observed_doc_ids: &[String],
) -> Result<PendingQueueProjection> {
    access
        .transact_readonly("queue.pending_user_snapshot", |txn| {
            Box::pin(async move {
                let rows = pending_in_txn(txn, node_did, session_id).await?;
                let entries = rows
                    .into_iter()
                    .map(|row| {
                        let can_edit = editable(&row, requester_did);
                        let edit_group = can_edit
                            .then(|| {
                                serde_json::to_string(&(
                                    &row.agent_id,
                                    &row.workspace_id,
                                    &row.workspace_owner_node_did,
                                    &row.workspace_authority,
                                    &row.workspace_seal_hash,
                                    row.input.as_ref().map(|input| {
                                        (
                                            &input.cwd,
                                            &input.selected_skill_ids,
                                            input.queue.as_ref().map(|queue| queue.delivery),
                                        )
                                    }),
                                ))
                            })
                            .transpose()?;
                        Ok(PendingQueueEntry {
                            request_doc_id: row
                                .doc_id
                                .context("pending row lacks physical identity")?,
                            request_id: row.request_id,
                            content: if row.requester_did.as_deref() == Some(requester_did) {
                                row.content.unwrap_or_default()
                            } else {
                                String::new()
                            },
                            captured_context: if row.requester_did.as_deref() == Some(requester_did)
                            {
                                row.input
                                    .as_ref()
                                    .map(|input| input.captured_context.clone())
                                    .unwrap_or_default()
                            } else {
                                Vec::new()
                            },
                            editable: can_edit,
                            steerable: can_edit
                                && row
                                    .input
                                    .as_ref()
                                    .and_then(|input| input.queue.as_ref())
                                    .is_some_and(|queue| queue.delivery == QueueDelivery::Queue),
                            edit_group,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                let arrival_order =
                    crate::trigger_engine::durable::request_arrival_order(txn, observed_doc_ids)
                        .await?;
                Ok(PendingQueueProjection {
                    pending: PendingQueueSnapshot { entries },
                    arrival_order,
                })
            })
        })
        .await
}

fn editable(row: &AgentRequestRow, requester: &str) -> bool {
    row.requester_did.as_deref() == Some(requester)
        && row.admission_signer_did.as_deref() == Some(requester)
        && crate::request_admission::verify_request_receipt_signature(row).is_ok()
        && matches!(
            crate::lifecycle::parse_valid_until(row.valid_until.as_deref(), chrono::Utc::now()),
            crate::lifecycle::TtlOutcome::NotSet | crate::lifecycle::TtlOutcome::Live(_)
        )
        && row.lifecycle_state == Some(RequestLifecycleState::Pending)
        && row.execution_generation.is_none()
        && row.interrupt_requested_at.is_none()
        && row.execution_origin.as_deref() == Some("interactive")
        && row.retry_parent_request.is_none()
        && row.caused_by_trigger_id.is_none()
        && row.caused_by_source_doc_id.is_none()
        && row.caused_by_parent_tool_call_doc_id.is_none()
        && row.input.as_ref().is_some_and(|input| {
            input.goal_continuation.is_none()
                && input.queue.as_ref().is_some_and(|queue| {
                    queue.source == QueueSource::User
                        && queue.policy == QueuePolicy::Append
                        && queue.key.is_none()
                        && queue.interrupted_request_id.is_none()
                })
        })
}

fn selected_group<'a>(
    rows: &'a [AgentRequestRow],
    edit: &PendingQueueEdit,
    requester: &str,
) -> Result<&'a [AgentRequestRow]> {
    let ids = rows
        .iter()
        .map(|row| row.doc_id.clone().unwrap_or_default())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        ids == edit.expected_request_doc_ids,
        "The message queue changed. Refresh it and try again."
    );
    anyhow::ensure!(
        !edit.selected_request_doc_ids.is_empty(),
        "Select a pending message first."
    );
    let count = edit.selected_request_doc_ids.len();
    let start = ids
        .windows(count)
        .position(|group| group == edit.selected_request_doc_ids)
        .context("Selected messages are no longer adjacent in the queue.")?;
    let selected = &rows[start..start + count];
    anyhow::ensure!(
        selected.iter().all(|row| editable(row, requester)),
        "A selected message has started or cannot be edited by this user."
    );
    let first = &selected[0];
    anyhow::ensure!(
        selected.iter().all(|row| row.agent_id == first.agent_id
            && row.workspace_id == first.workspace_id
            && row.workspace_authority == first.workspace_authority
            && row.workspace_owner_node_did == first.workspace_owner_node_did
            && row.workspace_seal_hash == first.workspace_seal_hash
            && row
                .input
                .as_ref()
                .map(|input| (&input.cwd, &input.selected_skill_ids))
                == first
                    .input
                    .as_ref()
                    .map(|input| (&input.cwd, &input.selected_skill_ids))),
        "Messages with different execution settings cannot be reordered together."
    );
    anyhow::ensure!(
        edit.messages.is_empty() || edit.messages.len() == count,
        "Replacement must preserve the selected message count."
    );
    let mut sources = rapidhash::RapidHashSet::default();
    for message in &edit.messages {
        anyhow::ensure!(
            edit.selected_request_doc_ids
                .contains(&message.request_doc_id)
                && sources.insert(message.request_doc_id.as_str()),
            "Replacement messages must name each selected message once."
        );
        anyhow::ensure!(
            selected
                .iter()
                .find(|row| row.doc_id.as_deref() == Some(message.request_doc_id.as_str()))
                .and_then(|row| row.input.as_ref())
                .is_some_and(|input| input.has_authored_input(&message.content)),
            "A message cannot be empty. Remove it instead."
        );
    }
    Ok(selected)
}

pub(super) fn replacement_delivery_allowed(before: QueueDelivery, after: QueueDelivery) -> bool {
    before == after || (before == QueueDelivery::Queue && after == QueueDelivery::Steer)
}

struct ReplacementIdentity<'a> {
    node_did: &'a str,
    session_id: &'a str,
    requester_did: &'a str,
    request_id: String,
    created_at: String,
    delivery: QueueDelivery,
}

fn replacement_spec(
    selected: &[AgentRequestRow],
    slot: &AgentRequestRow,
    message: &PendingMessageEdit,
    admission: AgentRequestAdmissionRecord,
    identity: ReplacementIdentity<'_>,
) -> Result<RequestSpec> {
    let ReplacementIdentity {
        node_did,
        session_id,
        requester_did,
        request_id,
        created_at,
        delivery,
    } = identity;
    let source = selected
        .iter()
        .find(|row| row.doc_id.as_deref() == Some(message.request_doc_id.as_str()))
        .unwrap();
    let mut input = source.input.clone().unwrap_or_default().reissued(
        source
            .doc_id
            .as_deref()
            .context("selected message has no physical identity")?,
    );
    input.initial_title = None;
    let slot_id = slot
        .doc_id
        .clone()
        .context("selected slot has no identity")?;
    let old_queue = slot
        .input
        .as_ref()
        .and_then(|input| input.queue.as_ref())
        .context("selected slot has no queue input")?;
    let queue = input
        .queue
        .as_mut()
        .context("selected message has no queue input")?;
    anyhow::ensure!(
        replacement_delivery_allowed(old_queue.delivery, delivery),
        "Steering input cannot be changed back to queued delivery."
    );
    queue.delivery = delivery;
    queue.position = Some(QueuePosition {
        slot_request_doc_id: old_queue
            .position
            .as_ref()
            .map(|position| position.slot_request_doc_id.clone())
            .unwrap_or_else(|| slot_id.clone()),
        replaces_request_doc_id: slot_id,
    });
    let mut spec = RequestSpec::new(
        RequestPurpose::Normal,
        RequestIdentity {
            requester_did: Some(requester_did.to_owned()),
            request_id,
            node_did: node_did.to_owned(),
            agent_id: source.agent_id.clone().context("message has no agent")?,
            session_id: session_id.to_owned(),
            content: message.content.trim().to_owned(),
            execution_origin: ExecutionOrigin::Interactive,
            created_at,
        },
        admission,
    );
    spec.input = input;
    spec.valid_until = source.valid_until.clone();
    spec.workspace = Some(WorkspaceLineage {
        workspace_id: source.workspace_id.clone(),
        workspace_owner_node_did: source.workspace_owner_node_did.clone(),
        workspace_authority: source.workspace_authority.clone(),
        workspace_seal_hash: source.workspace_seal_hash.clone(),
    });
    Ok(spec)
}

pub async fn prepare_pending_user_edit(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    node_did: &str,
    session_id: &str,
    requester_did: &str,
    edit: &PendingQueueEdit,
) -> Result<Vec<AgentRequestCreate>> {
    prepare_pending_user_edit_with_deliveries(
        access,
        signer,
        admission,
        node_did,
        session_id,
        requester_did,
        edit,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn prepare_pending_user_edit_with_deliveries(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    node_did: &str,
    session_id: &str,
    requester_did: &str,
    edit: &PendingQueueEdit,
    deliveries: Option<&[QueueDelivery]>,
) -> Result<Vec<AgentRequestCreate>> {
    anyhow::ensure!(
        signer.did() == requester_did && admission.signer_did == requester_did,
        "Only the author can edit pending messages."
    );
    anyhow::ensure!(
        deliveries.is_none_or(|values| values.len() == edit.messages.len()),
        "Replacement delivery count changed."
    );
    let rows = access
        .transact_readonly("queue.read_pending_edit", |txn| {
            Box::pin(async move { pending_in_txn(txn, node_did, session_id).await })
        })
        .await?;
    let selected = selected_group(&rows, edit, requester_did)?;
    let mut creates = Vec::new();
    for (index, (slot, message)) in selected.iter().zip(&edit.messages).enumerate() {
        let delivery = match deliveries {
            Some(values) => values[index],
            None => {
                selected
                    .iter()
                    .find(|row| row.doc_id.as_deref() == Some(message.request_doc_id.as_str()))
                    .and_then(|row| row.input.as_ref())
                    .and_then(|input| input.queue.as_ref())
                    .context("Selected message has no queue input.")?
                    .delivery
            }
        };
        let spec = replacement_spec(
            selected,
            slot,
            message,
            admission.clone(),
            ReplacementIdentity {
                node_did,
                session_id,
                requester_did,
                request_id: uuid::Uuid::new_v4().to_string(),
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                delivery,
            },
        )?;
        creates.push(
            crate::lifecycle::build_signed_request(spec, RequestSigner::Identity(signer)).await?,
        );
    }
    Ok(creates)
}

pub(crate) struct ValidatedPendingEdit {
    node_did: String,
    selected: Vec<AgentRequestRow>,
    creates: Vec<AgentRequestCreate>,
}

#[cfg(test)]
mod input_reissuance_tests {
    use super::*;

    #[tokio::test]
    async fn generated_reissued_inputs_keep_reordered_messages_goal_and_captures() {
        let temp = tempfile::tempdir().unwrap();
        let signer =
            crate::identity::KeyIdentity::load_or_create(temp.path().join("node.key"), None)
                .unwrap();
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().request_input_cases;
        let mut exercised = 0;
        for case in cases
            .iter()
            .filter(|case| !case["input"]["goal_assignment"].is_null())
        {
            let mut input: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
            input.queue = Some(RequestQueue {
                source: QueueSource::User,
                policy: QueuePolicy::Append,
                delivery: QueueDelivery::Queue,
                position: None,
                key: None,
                queued_after_request_id: Some("active".into()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            });
            let source = AgentRequestRow {
                doc_id: Some(case["reissued_source_doc_id"].as_str().unwrap().into()),
                request_id: "source-message".into(),
                agent_id: Some("coding".into()),
                input: Some(input.clone()),
                ..Default::default()
            };
            let slot = AgentRequestRow {
                doc_id: Some("physical:different-slot".into()),
                request_id: "slot-message".into(),
                ..source.clone()
            };
            let selected = [slot.clone(), source.clone()];
            let spec = replacement_spec(
                &selected,
                &slot,
                &PendingMessageEdit {
                    request_doc_id: source.doc_id.clone().unwrap(),
                    content: "Edited text in the other slot".into(),
                },
                AgentRequestAdmissionRecord::local_self(signer.did()),
                ReplacementIdentity {
                    node_did: signer.did(),
                    session_id: "session",
                    requester_did: signer.did(),
                    request_id: "replacement".into(),
                    created_at: "2026-10-21T00:00:00Z".into(),
                    delivery: QueueDelivery::Steer,
                },
            )
            .unwrap();
            let create =
                crate::lifecycle::build_signed_request(spec, RequestSigner::Identity(&signer))
                    .await
                    .unwrap();
            assert!(crate::identity::verify_did_signature(
                signer.did(),
                &create.signing_payload(),
                &create.admission.signature,
            )
            .unwrap());
            let mut expected: RequestInput =
                serde_json::from_value(case["expected_reissued_input"].clone()).unwrap();
            expected.initial_title = None;
            expected.queue = create.input.queue.clone();
            assert_eq!(create.input, expected, "{}", case["name"]);
            let position = create.input.queue.unwrap().position.unwrap();
            assert_eq!(position.slot_request_doc_id, slot.doc_id.clone().unwrap());
            assert_eq!(position.replaces_request_doc_id, slot.doc_id.unwrap());
            assert_eq!(source.input, Some(input), "source input remains immutable");
            exercised += 1;
        }
        assert_eq!(exercised, 6);
    }
}

pub(crate) async fn validate_pending_user_edit_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: &str,
    edit: &PendingQueueEdit,
    creates: &[AgentRequestCreate],
) -> Result<std::result::Result<ValidatedPendingEdit, String>> {
    crate::graphql::fence_arrivals(txn, "AgentRequest").await?;
    let current = pending_in_txn(txn, node_did, session_id).await?;
    let checked = (|| -> Result<ValidatedPendingEdit> {
        let selected = selected_group(&current, edit, requester_did)?;
        anyhow::ensure!(
            creates.len() == edit.messages.len(),
            "Replacement request count changed."
        );
        let mut ids = rapidhash::RapidHashSet::default();
        for ((slot, message), create) in selected.iter().zip(&edit.messages).zip(creates) {
            anyhow::ensure!(
                ids.insert(&create.request_id) && !create.request_id.is_empty(),
                "Replacement request identity is not fresh."
            );
            anyhow::ensure!(
                create.admission.signer_did == requester_did,
                "Only the author can replace pending messages."
            );
            create.admission.validate_canonical_fields()?;
            create
                .admission
                .validate_branch_fields()
                .map_err(anyhow::Error::msg)?;
            let expected = super::super::materialize::build_request(replacement_spec(
                selected,
                slot,
                message,
                create.admission.clone(),
                ReplacementIdentity {
                    node_did,
                    session_id,
                    requester_did,
                    request_id: create.request_id.clone(),
                    created_at: create.created_at.clone(),
                    delivery: create
                        .input
                        .queue
                        .as_ref()
                        .context("Replacement has no queue input.")?
                        .delivery,
                },
            )?)?;
            anyhow::ensure!(
                create == &expected,
                "Replacement request does not match the selected message."
            );
            create.graphql_input_fields().map_err(anyhow::Error::msg)?;
            anyhow::ensure!(
                crate::identity::verify_did_signature(
                    requester_did,
                    &create.signing_payload(),
                    &create.admission.signature
                )?,
                "Replacement request signature is invalid."
            );
        }
        Ok(ValidatedPendingEdit {
            node_did: node_did.to_owned(),
            selected: selected.to_vec(),
            creates: creates.to_vec(),
        })
    })();
    let validated = match checked {
        Ok(validated) => validated,
        Err(error) => return Ok(Err(error.to_string())),
    };
    if persisted_replacements_in_txn(txn, node_did, creates).await? != PersistedReplacements::None {
        return Ok(Err("Replacement request identity already exists.".into()));
    }
    Ok(Ok(validated))
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PersistedReplacements {
    None,
    Exact(Vec<String>),
    Conflicting,
}

pub(crate) async fn persisted_replacements_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    creates: &[AgentRequestCreate],
) -> Result<PersistedReplacements> {
    let mut exact = Vec::new();
    let mut missing = 0;
    for create in creates {
        let response = txn.execute(&format!(
            r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, request_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID admission_signature }} }}"#,
            escape_graphql_string(node_did), escape_graphql_string(&create.request_id)
        )).await?;
        let existing = response["data"]["AgentRequest"]
            .as_array()
            .context("replacement identity lookup missing")?;
        match existing.first() {
            None => missing += 1,
            Some(row)
                if row["admission_signature"].as_str()
                    == Some(
                        bs58::encode(&create.admission.signature)
                            .into_string()
                            .as_str(),
                    ) =>
            {
                exact.push(
                    row["_docID"]
                        .as_str()
                        .context("replacement lacks physical identity")?
                        .to_owned(),
                );
            }
            Some(_) => return Ok(PersistedReplacements::Conflicting),
        }
    }
    Ok(match (missing, exact.is_empty()) {
        (_, true) => PersistedReplacements::None,
        (0, false) => PersistedReplacements::Exact(exact),
        _ => PersistedReplacements::Conflicting,
    })
}

pub(crate) async fn apply_pending_user_edit_in_txn(
    txn: &ConfigApplyTxn<'_>,
    validated: ValidatedPendingEdit,
) -> Result<PendingQueueReceipt> {
    let ValidatedPendingEdit {
        node_did,
        selected,
        creates,
    } = validated;
    let node_did = node_did.as_str();
    let mut receipt = PendingQueueReceipt {
        request_doc_ids: Vec::new(),
        request_ids: Vec::new(),
    };
    for create in &creates {
        let response = txn
            .execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
            .await?;
        let id = crate::graphql::created_doc_id(&response, "AgentRequest")?;
        receipt.request_doc_ids.push(id);
        receipt.request_ids.push(create.request_id.clone());
    }
    for (index, old) in selected.iter().enumerate() {
        let old_id = old.doc_id.as_deref().unwrap();
        let mutation = if let Some(new_id) = receipt.request_doc_ids.get(index) {
            supersede_pending_mutation(
                old_id,
                node_did,
                &receipt.request_ids[index],
                new_id,
                REPLACED_REASON,
            )
        } else {
            format!(
                r#"mutation($terminal_output: JSON) {{ update_AgentRequest(docID: "{}", filter: {{
                        _docID: {{ _eq: "{}" }}, lifecycle_state: {{ _eq: "pending" }}
                    }}, input: {{ lifecycle_state: "interrupted", interrupt_requested_at: "{}",
                        terminalized_at: "{}", failure_reason: "{}",
                        terminal_redrive_attempts: 0, terminal_output: $terminal_output }}) {{ _docID }} }}"#,
                escape_graphql_string(old_id),
                escape_graphql_string(old_id),
                escape_graphql_string(&chrono::Utc::now().to_rfc3339()),
                escape_graphql_string(&chrono::Utc::now().to_rfc3339()),
                escape_graphql_string(REMOVED_REASON)
            )
        };
        let result = if creates.is_empty() {
            txn.execute_with_variables(
                &mutation,
                &serde_json::json!({
                    "terminal_output": gents_protocol::output::TerminalOutput::NoMessage
                }),
            )
            .await?
        } else {
            txn.execute(&mutation).await?
        };
        anyhow::ensure!(
            result["data"]["update_AgentRequest"]
                .as_array()
                .is_some_and(|rows| rows.len() == 1),
            "A selected message started before this edit. Refresh the queue."
        );
        let mut released = serde_json::to_value(old)?;
        released["_docID"] = serde_json::Value::String(old_id.to_owned());
        crate::workspace::release_terminal_writer_binding(txn, &released).await?;
        crate::trigger_engine::durable::publish_request_outcome(
            txn,
            node_did,
            &old.request_id,
            if creates.is_empty() {
                "interrupted"
            } else {
                "superseded"
            },
            if creates.is_empty() {
                REMOVED_REASON
            } else {
                REPLACED_REASON
            },
            &chrono::Utc::now().to_rfc3339(),
        )
        .await?;
    }
    Ok(receipt)
}

pub fn pending_steer_edit(
    snapshot: &PendingQueueSnapshot,
    expected: Vec<String>,
    request_doc_id: &str,
) -> Result<PendingQueueEdit> {
    anyhow::ensure!(
        snapshot
            .entries
            .iter()
            .map(|entry| &entry.request_doc_id)
            .eq(expected.iter()),
        "The message queue changed. Refresh it and try again."
    );
    let index = snapshot
        .entries
        .iter()
        .position(|entry| entry.request_doc_id == request_doc_id)
        .context("This message is no longer pending.")?;
    let target = &snapshot.entries[index];
    anyhow::ensure!(
        target.steerable,
        "This message cannot be steered or is already steering."
    );
    let selected = &snapshot.entries[..=index];
    anyhow::ensure!(
        selected
            .iter()
            .all(|entry| entry.steerable && entry.edit_group == target.edit_group),
        "This message cannot pass earlier input with different execution settings."
    );
    let reordered = std::iter::once(target).chain(snapshot.entries[..index].iter());
    Ok(PendingQueueEdit {
        expected_request_doc_ids: expected,
        selected_request_doc_ids: selected
            .iter()
            .map(|entry| entry.request_doc_id.clone())
            .collect(),
        messages: reordered
            .map(|entry| PendingMessageEdit {
                request_doc_id: entry.request_doc_id.clone(),
                content: entry.content.clone(),
            })
            .collect(),
    })
}

#[cfg(test)]
pub(super) struct PendingEditActor<'a> {
    pub(super) node_did: &'a str,
    pub(super) session_id: &'a str,
    pub(super) requester_did: &'a str,
}

#[cfg(test)]
pub(super) async fn replace_pending_user_messages(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    actor: PendingEditActor<'_>,
    edit: PendingQueueEdit,
    deliveries: &[QueueDelivery],
) -> Result<PendingQueueReceipt> {
    let PendingEditActor {
        node_did,
        session_id,
        requester_did,
    } = actor;
    let creates = prepare_pending_user_edit_with_deliveries(
        access,
        signer,
        admission,
        node_did,
        session_id,
        requester_did,
        &edit,
        Some(deliveries),
    )
    .await?;
    access
        .transact_write("queue.replace_pending_messages", |txn| {
            let edit = &edit;
            let creates = &creates;
            Box::pin(async move {
                let validated = validate_pending_user_edit_in_txn(
                    txn,
                    node_did,
                    session_id,
                    requester_did,
                    edit,
                    creates,
                )
                .await?
                .map_err(anyhow::Error::msg)?;
                apply_pending_user_edit_in_txn(txn, validated).await
            })
        })
        .await
}
