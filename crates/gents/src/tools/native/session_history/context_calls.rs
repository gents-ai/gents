use super::observation::{load_session_request_bindings, RequestBindings};
use super::*;

pub(super) const PAGE_SIZE: usize = 128;
const CONTEXT_CALL_LIMIT: usize = 10;
const ORDER_FIELDS: [&str; 4] = ["queued_at", "call_seq", "call_id", "_docID"];

/// Session hints select candidates, never authorize them. Retained calls without
/// a hint require paged scoped request reads, not a scan through unrelated
/// inference payloads. Each page uses canonical physical request ownership.
/// Query inputs and retained rows stay
/// bounded independently of the number of requests in the session.
pub async fn load_latest_session_context_calls(
    access: &dyn crate::config_client::ConfigRead,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> Result<Vec<Value>> {
    anyhow::ensure!(
        !node_did.trim().is_empty() && !session_id.trim().is_empty(),
        "session accounting requires a node and session"
    );
    let mut selected = indexed_calls(access, node_did, requester_did, session_id).await?;
    let lower_bound = if selected.len() == CONTEXT_CALL_LIMIT {
        selected
            .last()
            .and_then(|row| row["queued_at"].as_str())
            .filter(|time| !time.is_empty())
            .map(|time| format!(r#", queued_at: {{_ge: "{}"}}"#, escape_graphql_string(time)))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let legacy = format!(
        r#"node_did: {{_eq: "{}"}}, call_kind: {{_eq: "inference"}}, _or: [{{session_id: {{_eq: null}}}}, {{session_id: {{_eq: ""}}}}]{lower_bound}"#,
        escape_graphql_string(node_did.trim())
    );
    let data = access
        .execute_read(&format!(
            "{{ InferenceCall(filter: {{{legacy}}}, limit: 1) {{_docID}} }}"
        ))
        .await?;
    if call_rows(&data)?.is_empty() {
        return Ok(selected);
    }
    let mut after = None;
    loop {
        let bindings = load_session_request_bindings(
            access,
            node_did,
            requester_did,
            session_id,
            RequestBindings::Page(after.as_deref()),
        )
        .await?;
        if bindings.is_empty() {
            return Ok(selected);
        }
        let ids = bindings
            .keys()
            .map(|id| format!(r#""{}""#, escape_graphql_string(id)))
            .collect::<Vec<_>>()
            .join(", ");
        let data = access.execute_read(&format!(r#"{{ InferenceCall(filter: {{{legacy}, request_doc_id: {{_in: [{ids}]}}}}, order: [{{queued_at: DESC}}, {{call_seq: DESC}}, {{call_id: DESC}}, {{_docID: DESC}}], limit: {CONTEXT_CALL_LIMIT}) {{ _docID request_doc_id request_id call_id call_seq queued_at context_accounting_json }} }}"#)).await?;
        for row in call_rows(&data)? {
            let physical = row["request_doc_id"]
                .as_str()
                .context("InferenceCall accounting row has no request_doc_id")?;
            let logical = bindings
                .get(physical)
                .context("InferenceCall has no scoped physical request")?;
            anyhow::ensure!(
                Some(logical.as_str()) == row["request_id"].as_str(),
                "InferenceCall request identity does not match its scoped AgentRequest"
            );
            selected.push(row.clone());
        }
        selected.sort_by(|left, right| {
            right["queued_at"]
                .as_str()
                .cmp(&left["queued_at"].as_str())
                .then_with(|| right["call_seq"].as_i64().cmp(&left["call_seq"].as_i64()))
                .then_with(|| right["call_id"].as_str().cmp(&left["call_id"].as_str()))
                .then_with(|| right["_docID"].as_str().cmp(&left["_docID"].as_str()))
        });
        selected.truncate(CONTEXT_CALL_LIMIT);
        if bindings.len() < PAGE_SIZE {
            return Ok(selected);
        }
        after = bindings.last_key_value().map(|(id, _)| id.clone());
    }
}

fn call_rows(data: &Value) -> Result<&[Value]> {
    data.pointer("/data/InferenceCall")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .context("session InferenceCall response omitted its result array")
}

async fn indexed_calls(
    access: &dyn crate::config_client::ConfigRead,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> Result<Vec<Value>> {
    let mut selected = Vec::with_capacity(CONTEXT_CALL_LIMIT);
    let mut cursor = None;
    loop {
        let data = access
            .execute_read(&calls_query(node_did, session_id, cursor.as_ref())?)
            .await?;
        let rows = data
            .pointer("/data/InferenceCall")
            .and_then(Value::as_array)
            .context("session InferenceCall response omitted its result array")?;
        if rows.is_empty() {
            return Ok(selected);
        }
        let ids = rows
            .iter()
            .filter_map(|row| {
                row["request_doc_id"]
                    .as_str()
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        let bindings = load_session_request_bindings(
            access,
            node_did,
            requester_did,
            session_id,
            RequestBindings::Physical(&ids),
        )
        .await?;
        for row in rows {
            let Some(request_id) = row["request_doc_id"]
                .as_str()
                .and_then(|id| bindings.get(id))
            else {
                continue;
            };
            anyhow::ensure!(
                row["request_id"].as_str() == Some(request_id),
                "InferenceCall request identity does not match its scoped AgentRequest"
            );
            selected.push(row.clone());
            if selected.len() == CONTEXT_CALL_LIMIT {
                return Ok(selected);
            }
        }
        if rows.len() < CONTEXT_CALL_LIMIT {
            return Ok(selected);
        }
        cursor = rows.last().cloned();
    }
}

fn calls_query(node_did: &str, session_id: &str, cursor: Option<&Value>) -> Result<String> {
    let cursor_filter = cursor
        .map(descending_cursor_filter)
        .transpose()?
        .map(|filter| format!(", {filter}"))
        .unwrap_or_default();
    Ok(format!(
        r#"{{ InferenceCall(
            filter: {{ _and: [
                {{node_did: {{_eq: "{}"}}}},
                {{call_kind: {{_eq: "inference"}}}},
                {{session_id: {{_eq: "{}"}}}}
                {cursor_filter}
            ] }},
            order: [{{queued_at: DESC}}, {{call_seq: DESC}}, {{call_id: DESC}}, {{_docID: DESC}}],
            limit: {CONTEXT_CALL_LIMIT}
        ) {{ _docID request_doc_id request_id call_id call_seq queued_at context_accounting_json }} }}"#,
        escape_graphql_string(node_did.trim()),
        escape_graphql_string(session_id.trim()),
    ))
}

/// DefraDB sorts null before present values, but `_lt` excludes null when its
/// operand is present. Each descending keyset branch must therefore include
/// null explicitly. Physical identity makes tied and entirely null keys advance.
fn descending_cursor_filter(row: &Value) -> Result<String> {
    let mut equal = Vec::new();
    let mut branches = Vec::new();
    for field in ORDER_FIELDS {
        let value = row.get(field).unwrap_or(&Value::Null);
        let literal = gents_protocol::graphql::graphql_input_literal(value)?;
        if !value.is_null() {
            let lower = if field == "_docID" {
                format!("{{_docID: {{_lt: {literal}}}}}")
            } else {
                format!("{{_or: [{{{field}: {{_lt: {literal}}}}}, {{{field}: {{_eq: null}}}}]}}")
            };
            branches.push(format!(
                "{{_and: [{}]}}",
                equal
                    .iter()
                    .cloned()
                    .chain([lower])
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        equal.push(format!("{{{field}: {{_eq: {literal}}}}}"));
    }
    anyhow::ensure!(
        row["_docID"].as_str().is_some_and(|id| !id.is_empty()),
        "InferenceCall page has no physical cursor identity"
    );
    Ok(format!("{{_or: [{}]}}", branches.join(", ")))
}

#[cfg(test)]
mod tests;
