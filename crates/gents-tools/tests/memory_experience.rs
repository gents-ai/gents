use std::collections::BTreeMap;

use gents_loop::tool::ToolCallEnvelope;
use gents_loop::tool::ToolDyn;
use gents_loop::tool_call_lifecycle::runtime::{
    CurrentToolRuntimeContext, ToolRequestIdentity, ToolScopeEntry,
};
use gents_protocol::row::ExecutedInferenceSettings;
use gents_session::request::lifecycle::ClaimOutcome;
use gents_session::test_support::{AgentHome, ClaimedRequest, Stage};
use gents_store::graphql::escape_graphql_string;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

async fn grant_agent_delete(db: &AgentHome) {
    let agent = identity::Did::new(db.agent_did()).expect("agent did");
    db.user_access
        .grant_node_permission(
            &agent,
            gents_store::config_client::NodePermission::DocumentDelete,
        )
        .await
        .expect("the operator grants the agent document delete");
}

async fn memory_tool(db: &AgentHome) -> Box<dyn ToolDyn> {
    gents_tools::memory::build_memory_tool(
        db.node.clone(),
        db.node_identity.did().to_owned(),
        db.agent_did().to_owned(),
        db.agent_identity.as_ref(),
    )
    .await
    .expect("build the memory tool")
}

fn runtime_for(db: &AgentHome, request: &ClaimedRequest) -> CurrentToolRuntimeContext {
    let row = request.lifecycle.request();
    let entry = ToolScopeEntry {
        identity: Some(ToolRequestIdentity::new(
            None,
            None,
            Some(db.agent_did().to_owned()),
            Some(row.request_id.clone()),
        )),
        deadline_at: None,
        cancellation_token: CancellationToken::new(),
        workspace_cwd: None,
        live_output: None,
        session_id: Some(row.session_id.clone()),
        correlation: Some("memory-test-call".to_owned()),
        source_fields: BTreeMap::new(),
        background: false,
    };
    CurrentToolRuntimeContext::entered(None, entry)
}

async fn call(
    tool: &dyn ToolDyn,
    args: Value,
    runtime: Option<CurrentToolRuntimeContext>,
) -> Value {
    let envelope = ToolCallEnvelope::new(runtime);
    let text = tool
        .call(args.to_string(), &envelope)
        .await
        .expect("the memory tool succeeds");
    serde_json::from_str(&text).expect("the memory tool returns JSON")
}

async fn call_error(
    tool: &dyn ToolDyn,
    args: Value,
    runtime: Option<CurrentToolRuntimeContext>,
) -> String {
    let envelope = ToolCallEnvelope::new(runtime);
    let error = tool
        .call(args.to_string(), &envelope)
        .await
        .expect_err("the memory tool refuses the call");
    error.to_string()
}

async fn rows(db: &AgentHome, query: &str) -> Vec<Value> {
    let value = db
        .agent_access
        .execute(query)
        .await
        .expect("the agent reads its memory");
    value
        .pointer("/data")
        .and_then(Value::as_object)
        .and_then(|data| data.values().next())
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

async fn assert_parented(db: &AgentHome, collection: &str, doc_id: &str) {
    let agent = rows(
        db,
        &format!(
            "{{Agent(filter: {{agent_did: {{_eq: \"{}\"}}}}, limit: 1) {{ _docID }}}}",
            escape_graphql_string(db.agent_did()),
        ),
    )
    .await;
    let agent_doc = agent[0]["_docID"].as_str().expect("the agent document");
    let edges = db
        .agent_access
        .list_document_relationships(collection, doc_id)
        .await
        .expect("the agent lists its document relationships");
    assert!(
        edges
            .iter()
            .any(|edge| edge.relation == "parent" && edge.subject.contains(agent_doc)),
        "{collection} {doc_id} is not parented to agents {agent_doc}: {edges:?}"
    );
}

async fn open(tool: &dyn ToolDyn, name: &str) -> Value {
    call(tool, json!({"action": "problem", "name": name}), None).await
}

#[tokio::test]
async fn problem_open_is_idempotent_and_lists_nearest() {
    let db = AgentHome::start("memory-problem-open").await;
    let tool = memory_tool(&db).await;

    let first = open(tool.as_ref(), "  Timeout on startup ").await;
    let again = open(tool.as_ref(), "timeout ON startup").await;
    assert_eq!(first["problem_key"], again["problem_key"]);
    assert_eq!(again["normalized_name"], "timeout on startup");

    let problems = rows(&db, "{AgentMemoryProblem { _docID problem_key }}").await;
    assert_eq!(problems.len(), 1, "a case variant opened no second problem");
    assert_parented(
        &db,
        "AgentMemoryProblem",
        problems[0]["_docID"].as_str().expect("problem document"),
    )
    .await;

    let neighbor = open(tool.as_ref(), "startup timeout in the worker").await;
    let shown = call(
        tool.as_ref(),
        json!({"action": "problem", "problem_key": first["problem_key"]}),
        None,
    )
    .await;
    let nearest = shown["nearest"]["hits"].as_array().expect("nearest hits");
    assert!(
        nearest
            .iter()
            .any(|hit| hit["problem_key"] == neighbor["problem_key"]),
        "the other problem is offered: {shown}"
    );
}

#[tokio::test]
async fn attempt_and_outcome_record_request_provenance() {
    let db = AgentHome::start("memory-attempt-outcome").await;
    let tool = memory_tool(&db).await;
    let mut request = ClaimedRequest::client_signed(&db.home, &db.binding)
        .ids("request-memory-provenance", "session-memory-provenance")
        .drive(Stage::Admitted)
        .await;
    let request_id = request.lifecycle.request().request_id.clone();
    request
        .lifecycle
        .set_executed_inference(ExecutedInferenceSettings {
            model_name: "test-model".to_owned(),
            reasoning_effort: Some("high".to_owned()),
            service_tier: None,
            inference_profile_id: None,
            runtime_generation: Some(7),
        });
    assert!(
        matches!(
            request
                .lifecycle
                .claim_with_identity()
                .await
                .expect("claim"),
            ClaimOutcome::Claimed
        ),
        "the request is claimed with its executed setting"
    );
    let runtime = runtime_for(&db, &request);

    let opened = open(tool.as_ref(), "the startup read races the migration").await;
    let attempt = call(
        tool.as_ref(),
        json!({
            "action": "attempt",
            "problem_key": opened["problem_key"],
            "what": "wait for the migration before the first read",
            "rationale": "the read observes a table the migration creates",
            "predicted": ["startup succeeds", "readiness under one second"],
            "accepted": true
        }),
        Some(runtime.clone()),
    )
    .await;
    let attempt_key = attempt["attempt_key"].as_str().expect("attempt_key");

    let stored = rows(
        &db,
        &format!(
            "{{AgentMemoryAttempt(filter: {{attempt_key: {{_eq: \"{}\"}}}}) {{ _docID provenance }}}}",
            escape_graphql_string(attempt_key),
        ),
    )
    .await;
    assert_eq!(stored.len(), 1);
    let provenance = &stored[0]["provenance"];
    assert_eq!(provenance["executed_model_name"], "test-model");
    assert_eq!(provenance["executed_reasoning_effort"], "high");
    assert_eq!(provenance["executed_runtime_generation"], 7);
    assert_eq!(provenance["request_id"], request_id);
    assert_eq!(provenance["session_id"], "session-memory-provenance");
    assert!(provenance.get("executed_inference_profile_id").is_none());
    assert_parented(
        &db,
        "AgentMemoryAttempt",
        stored[0]["_docID"].as_str().expect("attempt document"),
    )
    .await;

    let outcome = call(
        tool.as_ref(),
        json!({
            "action": "outcome",
            "attempt_key": attempt_key,
            "fixed": ["startup succeeds"],
            "missed": ["readiness under one second"],
            "passed": 3,
            "total": 4,
            "note": "the wait costs 200ms"
        }),
        Some(runtime.clone()),
    )
    .await;
    assert!(outcome["outcome_key"].is_string());

    let shown = call(
        tool.as_ref(),
        json!({"action": "problem", "problem_key": opened["problem_key"]}),
        Some(runtime),
    )
    .await;
    let attempts = shown["attempts"].as_array().expect("attempt list");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["attempt_key"], attempt_key);
    assert_eq!(attempts[0]["fixed"], json!(["startup succeeds"]));
    assert_eq!(attempts[0]["missed"], json!(["readiness under one second"]));
    assert_eq!(attempts[0]["unresolved"], json!([]));
    assert_eq!(attempts[0]["latest"], json!({"passed": 3, "total": 4}));
    assert_eq!(attempts[0]["provenance"], "same");
    assert_eq!(shown["attempts_truncated"], false);

    let refusal = call_error(
        tool.as_ref(),
        json!({"action": "outcome", "attempt_key": "no-such-attempt"}),
        None,
    )
    .await;
    assert!(refusal.contains("no attempt"), "{refusal}");
}

#[tokio::test]
async fn merge_keeps_every_attempt_and_refuses_cycles() {
    let db = AgentHome::start("memory-merge").await;
    let tool = memory_tool(&db).await;
    let request = ClaimedRequest::client_signed(&db.home, &db.binding)
        .ids("request-memory-merge", "session-memory-merge")
        .drive(Stage::Executing)
        .await;
    let runtime = runtime_for(&db, &request);

    let left = open(tool.as_ref(), "the cache key ignores the tenant").await;
    let right = open(tool.as_ref(), "the cache returns another tenant's row").await;
    let third = open(tool.as_ref(), "the index is stale").await;

    let bare = call(
        tool.as_ref(),
        json!({
            "action": "attempt",
            "problem_key": third["problem_key"],
            "what": "restart the indexer"
        }),
        Some(runtime.clone()),
    )
    .await;
    let bare_key = bare["attempt_key"].as_str().expect("bare attempt key");
    let bare_outcome = call(
        tool.as_ref(),
        json!({"action": "outcome", "attempt_key": bare_key, "passed": 0, "total": 0}),
        Some(runtime.clone()),
    )
    .await;
    assert!(bare_outcome["outcome_key"].is_string());
    let bare_rows = rows(
        &db,
        &format!(
            "{{AgentMemoryAttempt(filter: {{attempt_key: {{_eq: \"{}\"}}}}) {{ predicted }}}}",
            escape_graphql_string(bare_key),
        ),
    )
    .await;
    assert_eq!(bare_rows[0]["predicted"], Value::Null);
    let bare_outcomes = rows(
        &db,
        &format!(
            "{{AgentMemoryOutcome(filter: {{attempt_key: {{_eq: \"{}\"}}}}) {{ fixed missed }}}}",
            escape_graphql_string(bare_key),
        ),
    )
    .await;
    assert_eq!(bare_outcomes[0]["fixed"], Value::Null);
    assert_eq!(bare_outcomes[0]["missed"], Value::Null);

    for (problem, what) in [
        (&left, "include the tenant in the key"),
        (&right, "include the tenant in the lookup"),
    ] {
        let attempt = call(
            tool.as_ref(),
            json!({
                "action": "attempt",
                "problem_key": problem["problem_key"],
                "what": what,
                "predicted": ["no cross-tenant hit"]
            }),
            Some(runtime.clone()),
        )
        .await;
        assert!(attempt["attempt_key"].is_string());
    }

    let merged = call(
        tool.as_ref(),
        json!({
            "action": "merge",
            "from": left["problem_key"],
            "to": right["problem_key"]
        }),
        Some(runtime.clone()),
    )
    .await;
    assert_eq!(merged["decision"], "admitted");
    assert_eq!(merged["merges"], 1);

    let shown = call(
        tool.as_ref(),
        json!({"action": "problem", "problem_key": right["problem_key"]}),
        None,
    )
    .await;
    assert_eq!(shown["attempts"].as_array().expect("attempts").len(), 2);
    assert_eq!(shown["merged"], json!([left["problem_key"]]));

    let aliases = rows(
        &db,
        "{AgentMemoryAlias { _docID from_problem_key to_problem_key }}",
    )
    .await;
    assert_eq!(aliases.len(), 1);
    assert_parented(
        &db,
        "AgentMemoryAlias",
        aliases[0]["_docID"].as_str().expect("alias document"),
    )
    .await;
    let guards = rows(&db, "{AgentMemoryAliasGuard { agent_did merges }}").await;
    assert_eq!(guards.len(), 1);
    assert_eq!(guards[0]["merges"], 1);

    for (from, to, decision) in [
        (
            &left["problem_key"],
            &right["problem_key"],
            "already_merged",
        ),
        (&right["problem_key"], &left["problem_key"], "closes_cycle"),
        (&right["problem_key"], &right["problem_key"], "same_key"),
    ] {
        let refusal = call(
            tool.as_ref(),
            json!({"action": "merge", "from": from, "to": to}),
            Some(runtime.clone()),
        )
        .await;
        assert_eq!(refusal["decision"], decision, "{refusal}");
        assert!(refusal["message"].is_string());
    }

    let admitted = call(
        tool.as_ref(),
        json!({"action": "merge", "from": third["problem_key"], "to": right["problem_key"]}),
        Some(runtime.clone()),
    )
    .await;
    assert_eq!(admitted["decision"], "admitted", "{admitted}");
    assert_eq!(admitted["merges"], 2);

    let refused = call(
        tool.as_ref(),
        json!({"action": "merge", "from": "missing", "to": right["problem_key"]}),
        Some(runtime),
    )
    .await;
    assert_eq!(refused["decision"], "unknown_key");
    assert!(refused["message"].is_string());
}

#[tokio::test]
async fn facts_are_remembered_searched_parented_and_forgotten() {
    let db = AgentHome::start("memory-facts").await;
    grant_agent_delete(&db).await;
    let tool = memory_tool(&db).await;
    let request = ClaimedRequest::client_signed(&db.home, &db.binding)
        .ids("request-memory-facts", "session-memory-facts")
        .drive(Stage::Executing)
        .await;
    let runtime = runtime_for(&db, &request);

    let text = "run the migration before the first database read";
    let first = call(
        tool.as_ref(),
        json!({"action": "remember", "text": text, "kind": "rule"}),
        Some(runtime.clone()),
    )
    .await;
    let memory_key = first["stored"][0]["memory_key"]
        .as_str()
        .expect("memory_key")
        .to_owned();

    let again = call(
        tool.as_ref(),
        json!({"action": "remember", "text": text, "kind": "rule"}),
        Some(runtime.clone()),
    )
    .await;
    assert_eq!(again["stored"][0]["memory_key"], memory_key);

    let stored = rows(&db, "{AgentMemory { _docID memory_key kind }}").await;
    assert_eq!(
        stored.len(),
        1,
        "remembering the same text twice stored one row"
    );
    assert_eq!(stored[0]["kind"], "rule");
    assert_parented(
        &db,
        "AgentMemory",
        stored[0]["_docID"].as_str().expect("memory document"),
    )
    .await;

    let query = "a startup database read needs the migration";
    let found = call(
        tool.as_ref(),
        json!({"action": "search", "query": query}),
        Some(runtime.clone()),
    )
    .await;
    assert_ne!(found["ranking"], "none", "{found}");
    assert!(
        found["hits"]
            .as_array()
            .expect("hits")
            .iter()
            .any(|hit| hit["memory_key"].as_str() == Some(memory_key.as_str())),
        "the paraphrase finds the rule: {found}"
    );

    let forgotten = call(
        tool.as_ref(),
        json!({"action": "forget", "memory_key": memory_key}),
        Some(runtime.clone()),
    )
    .await;
    assert_eq!(forgotten["deleted"], 1);

    let after = call(
        tool.as_ref(),
        json!({"action": "search", "query": query}),
        Some(runtime),
    )
    .await;
    assert!(
        after["hits"]
            .as_array()
            .expect("hits")
            .iter()
            .all(|hit| hit["memory_key"].as_str() != Some(memory_key.as_str())),
        "the forgotten rule is gone: {after}"
    );
}

#[tokio::test]
async fn problems_index_pages_and_states_truncation() {
    let db = AgentHome::start("memory-problems-page").await;
    let tool = memory_tool(&db).await;
    for name in ["one failure", "two failures", "three failures"] {
        open(tool.as_ref(), name).await;
    }

    let first = call(
        tool.as_ref(),
        json!({"action": "problems", "limit": 2}),
        None,
    )
    .await;
    assert_eq!(first["problems"].as_array().expect("problems").len(), 2);
    assert_eq!(first["next_cursor"], "2");
    assert_eq!(first["attempts_truncated"], false);

    let second = call(
        tool.as_ref(),
        json!({"action": "problems", "limit": 2, "cursor": first["next_cursor"]}),
        None,
    )
    .await;
    assert_eq!(second["problems"].as_array().expect("problems").len(), 1);
    assert_eq!(second["next_cursor"], Value::Null);
    for entry in first["problems"]
        .as_array()
        .expect("problems")
        .iter()
        .chain(second["problems"].as_array().expect("problems"))
    {
        assert_eq!(entry["attempts"], 0);
        assert_eq!(entry["unresolved"], 0);
        assert_eq!(entry["provenance_flags"], json!([]));
    }
}
