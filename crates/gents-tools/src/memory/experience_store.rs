use std::cmp::Ordering;

use chrono::Utc;
use serde_json::{json, Map, Value};

use super::experience::{
    flags, fold, normalize, resolve, unresolved_count, AttemptRow, Current, Experience,
    FoldedProblem, OutcomeRow, Rows, Score,
};
use super::experience_rows::{
    parse_aliases, parse_attempts, parse_outcomes, parse_problems, string_list, CallerContext,
    ProblemRecord, PROBLEM_COLLECTION,
};
use super::{
    content_hash, created_at_millis, link_parent_to_agent, nullable_string, vector_literal,
    MemoryParams, MemoryStore, MAX_ALIAS_ROWS, MAX_ATTEMPTS_PER_PAGE, MAX_OUTCOMES_PER_PAGE,
    MAX_PAGE, MAX_RECALL_PROBLEMS, PROBLEM_NEIGHBORS,
};

const DEFAULT_PAGE: usize = 25;
const RRF_K: f64 = 60.0;

fn page(args: &MemoryParams) -> (usize, usize) {
    let limit = args.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let offset = args
        .cursor
        .as_deref()
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .unwrap_or(0);
    (limit, offset)
}

fn merged_keys(aliases: &[(String, String)], canonical: &str) -> Vec<String> {
    let mut merged: Vec<String> = aliases
        .iter()
        .map(|(from, _)| from.clone())
        .filter(|from| resolve(aliases, from) == canonical)
        .collect();
    merged.sort();
    merged.dedup();
    merged
}

fn citation_keys(aliases: &[(String, String)], canonical: &str) -> Vec<String> {
    let mut keys = merged_keys(aliases, canonical);
    keys.push(canonical.to_owned());
    keys.sort();
    keys.dedup();
    keys
}

fn provenance_literal(values: &Map<String, Value>) -> String {
    let fields = values
        .iter()
        .map(|(key, value)| match value {
            Value::String(text) => format!(
                "{key}: \"{}\"",
                gents_store::graphql::escape_graphql_string(text)
            ),
            other => format!("{key}: {other}"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{fields}}}")
}

fn score_json(score: Option<Score>) -> Value {
    match score {
        Some(score) => json!({"passed": score.passed, "total": score.total}),
        None => Value::Null,
    }
}

pub(super) struct RecallPage {
    pub(super) ranking: &'static str,
    pub(super) ranks: Vec<String>,
    pub(super) experience: Experience,
    pub(super) history_truncated: bool,
}

pub(super) struct NewProblem<'a> {
    pub(super) problem_key: &'a str,
    pub(super) name: &'a str,
    pub(super) normalized: &'a str,
    pub(super) vector: &'a [f32],
    pub(super) request_id: Option<&'a str>,
    pub(super) session_id: Option<&'a str>,
    pub(super) provenance: &'a str,
    pub(super) created_at: &'a str,
}

pub(super) async fn ensure_problem_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    agent_doc_id: &str,
    agent_did: &str,
    problem: &NewProblem<'_>,
) -> anyhow::Result<ProblemRecord> {
    let existing = txn
        .execute(&format!(
            "{{AgentMemoryProblem(filter: {{_and: [{{agent_did: {{_eq: \"{agent}\"}}}}, {{problem_key: {{_eq: \"{key}\"}}}}]}}, limit: 1) {{ problem_key name carried_from created_at }}}}",
            agent = gents_store::graphql::escape_graphql_string(agent_did),
            key = gents_store::graphql::escape_graphql_string(problem.problem_key),
        ))
        .await?;
    if let Some(record) = parse_problems(&existing).into_iter().next() {
        return Ok(record);
    }
    let mutation = format!(
        "mutation {{ add_AgentMemoryProblem(input: {{ problem_key: \"{problem_key}\", agent_did: \"{agent_did}\", name: \"{name}\", normalized_name: \"{normalized}\", embedding: [{literal}], embed_model_id: {embed_model_id}, opened_request_id: {request_id}, opened_session_id: {session_id}, provenance: {provenance}, created_at: \"{created_at}\" }}) {{ _docID }} }}",
        problem_key = gents_store::graphql::escape_graphql_string(problem.problem_key),
        agent_did = gents_store::graphql::escape_graphql_string(agent_did),
        name = gents_store::graphql::escape_graphql_string(problem.name),
        normalized = gents_store::graphql::escape_graphql_string(problem.normalized),
        created_at = gents_store::graphql::escape_graphql_string(problem.created_at),
        provenance = problem.provenance,
        literal = vector_literal(problem.vector),
        embed_model_id = nullable_string(Some(gents_memory_embed::MODEL_ID)),
        request_id = nullable_string(problem.request_id),
        session_id = nullable_string(problem.session_id),
    );
    let created = txn.execute(&mutation).await?;
    let doc_id = created
        .pointer(&format!("/data/add_{PROBLEM_COLLECTION}/0/_docID"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("problem write returned no document id"))?;
    link_parent_to_agent(txn, PROBLEM_COLLECTION, doc_id, agent_doc_id).await?;
    Ok(ProblemRecord {
        problem_key: problem.problem_key.to_owned(),
        name: problem.name.to_owned(),
        carried_from: None,
        created_at: problem.created_at.to_owned(),
    })
}

impl MemoryStore<'_> {
    pub(super) async fn caller_context(
        &self,
        request_id: Option<&str>,
        session_id: Option<&str>,
    ) -> anyhow::Result<CallerContext> {
        let mut values: Map<String, Value> = Map::new();
        let mut executed_model_name = None;
        let mut session_id = session_id.map(str::to_owned);
        if let Some(request_id) = request_id {
            let read = self
                .read(&format!(
                    "{{AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}, limit: 1) {{ executed_model_name executed_reasoning_effort executed_inference_profile_id executed_runtime_generation workspace_id session_id }}}}",
                    gents_store::graphql::escape_graphql_string(request_id),
                ))
                .await;
            match read {
                Ok(value) => {
                    if let Some(row) = value.pointer("/data/AgentRequest/0") {
                        for field in [
                            "executed_model_name",
                            "executed_inference_profile_id",
                            "executed_reasoning_effort",
                        ] {
                            if let Some(text) = row.get(field).and_then(Value::as_str) {
                                values.insert(field.to_owned(), Value::String(text.to_owned()));
                                if field == "executed_model_name" {
                                    executed_model_name = Some(text.to_owned());
                                }
                            }
                        }
                        if let Some(generation) = row
                            .get("executed_runtime_generation")
                            .and_then(Value::as_i64)
                        {
                            values.insert(
                                "executed_runtime_generation".to_owned(),
                                json!(generation),
                            );
                        }
                        if let Some(workspace) = row.get("workspace_id").and_then(Value::as_str) {
                            values.insert(
                                "workspace_id".to_owned(),
                                Value::String(workspace.to_owned()),
                            );
                        }
                        if let Some(session) = row.get("session_id").and_then(Value::as_str) {
                            session_id = Some(session.to_owned());
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "memory provenance request read failed");
                }
            }
            values.insert(
                "request_id".to_owned(),
                Value::String(request_id.to_owned()),
            );
        }
        if let Some(session) = session_id.as_deref() {
            values.insert("session_id".to_owned(), Value::String(session.to_owned()));
        }
        Ok(CallerContext {
            request_id: request_id.map(str::to_owned),
            session_id,
            executed_model_name,
            provenance: provenance_literal(&values),
        })
    }

    fn reader(&self, context: &CallerContext) -> Current {
        Current {
            agent_did: self.agent_did.to_owned(),
            executed_model_name: context.executed_model_name.clone().unwrap_or_default(),
        }
    }

    async fn aliases(&self) -> anyhow::Result<Vec<(String, String)>> {
        let value = self
            .read(&format!(
                "{{AgentMemoryAlias(filter: {{agent_did: {{_eq: \"{}\"}}}}, limit: {}) {{ from_problem_key to_problem_key }}}}",
                gents_store::graphql::escape_graphql_string(self.agent_did),
                MAX_ALIAS_ROWS + 1,
            ))
            .await?;
        parse_aliases(&value)
    }

    async fn problems_by_key(&self, keys: &[String]) -> anyhow::Result<Vec<ProblemRecord>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let value = self
            .read(&format!(
                "{{AgentMemoryProblem(filter: {{_and: [{{agent_did: {{_eq: \"{}\"}}}}, {{problem_key: {{_in: {}}}}}]}}) {{ problem_key name carried_from created_at }}}}",
                gents_store::graphql::escape_graphql_string(self.agent_did),
                string_list(keys),
            ))
            .await?;
        Ok(parse_problems(&value))
    }

    async fn attempts_for_keys(
        &self,
        keys: &[String],
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<AttemptRow>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let value = self
            .read(&format!(
                "{{AgentMemoryAttempt(filter: {{_and: [{{agent_did: {{_eq: \"{}\"}}}}, {{problem_key: {{_in: {}}}}}]}}, order: [{{created_at: DESC}}], limit: {limit}, offset: {offset}) {{ attempt_key problem_key predicted accepted carried_from created_at provenance }}}}",
                gents_store::graphql::escape_graphql_string(self.agent_did),
                string_list(keys),
            ))
            .await?;
        Ok(parse_attempts(&value))
    }

    async fn outcomes_for_attempts(
        &self,
        attempt_keys: &[String],
        limit: usize,
    ) -> anyhow::Result<(Vec<OutcomeRow>, bool)> {
        if attempt_keys.is_empty() {
            return Ok((Vec::new(), false));
        }
        let value = self
            .read(&format!(
                "{{AgentMemoryOutcome(filter: {{_and: [{{agent_did: {{_eq: \"{}\"}}}}, {{attempt_key: {{_in: {}}}}}]}}, order: [{{created_at: ASC}}], limit: {}) {{ outcome_key attempt_key fixed missed passed total created_at }}}}",
                gents_store::graphql::escape_graphql_string(self.agent_did),
                string_list(attempt_keys),
                limit + 1,
            ))
            .await?;
        let mut outcomes = parse_outcomes(&value);
        let truncated = outcomes.len() > limit;
        outcomes.truncate(limit);
        Ok((outcomes, truncated))
    }

    async fn nearest_problems(&self, query: &str, exclude: &[String]) -> anyhow::Result<Value> {
        let (ranking, hits) = self
            .hybrid_problems(query, PROBLEM_NEIGHBORS + exclude.len())
            .await?;
        let hits = hits
            .into_iter()
            .filter(|hit| !exclude.iter().any(|key| key == &hit.problem_key))
            .take(PROBLEM_NEIGHBORS)
            .map(|hit| {
                json!({
                    "problem_key": hit.problem_key,
                    "name": hit.name,
                    "score": hit.score,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"ranking": ranking, "hits": hits}))
    }

    async fn hybrid_problems(
        &self,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<(&'static str, Vec<ProblemHit>)> {
        let filter = format!(
            "{{agent_did: {{_eq: \"{}\"}}}}",
            gents_store::graphql::escape_graphql_string(self.agent_did)
        );
        let bm25_query = format!(
            "{{AgentMemoryProblem(filter: {filter}, limit: {limit}, order: [{{_alias: {{_score: DESC}}}}]) {{ _docID problem_key name created_at _score: BM25(query: \"{}\", fields: [\"name\"]) }}}}",
            gents_store::graphql::escape_graphql_string(query),
        );
        let vector = self
            .embedder
            .embed_one(query.to_owned())
            .await
            .map_err(|error| anyhow::anyhow!("embedding failed: {error}"))?;
        let literal = vector_literal(&vector);
        let dense_query = format!(
            "{{AgentMemoryProblem(filter: {filter}, limit: {limit}, order: [{{_alias: {{_score: DESC}}}}]) {{ _docID problem_key name created_at _score: SIMILARITY(embedding: {{vector: [{literal}]}}) }}}}",
        );
        let (bm25, dense) = tokio::join!(self.read(&bm25_query), self.read(&dense_query));
        let bm25_rows = bm25.map(|value| problem_hits(&value)).unwrap_or_default();
        let dense_rows = dense.map(|value| problem_hits(&value)).unwrap_or_default();
        let ranking = match (bm25_rows.is_empty(), dense_rows.is_empty()) {
            (false, false) => "hybrid",
            (false, true) => "bm25",
            (true, false) => "dense",
            (true, true) => "none",
        };
        Ok((ranking, fuse_problems(bm25_rows, dense_rows, limit)))
    }

    pub(super) async fn recall_page(
        &self,
        query: &str,
        current: &Current,
    ) -> anyhow::Result<RecallPage> {
        let (ranking, hits) = self.hybrid_problems(query, MAX_RECALL_PROBLEMS).await?;
        if hits.is_empty() {
            return Ok(RecallPage {
                ranking,
                ranks: Vec::new(),
                experience: Experience {
                    problems: Vec::new(),
                    unattached: Vec::new(),
                },
                history_truncated: false,
            });
        }
        let aliases = self.aliases().await?;
        let mut ranks: Vec<String> = Vec::new();
        let mut keys: Vec<String> = Vec::new();
        for hit in hits {
            let canonical = resolve(&aliases, &hit.problem_key);
            if !ranks.contains(&canonical) {
                ranks.push(canonical.clone());
            }
            keys.extend(citation_keys(&aliases, &canonical));
        }
        keys.sort();
        keys.dedup();
        let problems = self.problems_by_key(&keys).await?;
        let mut attempts = self
            .attempts_for_keys(&keys, MAX_ATTEMPTS_PER_PAGE + 1, 0)
            .await?;
        let attempts_truncated = attempts.len() > MAX_ATTEMPTS_PER_PAGE;
        attempts.truncate(MAX_ATTEMPTS_PER_PAGE);
        let attempt_keys: Vec<String> = attempts
            .iter()
            .map(|attempt| attempt.attempt_key.clone())
            .collect();
        let (outcomes, outcomes_truncated) = self
            .outcomes_for_attempts(&attempt_keys, MAX_OUTCOMES_PER_PAGE)
            .await?;
        let experience = fold(
            current,
            &Rows {
                problems: problems.iter().map(ProblemRecord::row).collect(),
                attempts,
                outcomes,
                aliases,
            },
        );
        Ok(RecallPage {
            ranking,
            ranks,
            experience,
            history_truncated: attempts_truncated || outcomes_truncated,
        })
    }

    pub(super) async fn problem(
        &self,
        args: MemoryParams,
        runtime: Option<&gents_loop::tool_call_lifecycle::runtime::CurrentToolRuntimeContext>,
    ) -> anyhow::Result<Value> {
        let request_id = runtime.and_then(|runtime| runtime.request_id.as_deref());
        let session_id = runtime.and_then(|runtime| runtime.session_id.as_deref());
        if let Some(key) = args.problem_key.clone() {
            return self.show_problem(&key, args, request_id, session_id).await;
        }
        let name = args.name.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "problem requires name to open a problem or problem_key to show one; call memory help topic=problem"
            )
        })?;
        let normalized = normalize(name);
        if normalized.is_empty() {
            anyhow::bail!("problem name must contain non-whitespace text");
        }
        if normalized.chars().count() > 4_000 {
            anyhow::bail!("problem name exceeds 4000 characters");
        }
        let problem_key = content_hash(self.agent_did, &normalized);
        let vector = self
            .embedder
            .embed_one(name.to_owned())
            .await
            .map_err(|error| anyhow::anyhow!("embedding failed: {error}"))?;
        let context = self.caller_context(request_id, session_id).await?;
        let record = self
            .open_problem(&problem_key, name, &normalized, &vector, &context)
            .await?;
        let nearest = self
            .nearest_problems(name, std::slice::from_ref(&problem_key))
            .await?;
        Ok(json!({
            "problem_key": record.problem_key,
            "name": record.name,
            "normalized_name": normalized,
            "created_at": record.created_at,
            "nearest": nearest,
        }))
    }

    async fn open_problem(
        &self,
        problem_key: &str,
        name: &str,
        normalized: &str,
        vector: &[f32],
        context: &CallerContext,
    ) -> anyhow::Result<ProblemRecord> {
        let problem_key = problem_key.to_owned();
        let name = name.to_owned();
        let normalized = normalized.to_owned();
        let vector = vector.to_vec();
        let request_id = context.request_id.clone();
        let session_id = context.session_id.clone();
        let provenance = context.provenance.clone();
        let created_at = Utc::now().to_rfc3339();
        let node_did = self.node_did.to_owned();
        let agent_did = self.agent_did.to_owned();
        self.access
            .transact_write("tool.memory.problem", move |txn| {
                let problem_key = problem_key.clone();
                let name = name.clone();
                let normalized = normalized.clone();
                let vector = vector.clone();
                let request_id = request_id.clone();
                let session_id = session_id.clone();
                let provenance = provenance.clone();
                let created_at = created_at.clone();
                let node_did = node_did.clone();
                let agent_did = agent_did.clone();
                Box::pin(async move {
                    let (agent_doc_id, _) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Agent,
                        &node_did,
                        &agent_did,
                    )
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("the agent has no readable Agent document"))?;
                    let existing = parse_problems(
                        &txn.execute(&format!(
                            "{{AgentMemoryProblem(filter: {{_and: [{{agent_did: {{_eq: \"{agent_did}\"}}}}, {{problem_key: {{_eq: \"{problem_key}\"}}}}]}}, limit: 1) {{ problem_key name carried_from created_at }}}}",
                            agent_did = gents_store::graphql::escape_graphql_string(&agent_did),
                            problem_key = gents_store::graphql::escape_graphql_string(&problem_key),
                        ))
                        .await?,
                    )
                    .into_iter()
                    .next();
                    if let Some(record) = existing {
                        return Ok(record);
                    }
                    ensure_problem_in_txn(
                        txn,
                        &agent_doc_id,
                        &agent_did,
                        &NewProblem {
                            problem_key: &problem_key,
                            name: &name,
                            normalized: &normalized,
                            vector: &vector,
                            request_id: request_id.as_deref(),
                            session_id: session_id.as_deref(),
                            provenance: &provenance,
                            created_at: &created_at,
                        },
                    )
                    .await
                })
            })
            .await
    }

    async fn show_problem(
        &self,
        requested_key: &str,
        args: MemoryParams,
        request_id: Option<&str>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Value> {
        let aliases = self.aliases().await?;
        let canonical = resolve(&aliases, requested_key);
        let keys = citation_keys(&aliases, &canonical);
        let problem_rows = self.problems_by_key(&keys).await?;
        let Some(record) = problem_rows
            .iter()
            .find(|record| record.problem_key == canonical)
            .cloned()
        else {
            anyhow::bail!(
                "no problem {requested_key}; call memory problem with name to open one or memory problems to list the index"
            );
        };
        let nearest = self.nearest_problems(&record.name, &keys).await?;
        let (limit, offset) = page(&args);
        let attempts = self.attempts_for_keys(&keys, limit + 1, offset).await?;
        let attempts_truncated = attempts.len() > limit;
        let attempts: Vec<AttemptRow> = attempts.into_iter().take(limit).collect();
        let attempt_keys: Vec<String> = attempts
            .iter()
            .map(|attempt| attempt.attempt_key.clone())
            .collect();
        let (outcomes, outcomes_truncated) = self
            .outcomes_for_attempts(&attempt_keys, MAX_OUTCOMES_PER_PAGE)
            .await?;
        let context = self.caller_context(request_id, session_id).await?;
        let experience = fold(
            &self.reader(&context),
            &Rows {
                problems: problem_rows.iter().map(ProblemRecord::row).collect(),
                attempts: attempts.clone(),
                outcomes,
                aliases: aliases.clone(),
            },
        );
        let problem = experience
            .problems
            .into_iter()
            .find(|problem| problem.problem_key == canonical)
            .unwrap_or(FoldedProblem {
                problem_key: canonical.clone(),
                name: Some(record.name.clone()),
                carried_from: record.carried_from.clone(),
                merged: merged_keys(&aliases, &canonical),
                attempts: Vec::new(),
                latest: None,
                last_active: 0,
            });
        let next_cursor = (attempts.len() == limit).then(|| (offset + attempts.len()).to_string());
        Ok(json!({
            "problem_key": problem.problem_key,
            "requested_key": requested_key,
            "name": problem.name,
            "carried_from": problem.carried_from,
            "merged": merged_keys(&aliases, &canonical),
            "attempts": problem.attempts,
            "latest": score_json(problem.latest),
            "unresolved": unresolved_count(&problem),
            "last_active": problem.last_active,
            "nearest": nearest,
            "attempts_truncated": attempts_truncated,
            "next_cursor": next_cursor,
            "outcomes_truncated": outcomes_truncated,
        }))
    }

    pub(super) async fn problems(
        &self,
        args: MemoryParams,
        runtime: Option<&gents_loop::tool_call_lifecycle::runtime::CurrentToolRuntimeContext>,
    ) -> anyhow::Result<Value> {
        let aliases = self.aliases().await?;
        let (limit, offset) = page(&args);
        let value = self
            .read(&format!(
                "{{AgentMemoryProblem(filter: {{agent_did: {{_eq: \"{}\"}}}}, order: [{{created_at: DESC}}], limit: {limit}, offset: {offset}) {{ problem_key name carried_from created_at }}}}",
                gents_store::graphql::escape_graphql_string(self.agent_did),
            ))
            .await?;
        let rows = parse_problems(&value);
        let next_cursor = (rows.len() == limit).then(|| (offset + rows.len()).to_string());
        let canonical_rows: Vec<ProblemRecord> = rows
            .into_iter()
            .filter(|record| resolve(&aliases, &record.problem_key) == record.problem_key)
            .collect();
        let mut keys = Vec::new();
        for record in &canonical_rows {
            keys.extend(citation_keys(&aliases, &record.problem_key));
        }
        keys.sort();
        keys.dedup();
        let attempts = self
            .attempts_for_keys(&keys, MAX_ATTEMPTS_PER_PAGE + 1, 0)
            .await?;
        let attempts_truncated = attempts.len() > MAX_ATTEMPTS_PER_PAGE;
        let attempts: Vec<AttemptRow> = attempts.into_iter().take(MAX_ATTEMPTS_PER_PAGE).collect();
        let attempt_keys: Vec<String> = attempts
            .iter()
            .map(|attempt| attempt.attempt_key.clone())
            .collect();
        let (outcomes, outcomes_truncated) = self
            .outcomes_for_attempts(&attempt_keys, MAX_OUTCOMES_PER_PAGE)
            .await?;
        let context = self
            .caller_context(
                runtime.and_then(|runtime| runtime.request_id.as_deref()),
                runtime.and_then(|runtime| runtime.session_id.as_deref()),
            )
            .await?;
        let experience = fold(
            &self.reader(&context),
            &Rows {
                problems: canonical_rows.iter().map(ProblemRecord::row).collect(),
                attempts,
                outcomes,
                aliases: aliases.clone(),
            },
        );
        let entries: Vec<Value> = canonical_rows
            .iter()
            .map(|record| {
                let problem = experience
                    .problems
                    .iter()
                    .find(|problem| problem.problem_key == record.problem_key);
                let (count, latest, unresolved, flags, last_active) = match problem {
                    Some(problem) => (
                        problem.attempts.len(),
                        problem.latest,
                        unresolved_count(problem),
                        flags(problem)
                            .iter()
                            .map(|relation| Value::String(relation.text().to_owned()))
                            .collect::<Vec<_>>(),
                        problem.last_active,
                    ),
                    None => (
                        0,
                        None,
                        0,
                        Vec::new(),
                        created_at_millis(&record.created_at),
                    ),
                };
                json!({
                    "problem_key": record.problem_key,
                    "name": record.name,
                    "merged": merged_keys(&aliases, &record.problem_key),
                    "attempts": count,
                    "latest": score_json(latest),
                    "unresolved": unresolved,
                    "provenance_flags": flags,
                    "last_active": last_active,
                })
            })
            .collect();
        Ok(json!({
            "problems": entries,
            "next_cursor": next_cursor,
            "attempts_truncated": attempts_truncated,
            "outcomes_truncated": outcomes_truncated,
        }))
    }
}

struct ProblemHit {
    problem_key: String,
    name: String,
    score: f64,
}

fn problem_hits(value: &Value) -> Vec<(String, String)> {
    value
        .pointer("/data/AgentMemoryProblem")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some((
                        row.get("problem_key")?.as_str()?.to_owned(),
                        row.get("name")?.as_str()?.to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn fuse_problems(
    bm25: Vec<(String, String)>,
    dense: Vec<(String, String)>,
    limit: usize,
) -> Vec<ProblemHit> {
    let mut fused: Vec<ProblemHit> = Vec::new();
    for (rank, (key, name)) in bm25.into_iter().enumerate() {
        let score = 1.0 / (RRF_K + (rank + 1) as f64);
        match fused.iter_mut().find(|hit| hit.problem_key == key) {
            Some(hit) => hit.score += score,
            None => fused.push(ProblemHit {
                problem_key: key,
                name,
                score,
            }),
        }
    }
    for (rank, (key, name)) in dense.into_iter().enumerate() {
        let score = 1.0 / (RRF_K + (rank + 1) as f64);
        match fused.iter_mut().find(|hit| hit.problem_key == key) {
            Some(hit) => hit.score += score,
            None => fused.push(ProblemHit {
                problem_key: key,
                name,
                score,
            }),
        }
    }
    fused.sort_by(|first, second| {
        second
            .score
            .total_cmp(&first.score)
            .then_with(|| first.problem_key.cmp(&second.problem_key))
    });
    fused.truncate(limit);
    fused
}

impl PartialOrd for ProblemHit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ProblemHit {
    fn cmp(&self, other: &Self) -> Ordering {
        self.problem_key.cmp(&other.problem_key)
    }
}

impl PartialEq for ProblemHit {
    fn eq(&self, other: &Self) -> bool {
        self.problem_key == other.problem_key
    }
}

impl Eq for ProblemHit {}
