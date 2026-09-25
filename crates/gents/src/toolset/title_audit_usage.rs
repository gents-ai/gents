//! Explicit parent-inclusive inference usage over signed title provenance.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::row::AgentRequestRow;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditUsageTokens {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentAuditUsage {
    pub parent_request_doc_id: String,
    pub parent_request_id: String,
    pub normal_public: AuditUsageTokens,
    pub parent_inclusive_audit: AuditUsageTokens,
}

/// Failed authentication or malformed usage makes this projection unavailable.
/// Totals contain observed usage only; an absent provider usage report is not
/// evidence that the call consumed no tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ParentAuditUsageObservation {
    Available {
        usage: ParentAuditUsage,
    },
    Unavailable {
        parent_request_doc_id: Option<String>,
        parent_request_id: String,
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    call_id: String,
    request_doc_id: String,
    request_id: String,
    backend_id: String,
    call_state: String,
    ended_at: Option<String>,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
}

fn nonempty<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str> {
    value
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("audit usage has no {field}"))
}

fn required_rows<T: DeserializeOwned>(
    response: &defra_node::QueryResponse,
    field: &str,
) -> Result<Vec<T>> {
    let value = response
        .data
        .as_ref()
        .and_then(|data| data.get(field))
        .with_context(|| format!("audit usage query omitted {field} rows"))?;
    serde_json::from_value(value.clone())
        .with_context(|| format!("decoding audit usage {field} rows"))
}

fn totals_for_call(row: &CallRow) -> Result<AuditUsageTokens> {
    match (row.prompt_tokens, row.completion_tokens) {
        (None, None) => Ok(AuditUsageTokens::default()),
        (Some(prompt), Some(completion)) => Ok(AuditUsageTokens {
            prompt_tokens: u64::try_from(prompt).context("negative audit prompt usage")?,
            completion_tokens: u64::try_from(completion)
                .context("negative audit completion usage")?,
        }),
        _ => anyhow::bail!("audit usage has only one persisted token component"),
    }
}

fn add_usage(total: &mut AuditUsageTokens, usage: AuditUsageTokens) -> Result<()> {
    total.prompt_tokens = total
        .prompt_tokens
        .checked_add(usage.prompt_tokens)
        .context("audit prompt usage overflow")?;
    total.completion_tokens = total
        .completion_tokens
        .checked_add(usage.completion_tokens)
        .context("audit completion usage overflow")?;
    Ok(())
}

fn project_catalog(
    parent_doc_id: &str,
    parent_id: &str,
    request_ids: &BTreeMap<String, String>,
    calls: Vec<CallRow>,
) -> Result<ParentAuditUsage> {
    let mut by_call = BTreeMap::<String, CallRow>::new();
    let mut by_doc = BTreeMap::<String, String>::new();
    for call in calls {
        nonempty(Some(&call.doc_id), "inference call document")?;
        nonempty(Some(&call.call_id), "inference call ID")?;
        nonempty(Some(&call.backend_id), "inference backend")?;
        nonempty(Some(&call.call_state), "inference call state")?;
        let logical = request_ids
            .get(&call.request_doc_id)
            .context("inference call crossed selected physical request scope")?;
        anyhow::ensure!(
            call.request_id == *logical,
            "inference call logical request disagrees with physical owner"
        );
        if let Some(previous) = by_doc.insert(call.doc_id.clone(), call.call_id.clone()) {
            anyhow::ensure!(
                previous == call.call_id,
                "one inference document has conflicting call IDs"
            );
        }
        if let Some(previous) = by_call.get(&call.call_id) {
            anyhow::ensure!(
                previous == &call,
                "one call ID has conflicting persisted rows"
            );
        } else {
            by_call.insert(call.call_id.clone(), call);
        }
    }

    let mut normal_public = AuditUsageTokens::default();
    let mut parent_inclusive_audit = AuditUsageTokens::default();
    for call in by_call.values() {
        let usage = totals_for_call(call)?;
        if call.request_doc_id == parent_doc_id {
            add_usage(&mut normal_public, usage)?;
        }
        add_usage(&mut parent_inclusive_audit, usage)?;
    }
    Ok(ParentAuditUsage {
        parent_request_doc_id: parent_doc_id.to_owned(),
        parent_request_id: parent_id.to_owned(),
        normal_public,
        parent_inclusive_audit,
    })
}

/// The ordinary total remains parent-physical only. A title call enters the
/// second total only through its historically verified signed parent link.
pub async fn load_parent_inclusive_audit_usage(
    node: &EmbeddedNode,
    exact_parent: &AgentRequestRow,
) -> Result<ParentAuditUsage> {
    let parent_doc_id = nonempty(exact_parent.doc_id.as_deref(), "parent physical request")?;
    let parent_id = nonempty(Some(&exact_parent.request_id), "parent logical request")?;
    let agent = nonempty(exact_parent.agent_did.as_deref(), "parent agent")?;
    let session = nonempty(exact_parent.session_id.as_deref(), "parent session")?;
    let parent_doc = escape_graphql_string(parent_doc_id);
    let response = graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{parent_doc}" }} }}, limit: 2) {{ {} }} }}"#,
            crate::request_admission::SIGNED_REQUEST_FIELDS
        ),
        "load audit usage parent receipt",
    )
    .await?;
    let parents: Vec<AgentRequestRow> = required_rows(&response, "AgentRequest")?;
    anyhow::ensure!(
        parents.len() == 1,
        "audit usage parent is missing or ambiguous"
    );
    let parent = &parents[0];
    anyhow::ensure!(
        parent.doc_id.as_deref() == Some(parent_doc_id)
            && parent.request_id == parent_id
            && parent.agent_did.as_deref() == Some(agent)
            && parent.session_id.as_deref() == Some(session)
            && parent.requester_did == exact_parent.requester_did
            && parent.behavior_id == exact_parent.behavior_id
            && parent.purpose == Some(RequestPurpose::Normal),
        "audit usage parent crossed its exact normal request scope"
    );
    crate::request_admission::verify_request_receipt_signature(parent)?;

    let response = graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_request_doc_id: {{ _eq: "{parent_doc}" }}, purpose: {{ _eq: "title-audit" }} }}) {{ {} }} }}"#,
            crate::request_admission::SIGNED_REQUEST_FIELDS
        ),
        "load title audit usage receipts",
    )
    .await?;
    let titles: Vec<AgentRequestRow> = required_rows(&response, "AgentRequest")?;
    let mut request_ids = BTreeMap::new();
    request_ids.insert(parent_doc_id.to_owned(), parent_id.to_owned());
    for title in &titles {
        crate::request_admission::verify_historical_title_receipt(title, parent)?;
        let title_doc = nonempty(title.doc_id.as_deref(), "title physical request")?;
        anyhow::ensure!(
            request_ids
                .insert(title_doc.to_owned(), title.request_id.clone())
                .is_none(),
            "duplicate title request physical identity"
        );
    }

    let docs = request_ids
        .keys()
        .map(|doc| format!("\"{}\"", escape_graphql_string(doc)))
        .collect::<Vec<_>>()
        .join(", ");
    let agent_filter = escape_graphql_string(agent);
    let response = graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ InferenceCall(filter: {{ agent_did: {{ _eq: "{agent_filter}" }}, request_doc_id: {{ _in: [{docs}] }} }}) {{ _docID call_id request_doc_id request_id backend_id call_state ended_at prompt_tokens completion_tokens }} }}"#
        ),
        "load exact parent and title inference usage",
    )
    .await?;
    let calls: Vec<CallRow> = required_rows(&response, "InferenceCall")?;
    project_catalog(parent_doc_id, parent_id, &request_ids, calls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lean_vocab_test::{LeanRequestPurpose, LeanTitleUsageCase, LeanTitleUsageResult};

    fn native_catalog(
        case: &LeanTitleUsageCase,
        after_action: bool,
    ) -> (BTreeMap<String, String>, Vec<CallRow>) {
        let mut request_ids = BTreeMap::new();
        request_ids.insert(
            format!("request-{}", case.parent.physical),
            case.parent.logical.to_string(),
        );
        for fact in &case.signed_purposes {
            if fact.purpose == LeanRequestPurpose::TitleAudit {
                request_ids.insert(
                    format!("request-{}", fact.physical),
                    case.title_bindings
                        .iter()
                        .find(|binding| binding.physical == fact.physical)
                        .map_or_else(String::new, |binding| binding.binding.logical.to_string()),
                );
            }
        }
        let calls = case
            .rows
            .iter()
            .map(|row| {
                let usage = if after_action {
                    case.usage_action
                        .as_ref()
                        .filter(|action| action.call_id == row.call_id)
                        .map(|action| &action.usage)
                        .or(row.usage.as_ref())
                } else {
                    row.usage.as_ref()
                };
                CallRow {
                    doc_id: format!("call-{}", row.key),
                    call_id: row.call_id.to_string(),
                    request_doc_id: format!("request-{}", row.request_physical),
                    request_id: row.request_logical.to_string(),
                    backend_id: row.backend.clone(),
                    call_state: row.state.clone(),
                    ended_at: row.terminal_stamp.map(|stamp| stamp.to_string()),
                    prompt_tokens: usage.map(|usage| usage.prompt_tokens as i64),
                    completion_tokens: usage.map(|usage| usage.completion_tokens as i64),
                }
            })
            .collect();
        (request_ids, calls)
    }

    fn assert_totals(case: &LeanTitleUsageCase, after_action: bool) {
        let (request_ids, calls) = native_catalog(case, after_action);
        let actual = project_catalog(
            &format!("request-{}", case.parent.physical),
            &case.parent.logical.to_string(),
            &request_ids,
            calls,
        );
        let expected = if after_action {
            &case.expected_after
        } else {
            &case.expected_before
        };
        match expected {
            LeanTitleUsageResult::Ok {
                normal_public,
                parent_inclusive_audit,
            } => {
                let actual = actual.expect(&case.name);
                assert_eq!(
                    actual.normal_public.prompt_tokens,
                    normal_public.prompt_tokens
                );
                assert_eq!(
                    actual.normal_public.completion_tokens,
                    normal_public.completion_tokens
                );
                assert_eq!(
                    actual.parent_inclusive_audit.prompt_tokens,
                    parent_inclusive_audit.prompt_tokens
                );
                assert_eq!(
                    actual.parent_inclusive_audit.completion_tokens,
                    parent_inclusive_audit.completion_tokens
                );
            }
            LeanTitleUsageResult::Error { error } if error == "conflicting_call_id" => {
                let error = actual.expect_err(&case.name);
                assert_eq!(
                    error.root_cause().to_string(),
                    "one call ID has conflicting persisted rows",
                    "{} rejected for the wrong reason",
                    case.name,
                );
            }
            _ => panic!("{} is outside the physical catalog binding", case.name),
        }
    }

    #[test]
    fn generated_title_usage_cases_bind_native_catalog_and_late_usage() {
        let names = [
            "normal_public_usage",
            "title_audit_only",
            "normal_plus_title_parent_inclusive",
            "title_late_usage_after_parent_terminal",
            "exact_duplicate_call_counts_once",
            "conflicting_call_id_fails_closed",
        ];
        let cases = crate::lean_vocab_test::lean_title_usage_cases();
        for name in names {
            let case = cases
                .iter()
                .find(|case| case.name == name)
                .unwrap_or_else(|| panic!("missing model usage case {name}"));
            assert_totals(case, false);
            assert_totals(case, true);
        }
    }
}
