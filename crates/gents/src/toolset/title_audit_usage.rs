use std::collections::BTreeMap;

use defra_node::EmbeddedNode;
use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::row::AgentRequestRow;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};

type Result<T, E = TitleAuditUsageError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum TitleAuditUsageError {
    #[error("inference call row is not an object")]
    CallRowNotObject,
    #[error("decoding audit usage InferenceCall row")]
    DecodeCallRow(#[source] serde_json::Error),
    #[error("audit usage has no {field}")]
    MissingField { field: &'static str },
    #[error("audit usage query omitted {field} rows")]
    RowsOmitted { field: &'static str },
    #[error("decoding audit usage {field} rows")]
    DecodeRows {
        field: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("negative audit prompt usage")]
    NegativePrompt(#[source] std::num::TryFromIntError),
    #[error("negative audit completion usage")]
    NegativeCompletion(#[source] std::num::TryFromIntError),
    #[error("audit usage has only one persisted token component")]
    PartialTokens,
    #[error("audit prompt usage overflow")]
    PromptOverflow,
    #[error("audit completion usage overflow")]
    CompletionOverflow,
    #[error("inference call crossed selected physical request scope")]
    CrossedScope,
    #[error("inference call logical request disagrees with physical owner")]
    LogicalRequestMismatch,
    #[error("one inference document has conflicting call IDs")]
    ConflictingCallIds,
    #[error("one call ID has conflicting persisted rows")]
    ConflictingRows,
    #[error("budget restart requires reported provider usage for call {call_id}")]
    BudgetUsageMissing { call_id: String },
    #[error("budget restart usage overflow")]
    BudgetOverflow,
    #[error("duplicate goal request physical identity")]
    DuplicateGoalRequestPhysicalIdentity,
    #[error("budget restart disagrees with the execution owner's pinned limit")]
    BudgetRestartDisagreesExecutionOwnerS,
    #[error("audit usage parent is missing or ambiguous")]
    AuditUsageParentMissingAmbiguous,
    #[error("audit usage parent crossed its exact normal request scope")]
    AuditUsageParentCrossedExactNormal,
    #[error("duplicate auxiliary request physical identity")]
    DuplicateAuxiliaryRequestPhysicalIdentity,
    #[error("goal usage query omitted AgentRequest rows")]
    GoalUsageQueryOmittedAgentRequestRows,
    #[error("decoding goal usage AgentRequest rows")]
    DecodingGoalUsageAgentRequestRows(#[source] serde_json::Error),
    #[error("goal review has no same-session parent")]
    GoalReviewHasNoSameSession,
    #[error("goal usage query omitted InferenceCall rows")]
    GoalUsageQueryOmittedInferenceCallRows,
    #[error("audit usage query omitted InferenceCall rows")]
    AuditUsageQueryOmittedInferenceCallRows,
    #[error(transparent)]
    AdmissionCause(#[from] crate::request_admission::AdmissionCause),
    #[error(transparent)]
    ConfigClient(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    RequestReceipt(#[from] crate::request_admission::RequestReceiptError),
    #[error(transparent)]
    ProviderUsage(#[from] gents_loop::provider_usage::ProviderUsageError),
}

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

/// Unavailable covers failed authentication or malformed usage; an absent
/// provider usage report is not evidence of zero consumption.
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

fn decode_calls_for_node(values: &[serde_json::Value], node_did: &str) -> Result<Vec<CallRow>> {
    values
        .iter()
        .filter(|row| row.get("node_did").and_then(serde_json::Value::as_str) == Some(node_did))
        .map(|row| {
            let mut row = row.clone();
            row.as_object_mut()
                .ok_or(TitleAuditUsageError::CallRowNotObject)?
                .remove("node_did");
            serde_json::from_value(row).map_err(TitleAuditUsageError::DecodeCallRow)
        })
        .collect()
}

fn nonempty<'a>(value: Option<&'a str>, field: &'static str) -> Result<&'a str> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or(TitleAuditUsageError::MissingField { field })
}

fn required_rows<T>(response: &defra_node::QueryResponse, field: &'static str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let value = response
        .data
        .as_ref()
        .and_then(|data| data.get(field))
        .ok_or(TitleAuditUsageError::RowsOmitted { field })?;
    serde_json::from_value(value.clone())
        .map_err(|source| TitleAuditUsageError::DecodeRows { field, source })
}

fn totals_for_call(row: &CallRow) -> Result<AuditUsageTokens> {
    match (row.prompt_tokens, row.completion_tokens) {
        (None, None) => Ok(AuditUsageTokens::default()),
        (Some(prompt), Some(completion)) => Ok(AuditUsageTokens {
            prompt_tokens: u64::try_from(prompt).map_err(TitleAuditUsageError::NegativePrompt)?,
            completion_tokens: u64::try_from(completion)
                .map_err(TitleAuditUsageError::NegativeCompletion)?,
        }),
        _ => Err(TitleAuditUsageError::PartialTokens),
    }
}

fn add_usage(total: &mut AuditUsageTokens, usage: AuditUsageTokens) -> Result<()> {
    total.prompt_tokens = total
        .prompt_tokens
        .checked_add(usage.prompt_tokens)
        .ok_or(TitleAuditUsageError::PromptOverflow)?;
    total.completion_tokens = total
        .completion_tokens
        .checked_add(usage.completion_tokens)
        .ok_or(TitleAuditUsageError::CompletionOverflow)?;
    Ok(())
}

fn coherent_calls(
    request_ids: &BTreeMap<String, String>,
    calls: Vec<CallRow>,
) -> Result<BTreeMap<String, CallRow>> {
    let mut by_call = BTreeMap::<String, CallRow>::new();
    let mut by_doc = BTreeMap::<String, String>::new();
    for call in calls {
        nonempty(Some(&call.doc_id), "inference call document")?;
        nonempty(Some(&call.call_id), "inference call ID")?;
        nonempty(Some(&call.backend_id), "inference backend")?;
        nonempty(Some(&call.call_state), "inference call state")?;
        let logical = request_ids
            .get(&call.request_doc_id)
            .ok_or(TitleAuditUsageError::CrossedScope)?;
        if call.request_id != *logical {
            return Err(TitleAuditUsageError::LogicalRequestMismatch);
        }
        if let Some(previous) = by_doc.insert(call.doc_id.clone(), call.call_id.clone()) {
            if previous != call.call_id {
                return Err(TitleAuditUsageError::ConflictingCallIds);
            }
        }
        if let Some(previous) = by_call.get(&call.call_id) {
            if previous != &call {
                return Err(TitleAuditUsageError::ConflictingRows);
            }
        } else {
            by_call.insert(call.call_id.clone(), call);
        }
    }

    Ok(by_call)
}

fn project_catalog(
    parent_doc_id: &str,
    parent_id: &str,
    request_ids: &BTreeMap<String, String>,
    calls: Vec<CallRow>,
) -> Result<ParentAuditUsage> {
    let by_call = coherent_calls(request_ids, calls)?;
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

fn project_budget_catalog(
    request_ids: &BTreeMap<String, String>,
    calls: Vec<CallRow>,
) -> Result<u64> {
    coherent_calls(request_ids, calls)?
        .values()
        .try_fold(0_u64, |used, call| {
            let usage_missing = || TitleAuditUsageError::BudgetUsageMissing {
                call_id: call.call_id.clone(),
            };
            if call.prompt_tokens.is_none() || call.completion_tokens.is_none() {
                return Err(usage_missing());
            }
            let usage = totals_for_call(call)?;
            let charged = usage
                .prompt_tokens
                .checked_add(usage.completion_tokens)
                .ok_or(TitleAuditUsageError::BudgetOverflow)?;
            if charged == 0 {
                return Err(usage_missing());
            }
            used.checked_add(charged)
                .ok_or(TitleAuditUsageError::BudgetOverflow)
        })
}

struct ParentUsageCatalog {
    parent_doc_id: String,
    parent_id: String,
    max_total_tokens: Option<i64>,
    request_ids: BTreeMap<String, String>,
    calls: Vec<CallRow>,
}

fn project_goal_catalog(
    request_ids: &BTreeMap<String, String>,
    calls: Vec<CallRow>,
) -> Result<crate::goal::SessionTokenUsage> {
    let calls = coherent_calls(request_ids, calls)?;
    let parts = || {
        calls
            .values()
            .map(|call| (call.prompt_tokens, call.completion_tokens))
    };
    let total = crate::provider_usage::sum_charged_from_persisted_parts(parts())?;
    let (input, output, _) = crate::provider_usage::sum_persisted_usage_columns(
        parts().map(|(prompt, completion)| (prompt, completion, None)),
    );
    Ok(crate::goal::SessionTokenUsage {
        input: input.unwrap_or_default(),
        output: output.unwrap_or_default(),
        total,
    })
}

/// Review charges follow their signed physical parent in the same snapshot as
/// the goal baseline. Title overhead is excluded from this product budget.
pub(crate) async fn load_session_goal_usage(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<crate::goal::SessionTokenUsage> {
    let response = txn.execute(&format!(
        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, purpose: {{ _in: ["normal", "access-review"] }} }}) {{ {} }} }}"#,
        escape_graphql_string(node_did), escape_graphql_string(session_id),
        crate::request_admission::SIGNED_REQUEST_RECEIPT_FIELDS,
    )).await?;
    let requests: Vec<AgentRequestRow> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .ok_or(TitleAuditUsageError::GoalUsageQueryOmittedAgentRequestRows)?
            .clone(),
    )
    .map_err(TitleAuditUsageError::DecodingGoalUsageAgentRequestRows)?;
    let mut by_doc = BTreeMap::new();
    for request in &requests {
        let doc = nonempty(request.doc_id.as_deref(), "goal physical request")?;
        if !(by_doc.insert(doc, request).is_none()) {
            return Err(TitleAuditUsageError::DuplicateGoalRequestPhysicalIdentity);
        }
    }
    let mut request_ids = BTreeMap::new();
    for request in &requests {
        if request.purpose == Some(RequestPurpose::AccessReview) {
            let parent_doc = nonempty(
                request.caused_by_parent_request_doc_id.as_deref(),
                "review parent",
            )?;
            let parent = by_doc
                .get(parent_doc)
                .ok_or(TitleAuditUsageError::GoalReviewHasNoSameSession)?;
            crate::request_admission::verify_historical_auxiliary_receipt(request, parent)?;
        }
        request_ids.insert(
            nonempty(request.doc_id.as_deref(), "goal physical request")?.to_owned(),
            request.request_id.clone(),
        );
    }
    if request_ids.is_empty() {
        return Ok(crate::goal::SessionTokenUsage {
            input: 0,
            output: 0,
            total: 0,
        });
    }
    let docs = request_ids
        .keys()
        .map(|doc| format!("\"{}\"", escape_graphql_string(doc)))
        .collect::<Vec<_>>()
        .join(", ");
    let response = txn.execute(&format!(
        r#"{{ InferenceCall(filter: {{ node_did: {{ _eq: "{}" }}, request_doc_id: {{ _in: [{docs}] }} }}) {{ _docID node_did call_id request_doc_id request_id backend_id call_state ended_at prompt_tokens completion_tokens }} }}"#,
        escape_graphql_string(node_did),
    )).await?;
    let raw_calls = response
        .pointer("/data/InferenceCall")
        .and_then(serde_json::Value::as_array)
        .ok_or(TitleAuditUsageError::GoalUsageQueryOmittedInferenceCallRows)?;
    project_goal_catalog(&request_ids, decode_calls_for_node(raw_calls, node_did)?)
}

/// An auxiliary call enters the parent-inclusive total only through its
/// historically verified signed parent link.
pub async fn load_parent_inclusive_audit_usage(
    node: &EmbeddedNode,
    exact_parent: &AgentRequestRow,
) -> Result<ParentAuditUsage> {
    let catalog = load_parent_catalog(node, exact_parent).await?;
    Ok(project_catalog(
        &catalog.parent_doc_id,
        &catalog.parent_id,
        &catalog.request_ids,
        catalog.calls,
    )?)
}

pub(crate) async fn load_parent_inclusive_budget_usage(
    node: &EmbeddedNode,
    request: &crate::watcher::AgentRequest,
) -> Result<u64> {
    let scope = AgentRequestRow {
        doc_id: Some(request.doc_id.clone()),
        request_id: request.request_id.clone(),
        node_did: Some(request.node_did.clone()),
        requester_did: request.requester_did.clone(),
        agent_id: Some(request.agent_id.clone()),
        session_id: Some(request.session_id.clone()),
        ..Default::default()
    };
    let catalog = load_parent_catalog(node, &scope).await?;
    if catalog.max_total_tokens != request.max_total_tokens {
        return Err(TitleAuditUsageError::BudgetRestartDisagreesExecutionOwnerS);
    }
    Ok(project_budget_catalog(&catalog.request_ids, catalog.calls)?)
}

async fn load_parent_catalog(
    node: &EmbeddedNode,
    exact_parent: &AgentRequestRow,
) -> Result<ParentUsageCatalog> {
    let parent_doc_id = nonempty(exact_parent.doc_id.as_deref(), "parent physical request")?;
    let parent_id = nonempty(Some(&exact_parent.request_id), "parent logical request")?;
    let agent = nonempty(exact_parent.node_did.as_deref(), "parent agent")?;
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
    if parents.len() != 1 {
        return Err(TitleAuditUsageError::AuditUsageParentMissingAmbiguous);
    }
    let parent = &parents[0];
    if !(parent.doc_id.as_deref() == Some(parent_doc_id)
        && parent.request_id == parent_id
        && parent.node_did.as_deref() == Some(agent)
        && parent.session_id.as_deref() == Some(session)
        && parent.requester_did == exact_parent.requester_did
        && parent.agent_id == exact_parent.agent_id
        && parent.purpose == Some(RequestPurpose::Normal))
    {
        return Err(TitleAuditUsageError::AuditUsageParentCrossedExactNormal);
    }
    crate::request_admission::verify_request_receipt_signature(parent)?;

    let response = graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_request_doc_id: {{ _eq: "{parent_doc}" }}, purpose: {{ _in: ["title-audit", "access-review"] }} }}) {{ {} }} }}"#,
            crate::request_admission::SIGNED_REQUEST_FIELDS
        ),
        "load auxiliary audit usage receipts",
    )
    .await?;
    let auxiliaries: Vec<AgentRequestRow> = required_rows(&response, "AgentRequest")?;
    let mut request_ids = BTreeMap::new();
    request_ids.insert(parent_doc_id.to_owned(), parent_id.to_owned());
    for request in &auxiliaries {
        crate::request_admission::verify_historical_auxiliary_receipt(request, parent)?;
        let request_doc = nonempty(request.doc_id.as_deref(), "auxiliary physical request")?;
        if !(request_ids
            .insert(request_doc.to_owned(), request.request_id.clone())
            .is_none())
        {
            return Err(TitleAuditUsageError::DuplicateAuxiliaryRequestPhysicalIdentity);
        }
    }

    let docs = request_ids
        .keys()
        .map(|doc| format!("\"{}\"", escape_graphql_string(doc)))
        .collect::<Vec<_>>()
        .join(", ");
    let response = graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ InferenceCall(filter: {{ request_doc_id: {{ _in: [{docs}] }} }}) {{ _docID node_did call_id request_doc_id request_id backend_id call_state ended_at prompt_tokens completion_tokens }} }}"#
        ),
        "load exact parent and auxiliary inference usage",
    )
    .await?;
    let raw_calls = response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(serde_json::Value::as_array)
        .ok_or(TitleAuditUsageError::AuditUsageQueryOmittedInferenceCallRows)?;
    let calls = decode_calls_for_node(raw_calls, agent)?;
    Ok(ParentUsageCatalog {
        parent_doc_id: parent_doc_id.to_owned(),
        parent_id: parent_id.to_owned(),
        max_total_tokens: parent.max_total_tokens,
        request_ids,
        calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lean_vocab_test::{LeanTitleUsageCase, LeanTitleUsageResult};

    #[test]
    fn request_index_rows_preserve_exact_node_usage_and_ignore_malformed_twins() {
        let request_ids =
            BTreeMap::from([("request-parent".to_owned(), "logical-parent".to_owned())]);
        let accepted = serde_json::json!({
            "_docID": "call-1",
            "node_did": "agent",
            "call_id": "call-id",
            "request_doc_id": "request-parent",
            "request_id": "logical-parent",
            "backend_id": "backend",
            "call_state": "completed",
            "ended_at": "2026-09-09T22:30:00Z",
            "prompt_tokens": 11,
            "completion_tokens": 7
        });
        let mut rows = vec![accepted.clone()];
        let mut foreign = accepted.clone();
        foreign["node_did"] = serde_json::json!("other-agent");
        foreign["call_id"] = serde_json::json!(false);
        rows.push(foreign);

        let calls = decode_calls_for_node(&rows, "agent").unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].call_id, "call-id");
        let actual =
            project_catalog("request-parent", "logical-parent", &request_ids, calls).unwrap();
        let baseline = project_catalog(
            "request-parent",
            "logical-parent",
            &request_ids,
            decode_calls_for_node(&[accepted], "agent").unwrap(),
        )
        .unwrap();
        assert_eq!(actual, baseline);
    }

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
            if gents_protocol::request_admission::RequestPurpose::from(fact.purpose).is_auxiliary()
            {
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
                assert!(
                    matches!(error, TitleAuditUsageError::ConflictingRows),
                    "{} rejected for the wrong reason: {error:?}",
                    case.name,
                );
            }
            _ => panic!("{} is outside the physical catalog binding", case.name),
        }
    }

    fn assert_goal_usage_cases() {
        use crate::lean_vocab_test::LeanUsageBudgetResult;

        for name in [
            "empty_budget_has_no_spend",
            "normal_public_usage",
            "title_audit_only",
            "normal_plus_title_parent_inclusive",
            "access_review_usage_is_parent_inclusive_audit_only",
            "normal_review_and_title_have_separate_goal_charges",
            "duplicate_review_call_has_one_goal_charge",
            "access_review_late_usage_restores_budget_observation",
            "failed_access_review_usage_still_reduces_budget",
        ] {
            let case = crate::lean_vocab_test::lean_title_usage_cases()
                .iter()
                .find(|case| case.name == name)
                .unwrap();
            for after in [false, true] {
                let (mut ids, mut calls) = native_catalog(case, after);
                let titles: Vec<_> = case
                    .signed_purposes
                    .iter()
                    .filter(|fact| RequestPurpose::from(fact.purpose) == RequestPurpose::TitleAudit)
                    .map(|fact| format!("request-{}", fact.physical))
                    .collect();
                ids.retain(|doc, _| !titles.contains(doc));
                calls.retain(|call| ids.contains_key(&call.request_doc_id));
                let expected = if after {
                    &case.goal_after
                } else {
                    &case.goal_before
                };
                let LeanUsageBudgetResult::Ok { used } = expected else {
                    panic!("unexpected model goal error for {name}: {expected:?}")
                };
                assert_eq!(
                    project_goal_catalog(&ids, calls).unwrap().total,
                    *used,
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn generated_title_usage_cases_bind_native_catalog_and_late_usage() {
        let names = [
            "normal_public_usage",
            "title_audit_only",
            "access_review_usage_is_parent_inclusive_audit_only",
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

    #[test]
    fn generated_budget_recovery_cases_bind_signed_catalog_usage() {
        use crate::lean_vocab_test::LeanUsageBudgetResult;

        assert_goal_usage_cases();

        let names = [
            "empty_budget_has_no_spend",
            "normal_public_usage",
            "normal_plus_title_parent_inclusive",
            "normal_missing_usage_blocks_budget_restart",
            "access_review_usage_is_parent_inclusive_audit_only",
            "access_review_missing_usage_blocks_budget_restart",
            "access_review_zero_usage_blocks_budget_restart",
            "access_review_late_usage_restores_budget_observation",
            "failed_access_review_usage_still_reduces_budget",
            "title_late_usage_after_parent_terminal",
            "exact_duplicate_call_counts_once",
            "conflicting_call_id_fails_closed",
        ];
        let cases = crate::lean_vocab_test::lean_title_usage_cases();
        for name in names {
            let case = cases.iter().find(|case| case.name == name).unwrap();
            for after in [false, true] {
                let (request_ids, calls) = native_catalog(case, after);
                let result = project_budget_catalog(&request_ids, calls);
                let expected = if after {
                    &case.budget_after
                } else {
                    &case.budget_before
                };
                match expected {
                    LeanUsageBudgetResult::Ok { used } => {
                        assert_eq!(result.unwrap(), *used, "{name}");
                    }
                    LeanUsageBudgetResult::Error { error } => {
                        let failure = result.unwrap_err();
                        if error == "unreported_usage" {
                            assert!(
                                failure
                                    .to_string()
                                    .contains("requires reported provider usage"),
                                "{name}: {failure}"
                            );
                        } else {
                            assert_eq!(error, "conflicting_call_id");
                            assert_eq!(
                                failure.to_string(),
                                "one call ID has conflicting persisted rows",
                                "{name}"
                            );
                        }
                    }
                }
            }
        }
    }
}
