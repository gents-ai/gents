use std::collections::BTreeMap;

use anyhow::{Context, Result};
use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::row::AgentRequestRow;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use gents_store::config_client::ConfigRead;
use gents_store::graphql::escape_graphql_string;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditUsageTokens {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub incomplete_calls: u64,
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
    #[serde(flatten)]
    usage: gents_protocol::usage::TokenUsageColumns,
}

fn decode_calls_for_node(values: &[serde_json::Value], node_did: &str) -> Result<Vec<CallRow>> {
    values
        .iter()
        .filter(|row| row.get("node_did").and_then(serde_json::Value::as_str) == Some(node_did))
        .map(|row| {
            let mut row = row.clone();
            row.as_object_mut()
                .context("inference call row is not an object")?
                .remove("node_did");
            serde_json::from_value(row).context("decoding audit usage InferenceCall row")
        })
        .collect()
}

fn nonempty<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str> {
    value
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("audit usage has no {field}"))
}

fn required_rows<T>(response: &serde_json::Value, field: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let value = response
        .get("data")
        .and_then(|data| data.get(field))
        .with_context(|| format!("audit usage query omitted {field} rows"))?;
    serde_json::from_value(value.clone())
        .with_context(|| format!("decoding audit usage {field} rows"))
}

fn observed_usage(row: &CallRow) -> Result<gents_protocol::usage::TokenUsage> {
    Ok(row.usage.observation()?)
}

fn totals_for_call(row: &CallRow) -> Result<AuditUsageTokens> {
    let usage =
        gents_protocol::usage::TokenUsageSummary::from_observations([observed_usage(row)?])?;
    Ok(AuditUsageTokens {
        prompt_tokens: usage.input.known,
        completion_tokens: usage.output.known,
        incomplete_calls: usage.incomplete_calls,
    })
}

fn add_usage(total: &mut AuditUsageTokens, usage: AuditUsageTokens) -> Result<()> {
    total.incomplete_calls = total
        .incomplete_calls
        .checked_add(usage.incomplete_calls)
        .context("audit incomplete-call count overflow")?;
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
            let charged = observed_usage(call)?
                .charged_tokens()?
                .filter(|charged| *charged > 0)
                .with_context(|| {
                    format!(
                        "budget restart requires reported provider usage for call {}",
                        call.call_id
                    )
                })?;
            used.checked_add(charged)
                .context("budget restart usage overflow")
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
) -> Result<gents_protocol::session_token_usage::SessionTokenUsage> {
    let calls = coherent_calls(request_ids, calls)?;
    let parts = || {
        calls
            .values()
            .map(|call| (call.usage.prompt_tokens, call.usage.completion_tokens))
    };
    let total = gents_loop::provider_usage::sum_charged_from_persisted_parts(parts())?;
    let (input, output, _) = gents_loop::provider_usage::sum_persisted_usage_columns(
        parts().map(|(prompt, completion)| (prompt, completion, None)),
    );
    Ok(gents_protocol::session_token_usage::SessionTokenUsage {
        input: input.unwrap_or_default(),
        output: output.unwrap_or_default(),
        total,
    })
}

/// Review charges follow their signed physical parent in the same snapshot as
/// the goal baseline. Title overhead is excluded from this product budget.
pub async fn load_session_goal_usage(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<gents_protocol::session_token_usage::SessionTokenUsage> {
    let rendered = {
        use gents_protocol::document_query::{
            fields::agent_request as fields, Filter, Query, Selection,
        };
        Query::select(Selection::owned(
            gents_protocol::request_admission::SIGNED_REQUEST_RECEIPT_FIELDS,
        ))
        .filter(Filter::eq(fields::NODE_DID, node_did))
        .filter(Filter::eq(fields::SESSION_ID, session_id))
        .filter(Filter::any_of(fields::PURPOSE, ["normal", "access-review"]))
        .render()
    };
    let response = txn.run(&rendered).await?;
    let requests: Vec<AgentRequestRow> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .context("goal usage query omitted AgentRequest rows")?
            .clone(),
    )
    .context("decoding goal usage AgentRequest rows")?;
    let mut by_doc = BTreeMap::new();
    for request in &requests {
        let doc = nonempty(request.doc_id.as_deref(), "goal physical request")?;
        anyhow::ensure!(
            by_doc.insert(doc, request).is_none(),
            "duplicate goal request physical identity"
        );
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
                .context("goal review has no same-session parent")?;
            gents_store::identity::request_receipt::verify_historical_auxiliary_receipt(
                request, parent,
            )?;
        }
        request_ids.insert(
            nonempty(request.doc_id.as_deref(), "goal physical request")?.to_owned(),
            request.request_id.clone(),
        );
    }
    if request_ids.is_empty() {
        return Ok(gents_protocol::session_token_usage::SessionTokenUsage {
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
        r#"{{ InferenceCall(filter: {{ node_did: {{ _eq: "{}" }}, request_doc_id: {{ _in: [{docs}] }} }}) {{ _docID node_did call_id request_doc_id request_id backend_id call_state ended_at prompt_tokens completion_tokens cached_input_tokens cache_creation_input_tokens reasoning_output_tokens provider_total_tokens usage_completeness }} }}"#,
        escape_graphql_string(node_did),
    )).await?;
    let raw_calls = response
        .pointer("/data/InferenceCall")
        .and_then(serde_json::Value::as_array)
        .context("goal usage query omitted InferenceCall rows")?;
    project_goal_catalog(&request_ids, decode_calls_for_node(raw_calls, node_did)?)
}

/// The named core read of a session's charged token total (Lean
/// `Hooks.Read.core`), the baseline a Goal's usage is measured from. Arguments
/// `{node_did, session_id}`; one row `{total}`.
pub const SESSION_TOKEN_USAGE: &str = "session.token_usage";

struct SessionTokenUsageRead;

#[async_trait::async_trait]
impl gents_session::request::hooks::registry::CoreRead for SessionTokenUsageRead {
    fn name(&self) -> &'static str {
        SESSION_TOKEN_USAGE
    }

    async fn read(
        &self,
        txn: &gents_store::config_client::ConfigApplyTxn<'_>,
        args: &serde_json::Value,
    ) -> Result<Vec<serde_json::Value>> {
        #[derive(Deserialize)]
        struct Session {
            node_did: String,
            session_id: String,
        }
        let session = Session::deserialize(args).context("decoding the session usage read")?;
        let usage = load_session_goal_usage(txn, &session.node_did, &session.session_id)
            .await
            .context("summing goal inference usage")?;
        Ok(vec![
            serde_json::json!({ "total": i64::try_from(usage.total).unwrap_or(i64::MAX) }),
        ])
    }
}

/// Registers the core reads this crate owns.
pub fn register_lifecycle_reads(registry: &mut gents_session::request::hooks::registry::Registry) {
    registry
        .reads
        .push(std::sync::Arc::new(SessionTokenUsageRead));
}

/// An auxiliary call enters the parent-inclusive total only through its
/// historically verified signed parent link.
pub async fn load_parent_inclusive_audit_usage(
    access: &dyn ConfigRead,
    exact_parent: &AgentRequestRow,
) -> Result<ParentAuditUsage> {
    let catalog = load_parent_catalog(access, exact_parent).await?;
    project_catalog(
        &catalog.parent_doc_id,
        &catalog.parent_id,
        &catalog.request_ids,
        catalog.calls,
    )
}

pub async fn load_parent_inclusive_budget_usage(
    access: &dyn ConfigRead,
    request: &gents_protocol::row::AgentRequest,
) -> Result<u64> {
    let scope = AgentRequestRow {
        doc_id: Some(request.doc_id.clone()),
        request_id: request.request_id.clone(),
        node_did: Some(request.node_did.clone()),
        requester_did: request.requester_did.clone(),
        agent_did: Some(request.agent_did.clone()),
        session_id: Some(request.session_id.clone()),
        ..Default::default()
    };
    let catalog = load_parent_catalog(access, &scope).await?;
    anyhow::ensure!(
        catalog.max_total_tokens == request.max_total_tokens,
        "budget restart disagrees with the execution owner's pinned limit"
    );
    project_budget_catalog(&catalog.request_ids, catalog.calls)
}

async fn load_parent_catalog(
    access: &dyn ConfigRead,
    exact_parent: &AgentRequestRow,
) -> Result<ParentUsageCatalog> {
    let parent_doc_id = nonempty(exact_parent.doc_id.as_deref(), "parent physical request")?;
    let parent_id = nonempty(Some(&exact_parent.request_id), "parent logical request")?;
    let agent = nonempty(exact_parent.node_did.as_deref(), "parent agent")?;
    let session = nonempty(exact_parent.session_id.as_deref(), "parent session")?;
    let parent_doc = escape_graphql_string(parent_doc_id);
    let response = access.execute_read(
        &format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{parent_doc}" }} }}, limit: 2) {{ {} }} }}"#,
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
        ),
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
            && parent.node_did.as_deref() == Some(agent)
            && parent.session_id.as_deref() == Some(session)
            && parent.requester_did == exact_parent.requester_did
            && parent.agent_did == exact_parent.agent_did
            && parent.purpose == Some(RequestPurpose::Normal),
        "audit usage parent crossed its exact normal request scope"
    );
    gents_store::identity::request_receipt::verify_request_receipt_signature(parent)?;

    let response = access.execute_read(
        &format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_request_doc_id: {{ _eq: "{parent_doc}" }}, purpose: {{ _in: ["title-audit", "access-review"] }} }}) {{ {} }} }}"#,
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
        ),
    )
    .await?;
    let auxiliaries: Vec<AgentRequestRow> = required_rows(&response, "AgentRequest")?;
    let mut request_ids = BTreeMap::new();
    request_ids.insert(parent_doc_id.to_owned(), parent_id.to_owned());
    for request in &auxiliaries {
        gents_store::identity::request_receipt::verify_historical_auxiliary_receipt(
            request, parent,
        )?;
        let request_doc = nonempty(request.doc_id.as_deref(), "auxiliary physical request")?;
        anyhow::ensure!(
            request_ids
                .insert(request_doc.to_owned(), request.request_id.clone())
                .is_none(),
            "duplicate auxiliary request physical identity"
        );
    }

    let docs = request_ids
        .keys()
        .map(|doc| format!("\"{}\"", escape_graphql_string(doc)))
        .collect::<Vec<_>>()
        .join(", ");
    let response = access.execute_read(
        &format!(
            r#"{{ InferenceCall(filter: {{ request_doc_id: {{ _in: [{docs}] }} }}) {{ _docID node_did call_id request_doc_id request_id backend_id call_state ended_at prompt_tokens completion_tokens cached_input_tokens cache_creation_input_tokens reasoning_output_tokens provider_total_tokens usage_completeness }} }}"#
        ),
    )
    .await?;
    let raw_calls = response
        .get("data")
        .and_then(|data| data.get("InferenceCall"))
        .and_then(serde_json::Value::as_array)
        .context("audit usage query omitted InferenceCall rows")?;
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
            "usage_completeness": "complete",
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

    type UsageInput = gents_lean_vocab::families::title_usage_catalog::UsageInput<String, String>;

    fn purpose(wire: &str) -> RequestPurpose {
        serde_json::from_value(serde_json::Value::String(wire.to_owned()))
            .unwrap_or_else(|error| panic!("unknown signed purpose {wire:?}: {error}"))
    }

    fn native_catalog(
        input: &UsageInput,
        after_action: bool,
    ) -> (BTreeMap<String, String>, Vec<CallRow>) {
        let mut request_ids = BTreeMap::new();
        request_ids.insert(
            format!("request-{}", input.parent.physical),
            input.parent.logical.to_string(),
        );
        for fact in &input.signed_purposes {
            if purpose(&fact.purpose).is_auxiliary() {
                request_ids.insert(
                    format!("request-{}", fact.physical),
                    input
                        .title_bindings
                        .iter()
                        .find(|binding| binding.physical == fact.physical)
                        .map_or_else(String::new, |binding| binding.binding.logical.to_string()),
                );
            }
        }
        let calls = input
            .rows
            .iter()
            .map(|row| {
                let usage = if after_action {
                    input
                        .usage_action
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
                    usage: gents_protocol::usage::TokenUsageColumns {
                        prompt_tokens: usage.map(|usage| usage.prompt_tokens as i64),
                        completion_tokens: usage.map(|usage| usage.completion_tokens as i64),
                        usage_completeness: usage
                            .map(|_| gents_protocol::usage::UsageCompleteness::Complete),
                        ..Default::default()
                    },
                }
            })
            .collect();
        (request_ids, calls)
    }

    fn catalog_error(name: &str, error: &anyhow::Error) -> String {
        match error.root_cause().to_string().as_str() {
            "one call ID has conflicting persisted rows" => "conflicting_call_id".to_owned(),
            other => panic!("{name} failed outside the modeled catalog errors: {other}"),
        }
    }

    fn budget_error(name: &str, error: &anyhow::Error) -> String {
        if error
            .to_string()
            .contains("requires reported provider usage")
        {
            "unreported_usage".to_owned()
        } else {
            catalog_error(name, error)
        }
    }

    #[test]
    fn generated_title_usage_cases_bind_native_catalog_and_late_usage() {
        use gents_lean_vocab::families::title_usage_catalog::{
            CatalogOutput, TokenPair, TotalsResult,
        };
        let observe = |input: &UsageInput, after_action: bool| {
            let (request_ids, calls) = native_catalog(input, after_action);
            match project_catalog(
                &format!("request-{}", input.parent.physical),
                &input.parent.logical.to_string(),
                &request_ids,
                calls,
            ) {
                Ok(totals) => TotalsResult {
                    kind: "ok".to_owned(),
                    normal_public: Some(TokenPair {
                        prompt_tokens: totals.normal_public.prompt_tokens,
                        completion_tokens: totals.normal_public.completion_tokens,
                    }),
                    parent_inclusive_audit: Some(TokenPair {
                        prompt_tokens: totals.parent_inclusive_audit.prompt_tokens,
                        completion_tokens: totals.parent_inclusive_audit.completion_tokens,
                    }),
                    error: None,
                },
                Err(error) => TotalsResult {
                    kind: "error".to_owned(),
                    normal_public: None,
                    parent_inclusive_audit: None,
                    error: Some(catalog_error(&input.name, &error)),
                },
            }
        };
        gents_lean_vocab::families::title_usage_catalog::replay::<String, String>(|input| {
            CatalogOutput {
                expected_before: observe(input, false),
                expected_after: observe(input, true),
            }
        });
    }

    #[test]
    fn generated_budget_recovery_cases_bind_signed_catalog_usage() {
        use gents_lean_vocab::families::title_usage_budget::{BudgetOutput, BudgetResult};
        let observe = |input: &UsageInput, after_action: bool| {
            let (request_ids, calls) = native_catalog(input, after_action);
            match project_budget_catalog(&request_ids, calls) {
                Ok(used) => BudgetResult {
                    kind: "ok".to_owned(),
                    used: Some(used),
                    error: None,
                },
                Err(error) => BudgetResult {
                    kind: "error".to_owned(),
                    used: None,
                    error: Some(budget_error(&input.name, &error)),
                },
            }
        };
        gents_lean_vocab::families::title_usage_budget::replay::<String, String>(|input| {
            let input: UsageInput = gents_lean_vocab::family::recode(input);
            BudgetOutput {
                budget_before: observe(&input, false),
                budget_after: observe(&input, true),
            }
        });
    }

    #[test]
    fn generated_goal_usage_cases_bind_goal_catalog() {
        use gents_lean_vocab::families::title_usage_goal::{BudgetResult, GoalOutput};
        let observe = |input: &UsageInput, after_action: bool| {
            let (mut request_ids, mut calls) = native_catalog(input, after_action);
            let titles: Vec<_> = input
                .signed_purposes
                .iter()
                .filter(|fact| purpose(&fact.purpose) == RequestPurpose::TitleAudit)
                .map(|fact| format!("request-{}", fact.physical))
                .collect();
            request_ids.retain(|doc, _| !titles.contains(doc));
            calls.retain(|call| request_ids.contains_key(&call.request_doc_id));
            match project_goal_catalog(&request_ids, calls) {
                Ok(usage) => BudgetResult {
                    kind: "ok".to_owned(),
                    used: Some(usage.total),
                    error: None,
                },
                Err(error) => BudgetResult {
                    kind: "error".to_owned(),
                    used: None,
                    error: Some(catalog_error(&input.name, &error)),
                },
            }
        };
        gents_lean_vocab::families::title_usage_goal::replay::<String, String>(|input| {
            let input: UsageInput = gents_lean_vocab::family::recode(input);
            GoalOutput {
                goal_before: observe(&input, false),
                goal_after: observe(&input, true),
            }
        });
    }
}
