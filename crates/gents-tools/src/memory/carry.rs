use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use gents_memory_embed::{DIMENSIONS, MODEL_ID};
use gents_store::config_client::TransactionFailure;
use serde_json::{json, Value};

use super::experience::{normalize, problem_key};
use super::experience_rows::{
    string_list, ALIAS_COLLECTION, ATTEMPT_COLLECTION, OUTCOME_COLLECTION, PROBLEM_COLLECTION,
};
use super::{
    content_hash, digest_key, nullable_string, vector_literal, MemoryParams, MemoryStore, MAX_PAGE,
    MEMORY_COLLECTION,
};

const DEFAULT_CARRY_LIMIT: usize = 25;

struct SourceProblem {
    problem_key: String,
    name: String,
    provenance: Value,
    opened_request_id: Option<String>,
    opened_session_id: Option<String>,
    created_at: String,
}

struct SourceAttempt {
    attempt_key: String,
    problem_key: String,
    what: String,
    rationale: String,
    targets: Value,
    predicted: Value,
    accepted: bool,
    provenance: Value,
    request_id: Option<String>,
    session_id: Option<String>,
    created_at: String,
}

struct SourceOutcome {
    outcome_key: String,
    attempt_key: String,
    fixed: Value,
    missed: Value,
    passed: Option<i64>,
    total: Option<i64>,
    note: String,
    provenance: Value,
    request_id: Option<String>,
    created_at: String,
}

struct SourceAlias {
    from_problem_key: String,
    to_problem_key: String,
    request_id: Option<String>,
    created_at: String,
}

struct SourceFact {
    text: String,
    kind: String,
    source: String,
    metadata: Value,
    created_at: String,
}

struct CarryPage {
    problems: Vec<SourceProblem>,
    attempts: Vec<SourceAttempt>,
    outcomes: Vec<SourceOutcome>,
    aliases: Vec<SourceAlias>,
    facts: Vec<SourceFact>,
    more: bool,
}

impl SourceProblem {
    fn parse(row: &Value) -> Option<Self> {
        Some(Self {
            problem_key: row.get("problem_key")?.as_str()?.to_owned(),
            name: row.get("name")?.as_str()?.to_owned(),
            provenance: field(row, "provenance"),
            opened_request_id: optional_string(row, "opened_request_id"),
            opened_session_id: optional_string(row, "opened_session_id"),
            created_at: timestamp(row),
        })
    }
}

impl SourceAttempt {
    fn parse(row: &Value) -> Option<Self> {
        Some(Self {
            attempt_key: row.get("attempt_key")?.as_str()?.to_owned(),
            problem_key: row.get("problem_key")?.as_str()?.to_owned(),
            what: string(row, "what"),
            rationale: string(row, "rationale"),
            targets: field(row, "targets"),
            predicted: field(row, "predicted"),
            accepted: row.get("accepted").and_then(Value::as_bool).unwrap_or(true),
            provenance: field(row, "provenance"),
            request_id: optional_string(row, "request_id"),
            session_id: optional_string(row, "session_id"),
            created_at: timestamp(row),
        })
    }
}

impl SourceOutcome {
    fn parse(row: &Value) -> Option<Self> {
        Some(Self {
            outcome_key: row.get("outcome_key")?.as_str()?.to_owned(),
            attempt_key: row.get("attempt_key")?.as_str()?.to_owned(),
            fixed: field(row, "fixed"),
            missed: field(row, "missed"),
            passed: row.get("passed").and_then(Value::as_i64),
            total: row.get("total").and_then(Value::as_i64),
            note: string(row, "note"),
            provenance: field(row, "provenance"),
            request_id: optional_string(row, "request_id"),
            created_at: timestamp(row),
        })
    }
}

impl SourceAlias {
    fn parse(row: &Value) -> Option<Self> {
        Some(Self {
            from_problem_key: row.get("from_problem_key")?.as_str()?.to_owned(),
            to_problem_key: row.get("to_problem_key")?.as_str()?.to_owned(),
            request_id: optional_string(row, "request_id"),
            created_at: timestamp(row),
        })
    }
}

impl SourceFact {
    fn parse(row: &Value) -> Option<Self> {
        Some(Self {
            text: row.get("text")?.as_str()?.to_owned(),
            kind: string(row, "kind"),
            source: string(row, "source"),
            metadata: field(row, "metadata"),
            created_at: timestamp(row),
        })
    }
}

fn string(row: &Value, field: &str) -> String {
    row.get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn optional_string(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn nullable_int(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_owned())
}

fn field(row: &Value, name: &str) -> Value {
    row.get(name).cloned().unwrap_or(Value::Null)
}

fn timestamp(row: &Value) -> String {
    row.get("created_at")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| Utc::now().to_rfc3339())
}

fn parse_array<T>(value: &Value, collection: &str, parse: impl Fn(&Value) -> Option<T>) -> Vec<T> {
    value
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter_map(parse).collect())
        .unwrap_or_default()
}

fn escape(value: &str) -> String {
    gents_store::graphql::escape_graphql_string(value)
}

fn limit(args: &MemoryParams) -> usize {
    args.limit.unwrap_or(DEFAULT_CARRY_LIMIT).clamp(1, MAX_PAGE)
}

fn offset(args: &MemoryParams) -> usize {
    args.cursor
        .as_deref()
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .unwrap_or(0)
}

fn source_clauses(source: &str, source_doc: &str) -> String {
    format!(
        "{{agent_did: {{_eq: \"{}\"}}}}, {{agent_doc_id: {{_eq: \"{}\"}}}}",
        escape(source),
        escape(source_doc),
    )
}

fn source_filter(source: &str, source_doc: &str) -> String {
    format!("{{_and: [{}]}}", source_clauses(source, source_doc))
}

fn source_problems_query(source: &str, source_doc: &str, limit: usize, offset: usize) -> String {
    format!(
        "{{AgentMemoryProblem(filter: {}, order: [{{problem_key: ASC}}], limit: {limit}, offset: {offset}) {{ problem_key name provenance opened_request_id opened_session_id created_at }}}}",
        source_filter(source, source_doc),
    )
}

fn source_attempts_query(source: &str, source_doc: &str, limit: usize, offset: usize) -> String {
    format!(
        "{{AgentMemoryAttempt(filter: {}, order: [{{attempt_key: ASC}}], limit: {limit}, offset: {offset}) {{ attempt_key problem_key what rationale targets predicted accepted provenance request_id session_id created_at }}}}",
        source_filter(source, source_doc),
    )
}

fn source_outcomes_query(source: &str, source_doc: &str, limit: usize, offset: usize) -> String {
    format!(
        "{{AgentMemoryOutcome(filter: {}, order: [{{outcome_key: ASC}}], limit: {limit}, offset: {offset}) {{ outcome_key attempt_key fixed missed passed total note provenance request_id created_at }}}}",
        source_filter(source, source_doc),
    )
}

fn source_aliases_query(source: &str, source_doc: &str, limit: usize, offset: usize) -> String {
    format!(
        "{{AgentMemoryAlias(filter: {}, order: [{{from_problem_key: ASC}}], limit: {limit}, offset: {offset}) {{ from_problem_key to_problem_key request_id created_at }}}}",
        source_filter(source, source_doc),
    )
}

fn source_facts_query(source: &str, source_doc: &str, limit: usize, offset: usize) -> String {
    format!(
        "{{{MEMORY_COLLECTION}(filter: {}, order: [{{memory_key: ASC}}], limit: {limit}, offset: {offset}) {{ text kind source metadata created_at }}}}",
        source_filter(source, source_doc),
    )
}

fn source_names_query(source: &str, source_doc: &str, keys: &[String]) -> String {
    format!(
        "{{AgentMemoryProblem(filter: {{_and: [{}, {{problem_key: {{_in: {}}}}}]}}) {{ problem_key name }}}}",
        source_clauses(source, source_doc),
        string_list(keys),
    )
}

fn json_literal(value: &Value) -> anyhow::Result<String> {
    Ok(gents_protocol::graphql::graphql_input_literal(value)?)
}

async fn read_page(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    source: &str,
    source_doc: &str,
    page_limit: usize,
    page_offset: usize,
) -> anyhow::Result<CarryPage> {
    let probe = page_limit + 1;
    let problems_query = source_problems_query(source, source_doc, probe, page_offset);
    let attempts_query = source_attempts_query(source, source_doc, probe, page_offset);
    let outcomes_query = source_outcomes_query(source, source_doc, probe, page_offset);
    let aliases_query = source_aliases_query(source, source_doc, probe, page_offset);
    let facts_query = source_facts_query(source, source_doc, probe, page_offset);
    let (problems, attempts, outcomes, aliases, facts) = tokio::try_join!(
        txn.execute(&problems_query),
        txn.execute(&attempts_query),
        txn.execute(&outcomes_query),
        txn.execute(&aliases_query),
        txn.execute(&facts_query),
    )?;
    let mut problems = parse_array(&problems, PROBLEM_COLLECTION, SourceProblem::parse);
    let mut attempts = parse_array(&attempts, ATTEMPT_COLLECTION, SourceAttempt::parse);
    let mut outcomes = parse_array(&outcomes, OUTCOME_COLLECTION, SourceOutcome::parse);
    let mut aliases = parse_array(&aliases, ALIAS_COLLECTION, SourceAlias::parse);
    let mut facts = parse_array(&facts, MEMORY_COLLECTION, SourceFact::parse);
    let more = [
        problems.len(),
        attempts.len(),
        outcomes.len(),
        aliases.len(),
        facts.len(),
    ]
    .into_iter()
    .any(|count| count > page_limit);
    problems.truncate(page_limit);
    attempts.truncate(page_limit);
    outcomes.truncate(page_limit);
    aliases.truncate(page_limit);
    facts.truncate(page_limit);
    Ok(CarryPage {
        problems,
        attempts,
        outcomes,
        aliases,
        facts,
        more,
    })
}

async fn translation(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    caller: &str,
    source: &str,
    source_doc: &str,
    page: &CarryPage,
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut translated = BTreeMap::new();
    for problem in &page.problems {
        translated.insert(
            problem.problem_key.clone(),
            problem_key(caller, &problem.name),
        );
    }
    let mut needed = BTreeSet::new();
    for attempt in &page.attempts {
        if !translated.contains_key(&attempt.problem_key) {
            needed.insert(attempt.problem_key.clone());
        }
    }
    for alias in &page.aliases {
        for key in [&alias.from_problem_key, &alias.to_problem_key] {
            if !translated.contains_key(key) {
                needed.insert(key.clone());
            }
        }
    }
    if !needed.is_empty() {
        let keys: Vec<String> = needed.into_iter().collect();
        let value = txn
            .execute(&source_names_query(source, source_doc, &keys))
            .await?;
        for problem in parse_array(&value, PROBLEM_COLLECTION, SourceProblem::parse) {
            translated.insert(problem.problem_key, problem_key(caller, &problem.name));
        }
    }
    for attempt in &page.attempts {
        anyhow::ensure!(
            translated.contains_key(&attempt.problem_key),
            "source attempt {} cites problem {}, which is not readable from that agent; keep the problem row, or carry the problem page again",
            attempt.attempt_key,
            attempt.problem_key
        );
    }
    for alias in &page.aliases {
        for key in [&alias.from_problem_key, &alias.to_problem_key] {
            anyhow::ensure!(
                translated.contains_key(key),
                "source alias {} -> {} names problem {key}, which is not readable from that agent; keep the problem row, or carry the problem page again",
                alias.from_problem_key,
                alias.to_problem_key
            );
        }
    }
    Ok(translated)
}

async fn existing_keys(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    collection: &str,
    caller: &str,
    caller_doc: &str,
    field: &str,
    keys: &[String],
) -> anyhow::Result<BTreeSet<String>> {
    if keys.is_empty() {
        return Ok(BTreeSet::new());
    }
    let value = txn
        .execute(&format!(
            "{{{collection}(filter: {{_and: [{{agent_did: {{_eq: \"{}\"}}}}, {{agent_doc_id: {{_eq: \"{}\"}}}}, {{{field}: {{_in: {}}}}}]}}) {{{field}}}}}",
            escape(caller),
            escape(caller_doc),
            string_list(keys),
        ))
        .await?;
    Ok(value
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get(field).and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

async fn insert(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    collection: &str,
    mutation: &str,
) -> anyhow::Result<()> {
    let created = txn.execute(mutation).await?;
    created
        .pointer(&format!("/data/add_{collection}/0/_docID"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{collection} carry write returned no document id"))?;
    Ok(())
}

async fn copy_problems(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    store: &MemoryStore<'_>,
    caller: &str,
    caller_doc: &str,
    source: &str,
    page: &CarryPage,
    translated: &BTreeMap<String, String>,
) -> anyhow::Result<usize> {
    let mut wanted: BTreeMap<String, &SourceProblem> = BTreeMap::new();
    for problem in &page.problems {
        let key = translated
            .get(&problem.problem_key)
            .cloned()
            .unwrap_or_else(|| problem_key(caller, &problem.name));
        wanted.entry(key).or_insert(problem);
    }
    let keys: Vec<String> = wanted.keys().cloned().collect();
    let existing = existing_keys(
        txn,
        PROBLEM_COLLECTION,
        caller,
        caller_doc,
        "problem_key",
        &keys,
    )
    .await?;
    let mut missing: Vec<(String, &SourceProblem)> = wanted
        .into_iter()
        .filter(|(key, _)| !existing.contains(key))
        .collect();
    if missing.is_empty() {
        return Ok(0);
    }
    let names: Vec<String> = missing.iter().map(|(_, p)| p.name.clone()).collect();
    let vectors = store
        .embedder
        .embed(names)
        .await
        .map_err(|error| anyhow::anyhow!("embedding failed: {error}"))?;
    anyhow::ensure!(
        vectors.len() == missing.len(),
        "embedding returned {} vectors for {} problem names",
        vectors.len(),
        missing.len()
    );
    let mut stored = 0;
    for ((key, problem), vector) in missing.drain(..).zip(vectors) {
        anyhow::ensure!(
            vector.len() == DIMENSIONS,
            "embedding width {} does not match the collection's {DIMENSIONS}",
            vector.len()
        );
        let mutation = format!(
            "mutation {{ add_AgentMemoryProblem(input: {{ problem_key: \"{key}\", agent_did: \"{caller}\", agent_doc_id: \"{caller_doc}\", name: \"{name}\", normalized_name: \"{normalized}\", embedding: [{vector}], embed_model_id: \"{model}\", opened_request_id: {request}, opened_session_id: {session}, provenance: {provenance}, carried_from: \"{source}\", source_problem_key: \"{source_key}\", created_at: \"{created_at}\" }}) {{ _docID }} }}",
            key = escape(&key),
            caller = escape(caller),
            caller_doc = escape(caller_doc),
            name = escape(&problem.name),
            normalized = escape(&normalize(&problem.name)),
            vector = vector_literal(&vector),
            model = escape(MODEL_ID),
            request = nullable_string(problem.opened_request_id.as_deref()),
            session = nullable_string(problem.opened_session_id.as_deref()),
            provenance = json_literal(&problem.provenance)?,
            source = escape(source),
            source_key = escape(&problem.problem_key),
            created_at = escape(&problem.created_at),
        );
        insert(txn, PROBLEM_COLLECTION, &mutation).await?;
        stored += 1;
    }
    Ok(stored)
}

async fn copy_attempts(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    caller: &str,
    caller_doc: &str,
    source: &str,
    page: &CarryPage,
    translated: &BTreeMap<String, String>,
) -> anyhow::Result<usize> {
    let mut rows = Vec::new();
    for attempt in &page.attempts {
        let key = digest_key(&[caller, source, &attempt.attempt_key]);
        let problem = translated
            .get(&attempt.problem_key)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("source problem {} was not translated", attempt.problem_key)
            })?;
        rows.push((key, problem, attempt));
    }
    let keys: Vec<String> = rows.iter().map(|(key, _, _)| key.clone()).collect();
    let existing = existing_keys(
        txn,
        ATTEMPT_COLLECTION,
        caller,
        caller_doc,
        "attempt_key",
        &keys,
    )
    .await?;
    let mut stored = 0;
    for (key, problem, attempt) in rows {
        if existing.contains(&key) {
            continue;
        }
        let mutation = format!(
            "mutation {{ add_AgentMemoryAttempt(input: {{ attempt_key: \"{key}\", agent_did: \"{caller}\", agent_doc_id: \"{caller_doc}\", problem_key: \"{problem}\", what: \"{what}\", rationale: \"{rationale}\", targets: {targets}, predicted: {predicted}, accepted: {accepted}, provenance: {provenance}, carried_from: \"{source}\", source_attempt_key: \"{source_key}\", request_id: {request}, session_id: {session}, created_at: \"{created_at}\" }}) {{ _docID }} }}",
            key = escape(&key),
            caller = escape(caller),
            caller_doc = escape(caller_doc),
            problem = escape(&problem),
            what = escape(&attempt.what),
            rationale = escape(&attempt.rationale),
            targets = json_literal(&attempt.targets)?,
            predicted = json_literal(&attempt.predicted)?,
            accepted = attempt.accepted,
            provenance = json_literal(&attempt.provenance)?,
            source = escape(source),
            source_key = escape(&attempt.attempt_key),
            request = nullable_string(attempt.request_id.as_deref()),
            session = nullable_string(attempt.session_id.as_deref()),
            created_at = escape(&attempt.created_at),
        );
        insert(txn, ATTEMPT_COLLECTION, &mutation).await?;
        stored += 1;
    }
    Ok(stored)
}

async fn copy_outcomes(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    caller: &str,
    caller_doc: &str,
    source: &str,
    page: &CarryPage,
) -> anyhow::Result<usize> {
    let mut rows = Vec::new();
    for outcome in &page.outcomes {
        let key = digest_key(&[caller, source, &outcome.outcome_key]);
        let attempt = digest_key(&[caller, source, &outcome.attempt_key]);
        rows.push((key, attempt, outcome));
    }
    let keys: Vec<String> = rows.iter().map(|(key, _, _)| key.clone()).collect();
    let existing = existing_keys(
        txn,
        OUTCOME_COLLECTION,
        caller,
        caller_doc,
        "outcome_key",
        &keys,
    )
    .await?;
    let mut stored = 0;
    for (key, attempt, outcome) in rows {
        if existing.contains(&key) {
            continue;
        }
        let mutation = format!(
            "mutation {{ add_AgentMemoryOutcome(input: {{ outcome_key: \"{key}\", agent_did: \"{caller}\", agent_doc_id: \"{caller_doc}\", attempt_key: \"{attempt}\", fixed: {fixed}, missed: {missed}, passed: {passed}, total: {total}, note: \"{note}\", provenance: {provenance}, carried_from: \"{source}\", source_outcome_key: \"{source_key}\", request_id: {request}, created_at: \"{created_at}\" }}) {{ _docID }} }}",
            key = escape(&key),
            caller = escape(caller),
            caller_doc = escape(caller_doc),
            attempt = escape(&attempt),
            fixed = json_literal(&outcome.fixed)?,
            missed = json_literal(&outcome.missed)?,
            passed = nullable_int(outcome.passed),
            total = nullable_int(outcome.total),
            note = escape(&outcome.note),
            provenance = json_literal(&outcome.provenance)?,
            source = escape(source),
            source_key = escape(&outcome.outcome_key),
            request = nullable_string(outcome.request_id.as_deref()),
            created_at = escape(&outcome.created_at),
        );
        insert(txn, OUTCOME_COLLECTION, &mutation).await?;
        stored += 1;
    }
    Ok(stored)
}

async fn copy_aliases(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    caller: &str,
    caller_doc: &str,
    page: &CarryPage,
    translated: &BTreeMap<String, String>,
) -> anyhow::Result<usize> {
    let mut rows = Vec::new();
    for alias in &page.aliases {
        let from = translated
            .get(&alias.from_problem_key)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "source problem {} was not translated",
                    alias.from_problem_key
                )
            })?;
        let to = translated
            .get(&alias.to_problem_key)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("source problem {} was not translated", alias.to_problem_key)
            })?;
        if from == to {
            continue;
        }
        rows.push((from, to, alias));
    }
    let keys: Vec<String> = rows.iter().map(|(from, _, _)| from.clone()).collect();
    let existing = existing_keys(
        txn,
        ALIAS_COLLECTION,
        caller,
        caller_doc,
        "from_problem_key",
        &keys,
    )
    .await?;
    let mut stored = 0;
    for (from, to, alias) in rows {
        if existing.contains(&from) {
            continue;
        }
        let mutation = format!(
            "mutation {{ add_AgentMemoryAlias(input: {{ from_problem_key: \"{from}\", agent_did: \"{caller}\", agent_doc_id: \"{caller_doc}\", to_problem_key: \"{to}\", request_id: {request}, created_at: \"{created_at}\" }}) {{ _docID }} }}",
            from = escape(&from),
            caller = escape(caller),
            caller_doc = escape(caller_doc),
            to = escape(&to),
            request = nullable_string(alias.request_id.as_deref()),
            created_at = escape(&alias.created_at),
        );
        insert(txn, ALIAS_COLLECTION, &mutation).await?;
        stored += 1;
    }
    Ok(stored)
}

async fn copy_facts(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    store: &MemoryStore<'_>,
    caller: &str,
    caller_doc: &str,
    page: &CarryPage,
) -> anyhow::Result<usize> {
    let mut rows = Vec::new();
    for fact in &page.facts {
        if fact.text.trim().is_empty() {
            continue;
        }
        let hash = content_hash(caller, &fact.text);
        rows.push((hash, fact));
    }
    let hashes: Vec<String> = rows.iter().map(|(hash, _)| hash.clone()).collect();
    let existing = existing_keys(
        txn,
        MEMORY_COLLECTION,
        caller,
        caller_doc,
        "content_hash",
        &hashes,
    )
    .await?;
    let mut missing: Vec<(String, &SourceFact)> = rows
        .into_iter()
        .filter(|(hash, _)| !existing.contains(hash))
        .collect();
    if missing.is_empty() {
        return Ok(0);
    }
    let texts: Vec<String> = missing.iter().map(|(_, f)| f.text.clone()).collect();
    let vectors = store
        .embedder
        .embed(texts)
        .await
        .map_err(|error| anyhow::anyhow!("embedding failed: {error}"))?;
    anyhow::ensure!(
        vectors.len() == missing.len(),
        "embedding returned {} vectors for {} facts",
        vectors.len(),
        missing.len()
    );
    let node_did = store.node_did;
    let mut stored = 0;
    for ((hash, fact), vector) in missing.drain(..).zip(vectors) {
        anyhow::ensure!(
            vector.len() == DIMENSIONS,
            "embedding width {} does not match the collection's {DIMENSIONS}",
            vector.len()
        );
        let memory_key = format!("{caller}:{hash}");
        let mutation = format!(
            "mutation {{ add_AgentMemory(input: {{ memory_key: \"{key}\", node_did: \"{node}\", agent_did: \"{caller}\", agent_doc_id: \"{caller_doc}\", session_id: null, session_doc_id: null, request_doc_id: null, requester_did: null, creator_did: \"{caller}\", source: \"{source}\", kind: \"{kind}\", text: \"{text}\", embedding: [{vector}], content_hash: \"{hash}\", embed_model_id: \"{model}\", created_at: \"{created_at}\", metadata: {metadata} }}) {{ _docID }} }}",
            key = escape(&memory_key),
            node = escape(node_did),
            caller = escape(caller),
            caller_doc = escape(caller_doc),
            source = escape(&fact.source),
            kind = escape(&fact.kind),
            text = escape(&fact.text),
            vector = vector_literal(&vector),
            hash = escape(&hash),
            model = escape(MODEL_ID),
            created_at = escape(&fact.created_at),
            metadata = json_literal(&fact.metadata)?,
        );
        insert(txn, MEMORY_COLLECTION, &mutation).await?;
        stored += 1;
    }
    Ok(stored)
}

impl MemoryStore<'_> {
    pub(super) async fn share(&self, args: MemoryParams, revoke: bool) -> anyhow::Result<Value> {
        let verb = if revoke { "unshare" } else { "share" };
        let other = args
            .to_agent
            .as_deref()
            .map(str::trim)
            .filter(|other| !other.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{verb} requires to_agent: the agent DID to {verb}; call config agent list to find it"
                )
            })?
            .to_owned();
        let other_did = identity::Did::new(&other).map_err(|error| {
            anyhow::anyhow!("to_agent must be an agent DID, so {verb} cannot name {other}: {error}")
        })?;
        anyhow::ensure!(
            other != self.agent_did,
            "{verb} changes what another agent may read; this agent already reads its own memory, so pass to_agent naming a different agent"
        );
        let caller = self.agent_did.to_owned();
        let node_did = self.node_did.to_owned();
        let next_call = format!("memory carry from_agent={caller}");
        self.access
            .transact_write("tool.memory.share", move |txn| {
                let caller = caller.clone();
                let node_did = node_did.clone();
                let other_did = other_did.clone();
                Box::pin(async move {
                    let (caller_doc, _) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Agent,
                        &node_did,
                        &caller,
                    )
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("the agent has no readable Agent document"))?;
                    if revoke {
                        txn.delete_actor_relationship(
                            "Agent",
                            &caller_doc,
                            "memory_reader",
                            &other_did,
                        )
                        .await?;
                    } else {
                        txn.add_actor_relationship(
                            "Agent",
                            &caller_doc,
                            "memory_reader",
                            &other_did,
                        )
                        .await?;
                    }
                    Ok::<(), anyhow::Error>(())
                })
            })
            .await
            .map_err(|error| {
                if error.permission_denied() {
                    anyhow::anyhow!(
                        "{verb} needs update on this agent's own Agent document, which ACP refused; ask the User to run policy grant --subject {other} --collection Agent --doc <agent_doc> --relation memory_reader"
                    )
                } else {
                    error
                }
            })?;
        Ok(json!({
            "action": verb,
            "agent_did": self.agent_did,
            "to_agent": other,
            "memory_reader": !revoke,
            "next_call": next_call,
        }))
    }

    async fn require_memory_read(&self, source: &str) -> anyhow::Result<String> {
        let node_access = super::ConfigAccess::local_as(
            std::sync::Arc::clone(self.node),
            identity::Did::new(self.node_did)?,
        );
        let source_did = source.to_owned();
        let node_did = self.node_did.to_owned();
        let source_doc = node_access
            .transact_readonly("tool.memory.carry.source", move |txn| {
                let node_did = node_did.clone();
                let source_did = source_did.clone();
                Box::pin(async move {
                    super::agent_doc_id_in_txn(txn, &node_did, &source_did).await
                })
            })
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "no Agent {source} is published on this node ({error}); call config agent list to find the agent DID"
                )
            })?;
        let observed = gents_store::identity::observe_document_permissions(
            self.node,
            self.node_did,
            &self.actor,
            Some(("Agent", &source_doc)),
            &["memory_read".to_owned()],
        )
        .await?;
        let held = observed
            .map(|observed| observed.held.first().copied().unwrap_or(false))
            .unwrap_or(false);
        anyhow::ensure!(
            held,
            "agent {source} has not shared memory with this agent; ask it to run memory share to_agent={}, then retry memory carry from_agent={source}",
            self.agent_did
        );
        Ok(source_doc)
    }

    pub(super) async fn carry(&self, args: MemoryParams) -> anyhow::Result<Value> {
        let source = args
            .from_agent
            .as_deref()
            .map(str::trim)
            .filter(|source| !source.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "carry requires from_agent: the agent DID whose memory to copy; call config agent list to find it"
                )
            })?
            .to_owned();
        identity::Did::new(&source)?;
        anyhow::ensure!(
            source != self.agent_did,
            "carry copies another agent's memory; this agent already reads its own, so pass from_agent naming a different agent"
        );
        let source_doc = self.require_memory_read(&source).await?;
        let page_limit = limit(&args);
        let page_offset = offset(&args);
        let caller = self.agent_did.to_owned();
        let node_did = self.node_did.to_owned();
        let source_owned = source.clone();
        let source_doc_owned = source_doc.clone();
        let report = self
            .access
            .transact_write("tool.memory.carry", move |txn| {
                let caller = caller.clone();
                let node_did = node_did.clone();
                let source = source_owned.clone();
                let source_doc = source_doc_owned.clone();
                Box::pin(async move {
                    let caller_doc = super::agent_doc_id_in_txn(txn, &node_did, &caller).await?;
                    let page =
                        read_page(txn, &source, &source_doc, page_limit, page_offset).await?;
                    let translated = translation(txn, &caller, &source, &source_doc, &page).await?;
                    let store = MemoryStore {
                        node: self.node,
                        node_did: &node_did,
                        actor: identity::Did::new(&caller)?,
                        agent_did: &caller,
                        agent_doc_id: caller_doc.clone(),
                        access: self.access,
                        embedder: self.embedder,
                    };
                    let problems = copy_problems(
                        txn,
                        &store,
                        &caller,
                        &caller_doc,
                        &source,
                        &page,
                        &translated,
                    )
                    .await?;
                    let attempts =
                        copy_attempts(txn, &caller, &caller_doc, &source, &page, &translated)
                            .await?;
                    let outcomes = copy_outcomes(txn, &caller, &caller_doc, &source, &page).await?;
                    let aliases =
                        copy_aliases(txn, &caller, &caller_doc, &page, &translated).await?;
                    let facts = copy_facts(txn, &store, &caller, &caller_doc, &page).await?;
                    let next_cursor = page.more.then(|| (page_offset + page_limit).to_string());
                    Ok::<Value, anyhow::Error>(json!({
                        "from_agent": source,
                        "stored": {
                            "problems": problems,
                            "attempts": attempts,
                            "outcomes": outcomes,
                            "aliases": aliases,
                            "facts": facts,
                        },
                        "limit": page_limit,
                        "max_per_collection": MAX_PAGE,
                        "next_cursor": next_cursor,
                    }))
                })
            })
            .await?;
        Ok(report)
    }
}
