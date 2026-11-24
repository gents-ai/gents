use std::collections::VecDeque;

use super::*;
use crate::call::lifecycle::completion_delivery::owes_completion_delivery;
use gents_protocol::background_completion::background_completion_notification_message_key;
use gents_protocol::tool_call::ToolCallState;
use gents_store::config_client::ConfigAccess;

/// Lean `LineageReturn.Node`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineageNode {
    pub(crate) terminal: bool,
    pub(crate) owed: bool,
    pub(crate) rank: u64,
}

/// Lean `LineageReturn.quiet`.
pub(crate) fn quiet(terminal: bool, owed: bool) -> bool {
    terminal && !owed
}

/// Lean `LineageReturn.decide`: the rank of the request that answers, or
/// none while the lineage is not quiet.
pub(crate) fn answer_rank(nodes: &[LineageNode]) -> Option<u64> {
    nodes
        .iter()
        .all(|node| quiet(node.terminal, node.owed))
        .then(|| nodes.iter().map(|node| node.rank).fold(0, u64::max))
}

struct Observed {
    doc_id: String,
    wake: bool,
    created_at: String,
    request_id: String,
    terminal: bool,
    owed: bool,
}

#[derive(Deserialize)]
struct CallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

impl CallRow {
    fn owes_result(&self) -> bool {
        match self
            .lifecycle_state
            .as_deref()
            .and_then(ToolCallState::from_persisted)
        {
            Some(state) => {
                !state.is_terminal() || owes_completion_delivery(state, self.status.as_deref())
            }
            None => true,
        }
    }
}

/// The request whose terminal answers a session-message call that caused
/// `caused`, or none while its lineage is not quiet (Lean
/// `LineageReturn.decide`). The caused request answers as itself, so its
/// folded head is still resolved by the terminal reader. Each request's
/// terminal is read before its calls, and a call's wake is followed only once
/// the call no longer owes its result.
pub(crate) async fn answering_request(
    access: &ConfigAccess,
    caused: &AgentRequestRow,
) -> Result<Option<String>> {
    let caused_doc_id = caused
        .doc_id
        .clone()
        .context("caused request lacks physical identity")?;
    let mut pending = VecDeque::from([(caused_doc_id.clone(), false)]);
    let mut seen = rapidhash::RapidHashSet::default();
    let mut observed = Vec::new();
    while let Some((doc_id, wake)) = pending.pop_front() {
        if !seen.insert(doc_id.clone()) {
            continue;
        }
        let Some(request) = lineage_request(access, &doc_id).await? else {
            if wake {
                tracing::warn!(wake_doc_id = %doc_id, "lineage wake is not visible to the caller");
                continue;
            }
            return Ok(None);
        };
        let state = request.lifecycle_state;
        if state == Some(RequestLifecycleState::Superseded) {
            let folded = request.failure_reason.as_deref()
                == Some(gents_session::request::lifecycle::FOLDED_REASON);
            if let (true, Some(next)) =
                (wake || folded, request.superseded_by_request_doc_id.clone())
            {
                pending.push_back((next, wake));
                continue;
            }
        }
        let terminal = state.is_some_and(RequestLifecycleState::is_terminal);
        let mut owed = false;
        if terminal {
            for call in session_message_calls(access, &doc_id).await? {
                if call.owes_result() {
                    owed = true;
                } else if let Some(next) = delivered_wake(access, &request, &call.doc_id).await? {
                    pending.push_back((next, true));
                }
            }
        }
        observed.push(Observed {
            doc_id,
            wake,
            created_at: request.created_at.clone().unwrap_or_default(),
            request_id: request.request_id.clone(),
            terminal,
            owed,
        });
    }
    observed.sort_by(|a, b| {
        (a.wake, &a.created_at, &a.request_id).cmp(&(b.wake, &b.created_at, &b.request_id))
    });
    let nodes: Vec<LineageNode> = observed
        .iter()
        .zip(0u64..)
        .map(|(observed, rank)| LineageNode {
            terminal: observed.terminal,
            owed: observed.owed,
            rank,
        })
        .collect();
    Ok(answer_rank(&nodes).map(|rank| {
        let answer = &observed[rank as usize];
        if answer.wake {
            answer.doc_id.clone()
        } else {
            caused_doc_id
        }
    }))
}

async fn lineage_request(access: &ConfigAccess, doc_id: &str) -> Result<Option<AgentRequestRow>> {
    use gents_protocol::document_query::{
        fields::agent_request as fields, Filter, Query, Selection,
    };
    let query = Query::select(
        Selection::doc_id()
            .field(fields::REQUEST_ID)
            .field(fields::NODE_DID)
            .field(fields::REQUESTER_DID)
            .field(fields::SESSION_ID)
            .field(fields::LIFECYCLE_STATE)
            .field(fields::FAILURE_REASON)
            .field(fields::SUPERSEDED_BY_REQUEST_DOC_ID)
            .field(fields::CREATED_AT),
    )
    .filter(Filter::eq(fields::DOC_ID, doc_id))
    .limit(1)
    .render();
    let response = access.read(&query).await?;
    response
        .pointer("/data/AgentRequest/0")
        .map(|row| serde_json::from_value(row.clone()).context("decode lineage request"))
        .transpose()
}

async fn session_message_calls(
    access: &ConfigAccess,
    request_doc_id: &str,
) -> Result<Vec<CallRow>> {
    use gents_protocol::document_query::{
        fields::agent_tool_call as fields, Filter, Query, Selection,
    };
    let query = Query::select(
        Selection::doc_id()
            .field(fields::LIFECYCLE_STATE)
            .field(fields::STATUS),
    )
    .filter(
        Filter::eq(fields::REQUEST_DOC_ID, request_doc_id)
            .and(Filter::eq(
                fields::AWAIT_MODE,
                gents_protocol::tool_call::AwaitMode::Background.as_str(),
            ))
            .and(Filter::is_null(fields::SPAWNED_BY_TOOL_CALL_DOC_ID))
            .and(Filter::any_of(
                fields::TOOL_NAME,
                [
                    gents_protocol::tool_names::AGENT_NEW_TOOL_NAME,
                    gents_protocol::tool_names::AGENT_MESSAGE_TOOL_NAME,
                ],
            )),
    )
    .render();
    let response = access.read(&query).await?;
    Ok(serde_json::from_value(
        response["data"]["AgentToolCall"].clone(),
    )?)
}

async fn delivered_wake(
    access: &ConfigAccess,
    caller: &AgentRequestRow,
    call_doc_id: &str,
) -> Result<Option<String>> {
    use gents_protocol::document_query::{
        fields::agent_message as fields, session_scope, Filter, Query, Selection,
    };
    let (Some(node_did), Some(session_id)) =
        (caller.node_did.as_deref(), caller.session_id.as_deref())
    else {
        return Ok(None);
    };
    let query = Query::select(Selection::doc_id().field(fields::REQUEST_DOC_ID))
        .filter(session_scope(
            node_did,
            session_id,
            caller.requester_did.as_deref(),
        ))
        .filter(Filter::eq(
            fields::MESSAGE_KEY,
            background_completion_notification_message_key(call_doc_id, "tool"),
        ))
        .limit(1)
        .render();
    let response = access.read(&query).await?;
    Ok(response
        .pointer("/data/AgentMessage/0/request_doc_id")
        .and_then(serde_json::Value::as_str)
        .filter(|doc_id| !doc_id.is_empty())
        .map(str::to_owned))
}

#[cfg(test)]
mod conformance {
    use super::{answer_rank, quiet, LineageNode};

    #[test]
    fn quiet_matches_lean() {
        gents_lean_vocab::families::lineage_return_quiet::replay(|input| {
            quiet(input.terminal, input.owed)
        });
    }

    #[test]
    fn settlement_answer_matches_lean() {
        gents_lean_vocab::families::lineage_return_settle::replay(|input| {
            let nodes: Vec<LineageNode> = input
                .nodes
                .iter()
                .map(|node| LineageNode {
                    terminal: node.terminal,
                    owed: node.owed,
                    rank: node.rank,
                })
                .collect();
            answer_rank(&nodes)
        });
    }
}
