use std::collections::BTreeMap;

use anyhow::{Context, Result};
use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::row::AgentRequestRow;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use gents_store::config_client::ConfigRead;
use gents_store::graphql::escape_graphql_string;

/// Why a parent's audit, budget or goal usage cannot be observed. Each refusal
/// fails the whole catalog: no row it covers is left out silently.
#[derive(Debug, thiserror::Error)]
pub enum TitleUsageError {
    /// A request receipt in the catalog fails historical verification against
    /// its parent: a bad signature, or a parent link that names the parent's
    /// physical request but not its logical request, node, session or agent.
    #[error("audit usage request receipt is not bound to its parent")]
    Receipt(#[from] gents_store::identity::request_receipt::RequestReceiptError),
    #[error("inference call logical request disagrees with its physical owner")]
    CallLogicalScope,
    #[error("one call ID has conflicting persisted rows")]
    ConflictingCall,
    #[error("budget restart requires reported provider usage for call {0}")]
    UnreportedUsage(String),
    #[error("audit usage read failed")]
    Read(#[from] gents_store::config_client::ConfigClientError),
    #[error("audit usage observation is unavailable")]
    Unavailable(#[from] anyhow::Error),
}

type UsageResult<T> = std::result::Result<T, TitleUsageError>;

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
) -> UsageResult<BTreeMap<String, CallRow>> {
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
        if call.request_id != *logical {
            return Err(TitleUsageError::CallLogicalScope);
        }
        if let Some(previous) = by_doc.insert(call.doc_id.clone(), call.call_id.clone()) {
            if previous != call.call_id {
                return Err(
                    anyhow::anyhow!("one inference document has conflicting call IDs").into(),
                );
            }
        }
        if let Some(previous) = by_call.get(&call.call_id) {
            if previous != &call {
                return Err(TitleUsageError::ConflictingCall);
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
) -> UsageResult<ParentAuditUsage> {
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
) -> UsageResult<u64> {
    coherent_calls(request_ids, calls)?
        .values()
        .try_fold(0_u64, |used, call| {
            let charged = observed_usage(call)?
                .charged_tokens()
                .map_err(anyhow::Error::from)?
                .filter(|charged| *charged > 0)
                .ok_or_else(|| TitleUsageError::UnreportedUsage(call.call_id.clone()))?;
            Ok(used
                .checked_add(charged)
                .context("budget restart usage overflow")?)
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
) -> UsageResult<gents_protocol::session_token_usage::SessionTokenUsage> {
    let calls = coherent_calls(request_ids, calls)?;
    let parts = || {
        calls
            .values()
            .map(|call| (call.usage.prompt_tokens, call.usage.completion_tokens))
    };
    let total = gents_loop::provider_usage::sum_charged_from_persisted_parts(parts())
        .map_err(anyhow::Error::from)?;
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
) -> UsageResult<gents_protocol::session_token_usage::SessionTokenUsage> {
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
    session_goal_usage_from_rows(txn, node_did, &requests).await
}

pub async fn session_goal_usage_from_rows(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    rows: &[AgentRequestRow],
) -> UsageResult<gents_protocol::session_token_usage::SessionTokenUsage> {
    let requests: Vec<&AgentRequestRow> = rows
        .iter()
        .filter(|request| {
            matches!(
                request.purpose,
                Some(RequestPurpose::Normal) | Some(RequestPurpose::AccessReview)
            )
        })
        .collect();
    let mut by_doc = BTreeMap::new();
    for request in requests.iter().copied() {
        let doc = nonempty(request.doc_id.as_deref(), "goal physical request")?;
        if by_doc.insert(doc, request).is_some() {
            return Err(anyhow::anyhow!("duplicate goal request physical identity").into());
        }
    }
    let mut request_ids = BTreeMap::new();
    for request in requests.iter().copied() {
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
) -> UsageResult<ParentAuditUsage> {
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
) -> UsageResult<u64> {
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
    if catalog.max_total_tokens != request.max_total_tokens {
        return Err(anyhow::anyhow!(
            "budget restart disagrees with the execution owner's pinned limit"
        )
        .into());
    }
    project_budget_catalog(&catalog.request_ids, catalog.calls)
}

async fn load_parent_catalog(
    access: &dyn ConfigRead,
    exact_parent: &AgentRequestRow,
) -> UsageResult<ParentUsageCatalog> {
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
    if parents.len() != 1 {
        return Err(anyhow::anyhow!("audit usage parent is missing or ambiguous").into());
    }
    let parent = &parents[0];
    if !(parent.doc_id.as_deref() == Some(parent_doc_id)
        && parent.request_id == parent_id
        && parent.node_did.as_deref() == Some(agent)
        && parent.session_id.as_deref() == Some(session)
        && parent.requester_did == exact_parent.requester_did
        && parent.agent_did == exact_parent.agent_did
        && parent.purpose == Some(RequestPurpose::Normal))
    {
        return Err(
            anyhow::anyhow!("audit usage parent crossed its exact normal request scope").into(),
        );
    }
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
        if request_ids
            .insert(request_doc.to_owned(), request.request_id.clone())
            .is_some()
        {
            return Err(anyhow::anyhow!("duplicate auxiliary request physical identity").into());
        }
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
    type ModelUsage = gents_lean_vocab::families::title_usage_catalog::PersistedUsage;

    const CREATED_AT: &str = "2026-11-01T00:00:00Z";

    fn node_label(label: u64) -> String {
        format!("node-{label}")
    }

    fn session_label(label: u64) -> String {
        format!("session-{label}")
    }

    fn request_label(label: u64) -> String {
        format!("request-{label}")
    }

    fn purpose(wire: &str) -> RequestPurpose {
        serde_json::from_value(serde_json::Value::String(wire.to_owned()))
            .unwrap_or_else(|error| panic!("unknown signed purpose {wire:?}: {error}"))
    }

    /// A served node holding one case's signed requests and inference calls,
    /// read as the node root that wrote them.
    struct Materialized {
        _identities: tempfile::TempDir,
        db: gents_inference::test_support::TestDb,
        access: gents_store::config_client::ConfigAccess,
        root: identity::Did,
        parent_doc: String,
        call_docs: BTreeMap<u64, String>,
    }

    async fn write(
        node: &std::sync::Arc<defra_node::Node>,
        root: &identity::Did,
        mutation: String,
    ) -> String {
        gents_store::config_client::ConfigAccess::local_as(node.clone(), root.clone())
            .transact_write("test.title_usage.write", |txn| {
                let mutation = &mutation;
                Box::pin(async move {
                    let response = txn.execute(mutation).await?;
                    let collection = if mutation.contains("create_AgentRequest") {
                        "AgentRequest"
                    } else {
                        "InferenceCall"
                    };
                    Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(
                        &response, collection,
                    )?)
                })
            })
            .await
            .expect("write case row")
    }

    /// Call rows are written without an actor, as public rows every reader
    /// sees: the catalog reads calls by their request documents, not by who
    /// may read them.
    async fn write_call(node: &std::sync::Arc<defra_node::Node>, mutation: &str) -> String {
        let response = node.execute(mutation).await;
        assert!(
            !response.has_errors(),
            "call row write failed: {:?}",
            response.errors
        );
        let response = serde_json::json!({ "data": response.data });
        if mutation.contains("create_InferenceCall") {
            gents_store::graphql::created_doc_id(&response, "InferenceCall").unwrap()
        } else {
            String::new()
        }
    }

    fn usage_fields(usage: Option<&ModelUsage>) -> String {
        usage.map_or_else(String::new, |usage| {
            format!(
                "prompt_tokens: {}, completion_tokens: {}, usage_completeness: \"complete\",",
                usage.prompt_tokens, usage.completion_tokens
            )
        })
    }

    /// Writes the parent as a signed normal request, each auxiliary as the
    /// signed local-control receipt its binding describes (the receipt is its
    /// own binding), and each call row under the call ID that keys it.
    async fn materialize(input: &UsageInput) -> Materialized {
        let identities = tempfile::tempdir().unwrap();
        let root_identity =
            gents_store::test_support::pin_fixed_signing_identity(identities.path());
        let db = gents_inference::test_support::test_db_as(
            "title-usage",
            gents_store::test_support::PIN_FIXED_DID,
            gents_session::test_support::no_lifecycle_plugins(),
        )
        .await;
        let root: identity::Did = gents_store::test_support::PIN_FIXED_DID.parse().unwrap();
        let agent =
            gents_store::identity::derive_agent_identity(&root_identity, &"a".repeat(64)).unwrap();
        let parent = &input.parent;

        let mut parent_create = gents_protocol::request_admission::AgentRequestCreate::base(
            RequestPurpose::Normal,
            request_label(parent.logical),
            gents_protocol::request_admission::RequestScope::new(
                node_label(parent.node),
                agent.did(),
                agent.did(),
                session_label(parent.session),
            ),
            "work",
            "interactive",
            CREATED_AT,
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(agent.did()),
        );
        gents_session::request::admission::sign_agent_request_create(
            agent.as_ref(),
            &mut parent_create,
        )
        .await
        .unwrap();
        let parent_doc = write(&db.node, &root, parent_create.graphql_mutation().unwrap()).await;
        let mut request_docs = BTreeMap::from([(parent.physical, parent_doc.clone())]);

        for fact in &input.signed_purposes {
            if fact.physical == parent.physical {
                assert_eq!(purpose(&fact.purpose), RequestPurpose::Normal);
                continue;
            }
            let binding = &input
                .title_bindings
                .iter()
                .find(|binding| binding.physical == fact.physical)
                .expect("a built auxiliary carries its own binding")
                .binding;
            assert_eq!(binding.purpose, fact.purpose);
            assert_eq!(binding.physical, fact.physical);
            let source = request_label(binding.parent_logical);
            let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
                purpose(&fact.purpose),
                request_label(binding.logical),
                gents_protocol::request_admission::RequestScope::new(
                    node_label(binding.node),
                    agent.did(),
                    agent.did(),
                    session_label(binding.session),
                ),
                "audit",
                "interactive",
                CREATED_AT,
                gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_control(
                    agent.did(),
                    &source,
                ),
            );
            create.caused_by_parent_request_id = Some(source);
            create.caused_by_parent_request_doc_id = Some(
                request_docs
                    .get(&binding.parent_physical)
                    .cloned()
                    .unwrap_or_else(|| format!("doc-{}", binding.parent_physical)),
            );
            gents_session::request::admission::sign_agent_request_create(
                agent.as_ref(),
                &mut create,
            )
            .await
            .unwrap();
            if !binding.authenticated {
                create.content.push_str(" tampered");
            }
            let doc = write(&db.node, &root, create.graphql_mutation().unwrap()).await;
            request_docs.insert(fact.physical, doc);
        }

        let mut call_docs = BTreeMap::new();
        for row in &input.rows {
            assert_eq!(row.key, row.call_id, "the catalog is keyed by call ID");
            let ended_at = row.terminal_stamp.map_or_else(String::new, |stamp| {
                format!(
                    "ended_at: \"{}\",",
                    chrono::DateTime::from_timestamp(i64::try_from(stamp).unwrap(), 0)
                        .unwrap()
                        .to_rfc3339()
                )
            });
            let mutation = format!(
                r#"mutation {{ create_InferenceCall(input: {{ call_id: "call-{}", request_doc_id: "{}", request_id: "{}", backend_id: "{}", call_state: "{}", node_did: "{}", {ended_at} {} }}) {{ _docID }} }}"#,
                row.call_id,
                request_docs
                    .get(&row.request_physical)
                    .expect("a call row belongs to a written request"),
                request_label(row.request_logical),
                escape_graphql_string(&row.backend),
                row.state,
                node_label(parent.node),
                usage_fields(row.usage.as_ref()),
            );
            call_docs.insert(row.call_id, write_call(&db.node, &mutation).await);
        }
        Materialized {
            _identities: identities,
            access: gents_store::config_client::ConfigAccess::local_as(
                db.node.clone(),
                root.clone(),
            ),
            db,
            root,
            parent_doc,
            call_docs,
        }
    }

    impl Materialized {
        async fn apply(&self, input: &UsageInput) {
            if let Some(action) = &input.usage_action {
                let doc = &self.call_docs[&action.call_id];
                write_call(
                    &self.db.node,
                    &format!(
                        r#"mutation {{ update_InferenceCall(docID: "{doc}", input: {{ {} }}) {{ _docID }} }}"#,
                        usage_fields(Some(&action.usage))
                    ),
                )
                .await;
            }
        }

        async fn parent(&self) -> gents_protocol::row::AgentRequest {
            let response = self
                .access
                .execute(&format!(
                    "{{ AgentRequest(docIDs: [\"{}\"]) {{ {} }} }}",
                    escape_graphql_string(&self.parent_doc),
                    gents_protocol::row::AGENT_REQUEST_FIELDS,
                ))
                .await
                .unwrap();
            let rows: Vec<AgentRequestRow> =
                serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap();
            gents_protocol::row::AgentRequest::try_from(rows[0].clone()).unwrap()
        }

        async fn catalog(&self) -> UsageResult<ParentAuditUsage> {
            let parent = self.parent().await;
            let scope = AgentRequestRow {
                doc_id: Some(parent.doc_id.clone()),
                request_id: parent.request_id.clone(),
                node_did: Some(parent.node_did.clone()),
                requester_did: parent.requester_did.clone(),
                agent_did: Some(parent.agent_did.clone()),
                session_id: Some(parent.session_id.clone()),
                ..Default::default()
            };
            load_parent_inclusive_audit_usage(&self.access, &scope).await
        }

        async fn budget(&self) -> UsageResult<u64> {
            load_parent_inclusive_budget_usage(&self.access, &self.parent().await).await
        }

        async fn goal(&self) -> UsageResult<u64> {
            let parent = self.parent().await;
            gents_store::config_client::ConfigAccess::transact_local_readonly(
                &self.db.node,
                Some(self.root.clone()),
                "test.title_usage.goal",
                |txn| {
                    let parent = &parent;
                    Box::pin(async move {
                        Ok::<_, anyhow::Error>(
                            load_session_goal_usage(txn, &parent.node_did, &parent.session_id)
                                .await
                                .map(|usage| usage.total),
                        )
                    })
                },
            )
            .await
            .unwrap()
        }

        async fn shutdown(self) {
            self.db.node.shutdown().await.expect("node shutdown");
        }
    }

    /// The model's name for the refusal production returned.
    fn refusal(name: &str, error: &TitleUsageError) -> String {
        match error {
            TitleUsageError::Receipt(_) | TitleUsageError::CallLogicalScope => {
                "invalid_title_binding"
            }
            TitleUsageError::ConflictingCall => "conflicting_call_id",
            TitleUsageError::UnreportedUsage(_) => "unreported_usage",
            other => panic!("{name} failed outside the modeled refusals: {other:#}"),
        }
        .to_owned()
    }

    #[tokio::test]
    async fn generated_title_usage_cases_bind_the_signed_catalog() {
        use gents_lean_vocab::families::{
            title_usage_budget, title_usage_catalog, title_usage_goal,
        };
        let catalog = |name: &str, result: UsageResult<ParentAuditUsage>| match result {
            Ok(totals) => title_usage_catalog::TotalsResult {
                kind: "ok".to_owned(),
                normal_public: Some(title_usage_catalog::TokenPair {
                    prompt_tokens: totals.normal_public.prompt_tokens,
                    completion_tokens: totals.normal_public.completion_tokens,
                }),
                parent_inclusive_audit: Some(title_usage_catalog::TokenPair {
                    prompt_tokens: totals.parent_inclusive_audit.prompt_tokens,
                    completion_tokens: totals.parent_inclusive_audit.completion_tokens,
                }),
                error: None,
            },
            Err(error) => title_usage_catalog::TotalsResult {
                kind: "error".to_owned(),
                normal_public: None,
                parent_inclusive_audit: None,
                error: Some(refusal(name, &error)),
            },
        };
        let used = |name: &str, result: UsageResult<u64>| match result {
            Ok(used) => title_usage_budget::BudgetResult {
                kind: "ok".to_owned(),
                used: Some(used),
                error: None,
            },
            Err(error) => title_usage_budget::BudgetResult {
                kind: "error".to_owned(),
                used: None,
                error: Some(refusal(name, &error)),
            },
        };
        let catalog_cases = title_usage_catalog::cases::<String, String>();
        let budget_cases = title_usage_budget::cases::<String, String>();
        let goal_cases = title_usage_goal::cases::<String, String>();
        for case in &catalog_cases {
            let input = &case.input;
            let budget_case = budget_cases
                .iter()
                .find(|other| other.input.name == input.name)
                .expect("the budget family shares the catalog's cases");
            let goal_case = goal_cases
                .iter()
                .find(|other| other.input.name == input.name)
                .expect("the goal family shares the catalog's cases");
            let world = materialize(input).await;
            let before = (
                catalog(&input.name, world.catalog().await),
                used(&input.name, world.budget().await),
                used(&input.name, world.goal().await),
            );
            world.apply(input).await;
            let after = (
                catalog(&input.name, world.catalog().await),
                used(&input.name, world.budget().await),
                used(&input.name, world.goal().await),
            );
            world.shutdown().await;
            case.check(title_usage_catalog::CatalogOutput {
                expected_before: before.0,
                expected_after: after.0,
            });
            budget_case.check(title_usage_budget::BudgetOutput {
                budget_before: before.1,
                budget_after: after.1,
            });
            goal_case.check(title_usage_goal::GoalOutput {
                goal_before: gents_lean_vocab::family::recode(&before.2),
                goal_after: gents_lean_vocab::family::recode(&after.2),
            });
        }
        assert_eq!(budget_cases.len(), catalog_cases.len());
        assert_eq!(goal_cases.len(), catalog_cases.len());
    }
}
