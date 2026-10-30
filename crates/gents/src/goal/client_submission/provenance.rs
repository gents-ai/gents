use std::collections::BTreeMap;

use anyhow::{Context, Result};
use gents_protocol::goal_control::{GoalControlOutcome, GoalOperation};
use gents_protocol::output::{MessageRole, TranscriptMessage};
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

use super::{SessionGoalControl, SessionGoalControlReceipt};
use crate::config_client::ConfigRead;
use crate::graphql::escape_graphql_string;
use crate::store::identity::verify_did_signature;

const PAGE_SIZE: usize = 32;

#[derive(Deserialize)]
struct ControlRow {
    intent_json: String,
    receipt_json: Option<String>,
}

struct Candidate<'a> {
    request: &'a AgentRequestRow,
    goal_id: &'a str,
    command_id: &'a str,
}

/// The projected requests are lookup hints. Attribution requires their canonical
/// signed input, the runtime's applied receipts, and each Goal's physical root.
/// These facts survive completion and deletion independently of current Goal
/// state. Pages batch observation reads rather than add a round trip per row.
pub async fn load_applied_opening_goal_ids(
    access: &dyn ConfigRead,
    observed: &[&AgentRequestRow],
) -> Result<BTreeMap<String, String>> {
    Ok(load_applied_opening_goal_provenance(access, observed, &[])
        .await?
        .by_request_doc_id)
}

enum Hint<'a> {
    Request(&'a AgentRequestRow),
    Origin(&'a TranscriptMessage),
}

impl Hint<'_> {
    fn doc_id(&self) -> Option<&str> {
        match self {
            Self::Request(row) => row.doc_id.as_deref(),
            Self::Origin(message) => message.request_doc_id.as_deref(),
        }
    }

    fn matches(&self, request: &AgentRequestRow) -> bool {
        match self {
            Self::Request(hint) => {
                request.request_id == hint.request_id
                    && request.node_did == hint.node_did
                    && request.session_id == hint.session_id
                    && request.requester_did == hint.requester_did
                    && request.input == hint.input
                    && request.content == hint.content
            }
            Self::Origin(message) => {
                message.role == MessageRole::User
                    && request.node_did.as_deref() == Some(message.node_did.as_str())
                    && request.session_id.as_deref() == Some(message.session_id.as_str())
                    && request.requester_did == message.requester_did
                    && crate::lifecycle::input_message_owner(request).is_some_and(|(doc, key)| {
                        Some(doc.as_str()) == message.request_doc_id.as_deref()
                            && key == message.message_key
                    })
            }
        }
    }
}

#[derive(Default)]
pub struct OpeningGoalProvenance {
    pub by_request_doc_id: BTreeMap<String, String>,
    pub by_message_key: BTreeMap<String, String>,
}

/// Origins must come from canonical fork resolution, including its denial and
/// metadata checks. A copied message can name the opening request without a
/// local request projection; its original authored key still binds that input.
pub async fn load_applied_opening_goal_provenance(
    access: &dyn ConfigRead,
    observed: &[&AgentRequestRow],
    origins: &[&TranscriptMessage],
) -> Result<OpeningGoalProvenance> {
    let hints = observed
        .iter()
        .copied()
        .filter(|row| {
            row.doc_id.is_some()
                && row
                    .input
                    .as_ref()
                    .and_then(|input| input.goal_assignment.as_ref())
                    .is_some_and(|assignment| assignment.root_request_doc_id.is_none())
        })
        .map(Hint::Request)
        .chain(
            origins
                .iter()
                .copied()
                .filter(|message| {
                    message.request_doc_id.is_some() && message.role == MessageRole::User
                })
                .map(Hint::Origin),
        )
        .collect::<Vec<_>>();
    let mut confirmed = OpeningGoalProvenance::default();
    for page in hints.chunks(PAGE_SIZE) {
        let ids = page
            .iter()
            .filter_map(Hint::doc_id)
            .map(|doc| format!("\"{}\"", escape_graphql_string(doc)))
            .collect::<Vec<_>>()
            .join(", ");
        let data = access
            .execute_read(&format!(
                "{{ AgentRequest(docIDs: [{ids}]) {{ {} }} }}",
                crate::SIGNED_REQUEST_FIELDS,
            ))
            .await?;
        let requests: Vec<AgentRequestRow> = serde_json::from_value(
            data.pointer("/data/AgentRequest")
                .cloned()
                .context("opening provenance omitted request rows")?,
        )?;
        let candidates = page
            .iter()
            .filter_map(|hint| {
                let mut rows = requests
                    .iter()
                    .filter(|row| row.doc_id.as_deref() == hint.doc_id());
                let request = rows.next()?;
                if rows.next().is_some() || !hint.matches(request) {
                    return None;
                }
                let goal_id = super::super::request_assignment::verified_opening_goal_id(request)?;
                let command_id = request
                    .retry_key
                    .as_deref()?
                    .strip_prefix("session-goal:")?;
                Some(Candidate {
                    request,
                    goal_id,
                    command_id,
                })
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            continue;
        }
        let selections = candidates.iter().enumerate().map(|(i, candidate)| {
            let request = candidate.request;
            let node = escape_graphql_string(request.node_did.as_deref().unwrap_or_default());
            let session = escape_graphql_string(request.session_id.as_deref().unwrap_or_default());
            let requester = request.requester_did.as_deref().map(|did| {
                format!("\"{}\"", escape_graphql_string(did))
            }).unwrap_or_else(|| "null".into());
            let goal = escape_graphql_string(candidate.goal_id);
            let command = escape_graphql_string(candidate.command_id);
            let doc = escape_graphql_string(request.doc_id.as_deref().unwrap_or_default());
            format!(r#"
                goal_{i}: Goal(filter: {{node_did: {{_eq: "{node}"}}, session_id: {{_eq: "{session}"}}, goal_id: {{_eq: "{goal}"}}, assignment_root_request_doc_id: {{_eq: "{doc}"}}}}, showDeleted: true, limit: 1) {{assignment_root_request_doc_id}}
                control_{i}: AgentSessionGoalControl(filter: {{command_id: {{_eq: "{command}"}}, node_did: {{_eq: "{node}"}}, session_id: {{_eq: "{session}"}}, requester_did: {{_eq: {requester}}}}}, limit: 2) {{intent_json receipt_json}}
            "#)
        }).collect::<Vec<_>>().join("\n");
        let data = access.execute_read(&format!("{{ {selections} }}")).await?;
        for (i, candidate) in candidates.iter().enumerate() {
            let roots = data["data"][format!("goal_{i}")]
                .as_array()
                .context("opening provenance omitted Goal binding rows")?;
            if roots.len() != 1
                || roots[0]["assignment_root_request_doc_id"].as_str()
                    != candidate.request.doc_id.as_deref()
            {
                continue;
            }
            let controls: Vec<ControlRow> =
                serde_json::from_value(data["data"][format!("control_{i}")].clone())?;
            let [control] = controls.as_slice() else {
                continue;
            };
            if applied(candidate, control)? {
                for hint in page.iter().filter(|hint| {
                    hint.doc_id() == candidate.request.doc_id.as_deref()
                        && hint.matches(candidate.request)
                }) {
                    match hint {
                        Hint::Request(request) => {
                            if let Some(doc) = request.doc_id.as_ref() {
                                confirmed
                                    .by_request_doc_id
                                    .insert(doc.clone(), candidate.goal_id.to_owned());
                            }
                        }
                        Hint::Origin(message) => {
                            confirmed
                                .by_message_key
                                .insert(message.message_key.clone(), candidate.goal_id.to_owned());
                        }
                    }
                }
            }
        }
    }
    Ok(confirmed)
}

pub async fn load_applied_opening_goal_id(
    access: &dyn ConfigRead,
    observed: &AgentRequestRow,
) -> Result<Option<String>> {
    let mut roots = load_applied_opening_goal_ids(access, &[observed]).await?;
    Ok(observed.doc_id.as_ref().and_then(|doc| roots.remove(doc)))
}

fn applied(candidate: &Candidate<'_>, control: &ControlRow) -> Result<bool> {
    let Ok(command) = serde_json::from_str::<SessionGoalControl>(&control.intent_json) else {
        return Ok(false);
    };
    let Some(receipt) = control
        .receipt_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<SessionGoalControlReceipt>(raw).ok())
    else {
        return Ok(false);
    };
    let Some(initial) = command.initial_request.as_deref() else {
        return Ok(false);
    };
    let request = candidate.request;
    if command.validate_shape().is_err()
        || command.command_id != candidate.command_id
        || Some(command.node_did.as_str()) != request.node_did.as_deref()
        || Some(command.session_id.as_str()) != request.session_id.as_deref()
        || Some(command.requester_did.as_str()) != request.requester_did.as_deref()
        || !matches!(command.operation, GoalOperation::Create { .. })
        || initial.request_id != request.request_id
        || initial.node_did != command.node_did
        || initial.session_id != command.session_id
        || initial.requester_did != command.requester_did
        || Some(&initial.input) != request.input.as_ref()
        || request.admission_signature.as_deref()
            != Some(
                bs58::encode(&initial.admission.signature)
                    .into_string()
                    .as_str(),
            )
        || !verify_did_signature(
            &initial.admission.signer_did,
            &initial.signing_payload(),
            &initial.admission.signature,
        )
        .unwrap_or(false)
        || !verify_did_signature(
            &command.requester_did,
            &command.signing_payload()?,
            &command.signature,
        )
        .unwrap_or(false)
        || receipt.validate_for(&command).is_err()
        || !verify_did_signature(
            &command.node_did,
            &receipt.signing_payload()?,
            &receipt.signature,
        )
        .unwrap_or(false)
    {
        return Ok(false);
    }
    Ok(
        matches!(receipt.outcome, GoalControlOutcome::Applied {goal_id: ref goal, request_id: Some(ref id)}
        if goal == candidate.goal_id && id == &request.request_id),
    )
}
