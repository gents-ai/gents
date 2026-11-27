use chrono::Utc;
use serde_json::{json, Value};

use super::experience::{merge_admitted, normalize, MergeDecision};
use super::experience_rows::{
    parse_aliases, ALIAS_COLLECTION, ATTEMPT_COLLECTION, GUARD_COLLECTION, OUTCOME_COLLECTION,
};
use super::experience_store::{ensure_problem_in_txn, NewProblem};
use super::{
    content_hash, digest_key, link_parent_to_agent, nullable_string, nullable_string_list,
    MemoryParams, MemoryStore, MAX_ALIAS_ROWS, MAX_ATTEMPTS_PER_PAGE, MAX_OUTCOMES_PER_PAGE,
};

impl MemoryStore<'_> {
    pub(super) async fn attempt(
        &self,
        args: MemoryParams,
        runtime: Option<&gents_loop::tool_call_lifecycle::runtime::CurrentToolRuntimeContext>,
    ) -> anyhow::Result<Value> {
        let cited = args.problem_key.clone();
        let new_problem = args.new_problem.clone();
        let (problem_key, new_name) = match (cited, new_problem) {
            (Some(key), None) => (key, None),
            (None, Some(name)) => {
                let normalized = normalize(&name);
                if normalized.is_empty() {
                    anyhow::bail!("new_problem must contain non-whitespace text");
                }
                (
                    content_hash(self.agent_did, &normalized),
                    Some((name, normalized)),
                )
            }
            (Some(_), Some(_)) => {
                anyhow::bail!(
                    "attempt takes problem_key or new_problem, not both; call memory problems for an index or memory problem with name to open one"
                )
            }
            (None, None) => {
                anyhow::bail!(
                    "attempt needs problem_key from memory problem or problems, or new_problem when none explains the failure"
                )
            }
        };
        let what = args
            .what
            .as_deref()
            .map(str::trim)
            .filter(|what| !what.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("attempt requires what: the committed change, in one line")
            })?
            .to_owned();
        let request_id = runtime
            .and_then(|runtime| runtime.request_id.as_deref())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "attempt needs a request; call it from a session request so provenance is recorded"
                )
            })?
            .to_owned();
        let session_id = runtime.and_then(|runtime| runtime.session_id.as_deref());
        let correlation = runtime
            .and_then(|runtime| runtime.correlation.as_deref())
            .unwrap_or("");
        let context = self.caller_context(Some(&request_id), session_id).await?;
        let rationale = args.rationale.clone().unwrap_or_default();
        let predicted = args.predicted.clone().unwrap_or_default();
        let accepted = args.accepted.unwrap_or(true);
        let new_vector = match new_name.as_ref() {
            Some((name, _)) => Some(
                self.embedder
                    .embed_one(name.clone())
                    .await
                    .map_err(|error| anyhow::anyhow!("embedding failed: {error}"))?,
            ),
            None => None,
        };
        let agent_did = self.agent_did.to_owned();
        let node_did = self.node_did.to_owned();
        let problem_key_owned = problem_key.clone();
        let new_name_owned = new_name.clone();
        let provenance = context.provenance.clone();
        let request_id_clone = request_id.clone();
        let session_id_owned = context.session_id.clone();
        let predicted_literal = nullable_string_list(&predicted);
        let created_at = Utc::now().to_rfc3339();
        let (attempt_key, problem_key) = self
            .access
            .transact_write("tool.memory.attempt", move |txn| {
                let agent_did = agent_did.clone();
                let node_did = node_did.clone();
                let problem_key = problem_key_owned.clone();
                let new_name = new_name_owned.clone();
                let new_vector = new_vector.clone();
                let provenance = provenance.clone();
                let request_id = request_id_clone.clone();
                let session_id = session_id_owned.clone();
                let predicted = predicted_literal.clone();
                let created_at = created_at.clone();
                let what = what.clone();
                let rationale = rationale.clone();
                let correlation = correlation.to_owned();
                Box::pin(async move {
                    let (agent_doc_id, _) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Agent,
                        &node_did,
                        &agent_did,
                    )
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("the agent has no readable Agent document"))?;
                    if let Some((name, normalized)) = new_name {
                        let vector = new_vector.unwrap_or_default();
                        ensure_problem_in_txn(
                            txn,
                            &agent_doc_id,
                            &agent_did,
                            &NewProblem {
                                problem_key: &problem_key,
                                name: &name,
                                normalized: &normalized,
                                vector: &vector,
                                request_id: Some(&request_id),
                                session_id: session_id.as_deref(),
                                provenance: &provenance,
                                created_at: &created_at,
                            },
                        )
                        .await?;
                    } else {
                        let existing = txn
                            .execute(&format!(
                                "{{AgentMemoryProblem(filter: {{_and: [{{agent_did: {{_eq: \"{agent}\"}}}}, {{problem_key: {{_eq: \"{key}\"}}}}]}}, limit: 1) {{ problem_key }}}}",
                                agent = gents_store::graphql::escape_graphql_string(&agent_did),
                                key = gents_store::graphql::escape_graphql_string(&problem_key),
                            ))
                            .await?;
                        if existing
                            .pointer("/data/AgentMemoryProblem/0/problem_key")
                            .is_none()
                        {
                            anyhow::bail!(
                                "no problem {problem_key}; call memory problem with name to open one or memory problems to list the index"
                            );
                        }
                    }
                    let prior = txn
                        .execute(&format!(
                            "{{AgentMemoryAttempt(filter: {{_and: [{{agent_did: {{_eq: \"{agent}\"}}}}, {{request_id: {{_eq: \"{request}\"}}}}, {{problem_key: {{_eq: \"{key}\"}}}}]}}, limit: {limit}) {{ attempt_key }}}}",
                            agent = gents_store::graphql::escape_graphql_string(&agent_did),
                            request = gents_store::graphql::escape_graphql_string(&request_id),
                            key = gents_store::graphql::escape_graphql_string(&problem_key),
                            limit = MAX_ATTEMPTS_PER_PAGE + 1,
                        ))
                        .await?;
                    let ordinal = prior
                        .pointer("/data/AgentMemoryAttempt")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    if ordinal >= MAX_ATTEMPTS_PER_PAGE {
                        anyhow::bail!(
                            "more than {MAX_ATTEMPTS_PER_PAGE} attempts under this request and problem; start a new request"
                        );
                    }
                    let attempt_key = digest_key(&[
                        &agent_did,
                        &problem_key,
                        &request_id,
                        &correlation,
                        &ordinal.to_string(),
                    ]);
                    let mutation = format!(
                        "mutation {{ add_AgentMemoryAttempt(input: {{ attempt_key: \"{attempt_key}\", agent_did: \"{agent}\", problem_key: \"{problem}\", what: \"{what}\", rationale: \"{rationale}\", predicted: {predicted}, accepted: {accepted}, provenance: {provenance}, request_id: \"{request}\", session_id: {session_id}, created_at: \"{created_at}\" }}) {{ _docID }} }}",
                        attempt_key = gents_store::graphql::escape_graphql_string(&attempt_key),
                        agent = gents_store::graphql::escape_graphql_string(&agent_did),
                        problem = gents_store::graphql::escape_graphql_string(&problem_key),
                        what = gents_store::graphql::escape_graphql_string(&what),
                        rationale = gents_store::graphql::escape_graphql_string(&rationale),
                        request = gents_store::graphql::escape_graphql_string(&request_id),
                        session_id = nullable_string(session_id.as_deref()),
                        created_at = gents_store::graphql::escape_graphql_string(&created_at),
                    );
                    let created = txn.execute(&mutation).await?;
                    let doc_id = created
                        .pointer(&format!("/data/add_{ATTEMPT_COLLECTION}/0/_docID"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow::anyhow!("attempt write returned no document id"))?
                        .to_owned();
                    link_parent_to_agent(txn, ATTEMPT_COLLECTION, &doc_id, &agent_doc_id).await?;
                    Ok::<(String, String), anyhow::Error>((attempt_key, problem_key))
                })
            })
            .await?;
        Ok(json!({"attempt_key": attempt_key, "problem_key": problem_key}))
    }

    pub(super) async fn outcome(
        &self,
        args: MemoryParams,
        runtime: Option<&gents_loop::tool_call_lifecycle::runtime::CurrentToolRuntimeContext>,
    ) -> anyhow::Result<Value> {
        let attempt_key = args
            .attempt_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "outcome requires attempt_key from memory attempt; call memory problems to find the problem"
                )
            })?
            .to_owned();
        let fixed = args.fixed.clone().unwrap_or_default();
        let missed = args.missed.clone().unwrap_or_default();
        let passed = args.passed.unwrap_or(0).max(0) as u64;
        let total = args.total.unwrap_or(0).max(0) as u64;
        let note = args.note.clone().unwrap_or_default();
        let request_id = runtime.and_then(|runtime| runtime.request_id.as_deref());
        let session_id = runtime.and_then(|runtime| runtime.session_id.as_deref());
        let correlation = runtime
            .and_then(|runtime| runtime.correlation.as_deref())
            .unwrap_or("")
            .to_owned();
        let context = self.caller_context(request_id, session_id).await?;
        let agent_did = self.agent_did.to_owned();
        let node_did = self.node_did.to_owned();
        let provenance = context.provenance.clone();
        let request_id_owned = context.request_id.clone();
        let request_part = context.request_id.clone().unwrap_or_default();
        let fixed_literal = nullable_string_list(&fixed);
        let missed_literal = nullable_string_list(&missed);
        let created_at = Utc::now().to_rfc3339();
        let outcome_key = self
            .access
            .transact_write("tool.memory.outcome", move |txn| {
                let agent_did = agent_did.clone();
                let node_did = node_did.clone();
                let attempt_key = attempt_key.clone();
                let provenance = provenance.clone();
                let fixed = fixed_literal.clone();
                let missed = missed_literal.clone();
                let note = note.clone();
                let created_at = created_at.clone();
                let request_id = request_id_owned.clone();
                let request_part = request_part.clone();
                let correlation = correlation.clone();
                Box::pin(async move {
                    let (agent_doc_id, _) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Agent,
                        &node_did,
                        &agent_did,
                    )
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("the agent has no readable Agent document"))?;
                    let attempt = txn
                        .execute(&format!(
                            "{{AgentMemoryAttempt(filter: {{_and: [{{agent_did: {{_eq: \"{agent}\"}}}}, {{attempt_key: {{_eq: \"{attempt}\"}}}}]}}, limit: 1) {{ attempt_key }}}}",
                            agent = gents_store::graphql::escape_graphql_string(&agent_did),
                            attempt = gents_store::graphql::escape_graphql_string(&attempt_key),
                        ))
                        .await?;
                    if attempt
                        .pointer("/data/AgentMemoryAttempt/0/attempt_key")
                        .is_none()
                    {
                        anyhow::bail!(
                            "no attempt {attempt_key} under this agent; call memory problem with problem_key to show its attempts, or memory attempt to record one"
                        );
                    }
                    let prior = txn
                        .execute(&format!(
                            "{{AgentMemoryOutcome(filter: {{_and: [{{agent_did: {{_eq: \"{agent}\"}}}}, {{attempt_key: {{_eq: \"{attempt}\"}}}}]}}, limit: {limit}) {{ outcome_key }}}}",
                            agent = gents_store::graphql::escape_graphql_string(&agent_did),
                            attempt = gents_store::graphql::escape_graphql_string(&attempt_key),
                            limit = MAX_OUTCOMES_PER_PAGE + 1,
                        ))
                        .await?;
                    let ordinal = prior
                        .pointer("/data/AgentMemoryOutcome")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    if ordinal >= MAX_OUTCOMES_PER_PAGE {
                        anyhow::bail!(
                            "more than {MAX_OUTCOMES_PER_PAGE} outcomes for this attempt; start a new attempt"
                        );
                    }
                    let outcome_key = digest_key(&[
                        &agent_did,
                        &attempt_key,
                        &request_part,
                        &correlation,
                        &ordinal.to_string(),
                    ]);
                    let mutation = format!(
                        "mutation {{ add_AgentMemoryOutcome(input: {{ outcome_key: \"{outcome_key}\", agent_did: \"{agent}\", attempt_key: \"{attempt}\", fixed: {fixed}, missed: {missed}, passed: {passed}, total: {total}, note: \"{note}\", provenance: {provenance}, request_id: {request}, created_at: \"{created_at}\" }}) {{ _docID }} }}",
                        outcome_key = gents_store::graphql::escape_graphql_string(&outcome_key),
                        agent = gents_store::graphql::escape_graphql_string(&agent_did),
                        attempt = gents_store::graphql::escape_graphql_string(&attempt_key),
                        note = gents_store::graphql::escape_graphql_string(&note),
                        request = nullable_string(request_id.as_deref()),
                        created_at = gents_store::graphql::escape_graphql_string(&created_at),
                    );
                    let created = txn.execute(&mutation).await?;
                    let doc_id = created
                        .pointer(&format!("/data/add_{OUTCOME_COLLECTION}/0/_docID"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow::anyhow!("outcome write returned no document id"))?
                        .to_owned();
                    link_parent_to_agent(txn, OUTCOME_COLLECTION, &doc_id, &agent_doc_id).await?;
                    Ok::<String, anyhow::Error>(outcome_key)
                })
            })
            .await?;
        Ok(json!({"outcome_key": outcome_key, "attempt_key": args.attempt_key}))
    }

    pub(super) async fn merge(
        &self,
        args: MemoryParams,
        runtime: Option<&gents_loop::tool_call_lifecycle::runtime::CurrentToolRuntimeContext>,
    ) -> anyhow::Result<Value> {
        let from = args
            .from
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("merge requires from: the problem to merge away"))?
            .to_owned();
        let to = args
            .to
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("merge requires to: the problem that remains"))?
            .to_owned();
        let request_id = runtime.and_then(|runtime| runtime.request_id.as_deref());
        let session_id = runtime.and_then(|runtime| runtime.session_id.as_deref());
        let context = self.caller_context(request_id, session_id).await?;
        let agent_did = self.agent_did.to_owned();
        let node_did = self.node_did.to_owned();
        let created_at = Utc::now().to_rfc3339();
        let request = nullable_string(context.request_id.as_deref());
        let operation = self
            .access
            .transact_write("tool.memory.merge", move |txn| {
                let agent_did = agent_did.clone();
                let node_did = node_did.clone();
                let from = from.clone();
                let to = to.clone();
                let created_at = created_at.clone();
                let request = request.clone();
                Box::pin(async move {
                    let (agent_doc_id, _) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Agent,
                        &node_did,
                        &agent_did,
                    )
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("the agent has no readable Agent document"))?;
                    let aliases_value = txn
                        .execute(&format!(
                            "{{AgentMemoryAlias(filter: {{agent_did: {{_eq: \"{}\"}}}}, limit: {}) {{ from_problem_key to_problem_key }}}}",
                            gents_store::graphql::escape_graphql_string(&agent_did),
                            MAX_ALIAS_ROWS + 1,
                        ))
                        .await?;
                    let aliases = parse_aliases(&aliases_value)?;
                    let problems_value = txn
                        .execute(&format!(
                            "{{AgentMemoryProblem(filter: {{_and: [{{agent_did: {{_eq: \"{agent}\"}}}}, {{problem_key: {{_in: [\"{from}\", \"{to}\"]}}}}]}}) {{ problem_key }}}}",
                            agent = gents_store::graphql::escape_graphql_string(&agent_did),
                            from = gents_store::graphql::escape_graphql_string(&from),
                            to = gents_store::graphql::escape_graphql_string(&to),
                        ))
                        .await?;
                    let problems: Vec<String> = problems_value
                        .pointer("/data/AgentMemoryProblem")
                        .and_then(Value::as_array)
                        .map(|rows| {
                            rows.iter()
                                .filter_map(|row| {
                                    row.get("problem_key")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let decision = merge_admitted(&problems, &aliases, &from, &to);
                    if decision != MergeDecision::Admitted {
                        return Ok::<(MergeDecision, i64), anyhow::Error>((decision, 0));
                    }
                    let guard_value = txn
                        .execute(&format!(
                            "{{AgentMemoryAliasGuard(filter: {{agent_did: {{_eq: \"{}\"}}}}, limit: 1) {{ _docID merges }}}}",
                            gents_store::graphql::escape_graphql_string(&agent_did),
                        ))
                        .await?;
                    let guard = guard_value.pointer("/data/AgentMemoryAliasGuard/0");
                    let guard_doc_id = guard
                        .and_then(|row| row.get("_docID"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let merges = guard
                        .and_then(|row| row.get("merges"))
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        + 1;
                    let alias_mutation = format!(
                        "mutation {{ add_AgentMemoryAlias(input: {{ from_problem_key: \"{from}\", agent_did: \"{agent}\", to_problem_key: \"{to}\", request_id: {request}, created_at: \"{created_at}\" }}) {{ _docID }} }}",
                        from = gents_store::graphql::escape_graphql_string(&from),
                        agent = gents_store::graphql::escape_graphql_string(&agent_did),
                        to = gents_store::graphql::escape_graphql_string(&to),
                        created_at = gents_store::graphql::escape_graphql_string(&created_at),
                    );
                    let created = txn.execute(&alias_mutation).await?;
                    let alias_doc_id = created
                        .pointer(&format!("/data/add_{ALIAS_COLLECTION}/0/_docID"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow::anyhow!("alias write returned no document id"))?
                        .to_owned();
                    link_parent_to_agent(txn, ALIAS_COLLECTION, &alias_doc_id, &agent_doc_id)
                        .await?;
                    match guard_doc_id {
                        Some(doc_id) => {
                            txn.execute(&format!(
                                "mutation {{ update_AgentMemoryAliasGuard(filter: {{ _docID: {{ _eq: \"{}\" }} }}, input: {{ merges: {merges}, updated_at: \"{updated_at}\" }}) {{ _docID }} }}",
                                gents_store::graphql::escape_graphql_string(&doc_id),
                                updated_at = gents_store::graphql::escape_graphql_string(&created_at),
                            ))
                            .await?;
                        }
                        None => {
                            let guard_mutation = format!(
                                "mutation {{ add_AgentMemoryAliasGuard(input: {{ agent_did: \"{agent}\", merges: 1, updated_at: \"{updated_at}\" }}) {{ _docID }} }}",
                                agent = gents_store::graphql::escape_graphql_string(&agent_did),
                                updated_at = gents_store::graphql::escape_graphql_string(&created_at),
                            );
                            let created = txn.execute(&guard_mutation).await?;
                            let guard_doc_id = created
                                .pointer(&format!("/data/add_{GUARD_COLLECTION}/0/_docID"))
                                .and_then(Value::as_str)
                                .ok_or_else(|| anyhow::anyhow!("guard write returned no document id"))?
                                .to_owned();
                            link_parent_to_agent(
                                txn,
                                GUARD_COLLECTION,
                                &guard_doc_id,
                                &agent_doc_id,
                            )
                            .await?;
                        }
                    }
                    Ok::<(MergeDecision, i64), anyhow::Error>((decision, merges))
                })
            })
            .await?;
        match operation {
            (MergeDecision::Admitted, merges) => Ok(json!({
                "decision": "admitted",
                "from": args.from,
                "to": args.to,
                "merges": merges,
            })),
            (decision, _) => Ok(json!({
                "decision": decision.as_str(),
                "message": merge_message(decision, &args),
                "from": args.from,
                "to": args.to,
            })),
        }
    }
}

fn merge_message(decision: MergeDecision, args: &MemoryParams) -> String {
    let from = args.from.as_deref().unwrap_or("");
    let to = args.to.as_deref().unwrap_or("");
    match decision {
        MergeDecision::Admitted => "merge admitted".to_owned(),
        MergeDecision::UnknownKey => format!(
            "merge refused (unknown_key): {from} or {to} names no problem of this agent; call memory problems to list the index or memory problem with name to open one"
        ),
        MergeDecision::SameKey => format!(
            "merge refused (same_key): {from} and {to} are one problem; cite two different problem keys from memory problems"
        ),
        MergeDecision::AlreadyMerged => format!(
            "merge refused (already_merged): {from} is already merged into another problem; call memory problems to read the index, and merge that target instead"
        ),
        MergeDecision::ClosesCycle => format!(
            "merge refused (closes_cycle): merging {from} into {to} would close an alias cycle; choose the problem that should remain and merge the other into it"
        ),
    }
}
