use std::collections::BTreeMap;

use serde_json::Value;

use crate::eval::runner::progress::SessionContextUsage;

pub(super) fn session_contexts(data: &Value) -> Option<Vec<SessionContextUsage>> {
    let requests = data.get("AgentRequest")?.as_array()?;
    let calls = data.get("InferenceCall")?.as_array()?;
    let compactions = data.get("CompactionEntry")?.as_array()?;
    let reductions = data.get("ProviderContextReduction")?.as_array()?;
    let mut sessions = BTreeMap::new();
    let mut owners = BTreeMap::new();
    for request in requests {
        let Some(scope) = scope(request) else {
            continue;
        };
        let Some(doc_id) = request.get("_docID").and_then(Value::as_str) else {
            continue;
        };
        sessions
            .entry(scope.clone())
            .or_insert_with(|| SessionContextUsage {
                node_did: scope.0.clone(),
                session_id: scope.1.clone(),
                requester_did: scope.2.clone(),
                ..Default::default()
            });
        owners.insert(doc_id, scope);
    }
    let mut calls: Vec<_> = calls
        .iter()
        .filter(|c| c["call_kind"] == "inference")
        .collect();
    calls.sort_by_key(|c| {
        (
            c["queued_at"].as_str().unwrap_or_default(),
            c["call_seq"].as_i64().unwrap_or_default(),
            c["call_id"].as_str().unwrap_or_default(),
        )
    });
    for call in calls {
        let Some(owner) = call["request_doc_id"]
            .as_str()
            .and_then(|id| owners.get(id))
        else {
            continue;
        };
        if call["node_did"].as_str() != Some(owner.0.as_str()) {
            continue;
        }
        let session = sessions.get_mut(owner)?;
        session.last_prompt_tokens = call["prompt_tokens"].as_u64();
        session.peak_prompt_tokens = session.peak_prompt_tokens.max(session.last_prompt_tokens);
        let accounting = call["context_accounting_json"]
            .as_str()
            .and_then(|encoded| {
                serde_json::from_str::<gents_protocol::rendered_request::ContextAccounting>(encoded)
                    .ok()
            });
        session.last_estimated_input_tokens =
            accounting.as_ref().map(|a| a.estimated_input_tokens as u64);
        session.context_window = accounting.as_ref().map(|a| a.context_window as u64);
    }
    for (rows, provider) in [(compactions, false), (reductions, true)] {
        for row in rows {
            if let Some(session) = scope(row).and_then(|key| sessions.get_mut(&key)) {
                if provider {
                    session.provider_reductions += 1;
                } else {
                    session.session_compactions += 1;
                }
            }
        }
    }
    Some(sessions.into_values().collect())
}

fn scope(row: &Value) -> Option<(String, String, Option<String>)> {
    let agent = row.get("node_did")?.as_str()?;
    let session = row.get("session_id")?.as_str()?;
    if agent.is_empty() || session.is_empty() {
        return None;
    }
    Some((
        agent.to_owned(),
        session.to_owned(),
        row.get("requester_did")?.as_str().map(str::to_owned),
    ))
}
