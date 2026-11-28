use std::collections::BTreeMap;

use gents_loop::tool::ToolCallEnvelope;
use gents_loop::tool::ToolDyn;
use gents_loop::tool_call_lifecycle::runtime::{
    CurrentToolRuntimeContext, ToolRequestIdentity, ToolScopeEntry,
};
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_protocol::operations::StringUpdate;
use gents_session::test_support::{AgentHome, ClaimedRequest, Stage};
use gents_store::graphql::escape_graphql_string;
use gents_tools::surface::SelfConfigProcessCeiling;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

async fn agent_tool(db: &AgentHome, binding: &AgentIdentityBinding) -> Box<dyn ToolDyn> {
    let identity = db.home.resolve_agent(binding);
    gents_tools::memory::build_memory_tool(
        db.node.clone(),
        db.node_identity.did().to_owned(),
        binding.agent_did.clone(),
        identity.as_ref(),
    )
    .await
    .expect("build the memory tool")
}

fn runtime_for(
    _db: &AgentHome,
    agent_did: &str,
    request: &ClaimedRequest,
) -> CurrentToolRuntimeContext {
    let row = request.lifecycle.request();
    let entry = ToolScopeEntry {
        identity: Some(ToolRequestIdentity::new(
            None,
            None,
            Some(agent_did.to_owned()),
            Some(row.request_id.clone()),
        )),
        deadline_at: None,
        cancellation_token: CancellationToken::new(),
        workspace_cwd: None,
        live_output: None,
        session_id: Some(row.session_id.clone()),
        correlation: Some("memory-carry-test-call".to_owned()),
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

async fn count(db: &AgentHome, collection: &str) -> usize {
    rows(
        db,
        &format!(
            "{{{collection}(filter: {{agent_did: {{_eq: \"{}\"}}}}) {{ _docID }}}}",
            escape_graphql_string(db.agent_did()),
        ),
    )
    .await
    .len()
}

async fn user_agent_doc(db: &AgentHome, agent_did: &str) -> String {
    let value = db
        .user_access
        .execute(&format!(
            "{{Agent(filter: {{agent_did: {{_eq: \"{}\"}}}}, limit: 1) {{ _docID }}}}",
            escape_graphql_string(agent_did),
        ))
        .await
        .expect("the User reads the agent document");
    value
        .pointer("/data/Agent/0/_docID")
        .and_then(Value::as_str)
        .expect("the agent document id")
        .to_owned()
}

async fn agent_doc(db: &AgentHome, agent_did: &str) -> String {
    let value = db
        .agent_access
        .execute(&format!(
            "{{Agent(filter: {{agent_did: {{_eq: \"{}\"}}}}, limit: 1) {{ _docID }}}}",
            escape_graphql_string(agent_did),
        ))
        .await
        .expect("the agent document is readable");
    value
        .pointer("/data/Agent/0/_docID")
        .and_then(Value::as_str)
        .expect("the agent document id")
        .to_owned()
}

async fn assert_parented(db: &AgentHome, collection: &str, doc_id: &str) {
    let agent_doc = agent_doc(db, db.agent_did()).await;
    let value = db
        .agent_access
        .execute(&format!(
            "{{{collection}(filter: {{_docID: {{_eq: \"{}\"}}}}, limit: 1) {{ agent_doc_id }}}}",
            escape_graphql_string(doc_id),
        ))
        .await
        .expect("the memory row is readable");
    let stored = value
        .pointer(&format!("/data/{collection}/0/agent_doc_id"))
        .and_then(Value::as_str);
    assert_eq!(
        stored,
        Some(agent_doc.as_str()),
        "{collection} {doc_id} does not name the Agent document"
    );
}

async fn open(tool: &dyn ToolDyn, name: &str) -> Value {
    call(tool, json!({"action": "problem", "name": name}), None).await
}

struct Source {
    binding: AgentIdentityBinding,
    tool: Box<dyn ToolDyn>,
    merged: Value,
    attempt: Value,
    outcome: Value,
}

async fn seed_source(db: &AgentHome, source: &AgentIdentityBinding) -> Source {
    let tool = agent_tool(db, source).await;
    let problem = open(tool.as_ref(), "worker timeout").await;
    let merged = open(tool.as_ref(), "startup race").await;
    let request = ClaimedRequest::client_signed(&db.home, source)
        .ids("request-memory-carry-source", "session-memory-carry-source")
        .drive(Stage::Executing)
        .await;
    let runtime = runtime_for(db, &source.agent_did, &request);
    let attempt = call(
        tool.as_ref(),
        json!({
            "action": "attempt",
            "problem_key": problem["problem_key"],
            "what": "warm the pool before the first request",
            "rationale": "the first request pays for pool creation",
            "predicted": ["startup under one second"],
            "accepted": true
        }),
        Some(runtime.clone()),
    )
    .await;
    let outcome = call(
        tool.as_ref(),
        json!({
            "action": "outcome",
            "attempt_key": attempt["attempt_key"],
            "fixed": ["startup under one second"],
            "missed": [],
            "passed": 3,
            "total": 3,
            "note": "the warm pool held"
        }),
        Some(runtime.clone()),
    )
    .await;
    let merged_report = call(
        tool.as_ref(),
        json!({"action": "merge", "from": merged["problem_key"], "to": problem["problem_key"]}),
        None,
    )
    .await;
    assert_eq!(merged_report["decision"], "admitted", "{merged_report}");
    call(
        tool.as_ref(),
        json!({"action": "remember", "text": "warm the worker pool before serving", "kind": "fact"}),
        Some(runtime),
    )
    .await;
    Source {
        binding: source.clone(),
        tool,
        merged,
        attempt,
        outcome,
    }
}

#[tokio::test]
async fn carry_refuses_without_a_share_and_unshare_revokes_it() {
    let db = AgentHome::start("memory-carry-refused").await;
    let source_binding = db
        .create_agent(
            "Carry source",
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await;
    let source = seed_source(&db, &source_binding).await;
    let caller = agent_tool(&db, &db.binding).await;

    let refused = call_error(
        caller.as_ref(),
        json!({"action": "carry", "from_agent": source.binding.agent_did}),
        None,
    )
    .await;
    assert!(refused.contains("has not shared memory"), "{refused}");
    assert!(refused.contains("memory share"), "{refused}");
    for collection in [
        "AgentMemoryProblem",
        "AgentMemoryAttempt",
        "AgentMemoryOutcome",
        "AgentMemoryAlias",
        "AgentMemory",
    ] {
        assert_eq!(
            count(&db, collection).await,
            0,
            "{collection} stored a row without a share"
        );
    }

    call(
        source.tool.as_ref(),
        json!({"action": "share", "to_agent": db.agent_did()}),
        None,
    )
    .await;
    let carried = call(
        caller.as_ref(),
        json!({"action": "carry", "from_agent": source.binding.agent_did}),
        None,
    )
    .await;
    assert_eq!(carried["stored"]["problems"], 2, "{carried}");

    call(
        source.tool.as_ref(),
        json!({"action": "unshare", "to_agent": db.agent_did()}),
        None,
    )
    .await;
    let refused = call_error(
        caller.as_ref(),
        json!({"action": "carry", "from_agent": source.binding.agent_did}),
        None,
    )
    .await;
    assert!(refused.contains("has not shared memory"), "{refused}");
}

#[tokio::test]
async fn carry_copies_once_and_lands_on_the_existing_problem() {
    let db = AgentHome::start("memory-carry").await;
    let source_binding = db
        .create_agent(
            "Carry source",
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await;
    let source = seed_source(&db, &source_binding).await;
    let caller = agent_tool(&db, &db.binding).await;
    let caller_problem = open(caller.as_ref(), "  Worker TIMEOUT ").await;

    call(
        source.tool.as_ref(),
        json!({"action": "share", "to_agent": db.agent_did()}),
        None,
    )
    .await;
    let report = call(
        caller.as_ref(),
        json!({"action": "carry", "from_agent": source.binding.agent_did}),
        None,
    )
    .await;
    assert_eq!(report["stored"]["problems"], 1, "{report}");
    assert_eq!(report["stored"]["attempts"], 1, "{report}");
    assert_eq!(report["stored"]["outcomes"], 1, "{report}");
    assert_eq!(report["stored"]["aliases"], 1, "{report}");
    assert_eq!(
        report["stored"]["facts"], 0,
        "a memory_reader reads experience only, never facts: {report}"
    );
    assert_eq!(report["next_cursor"], Value::Null, "{report}");

    let caller_did = escape_graphql_string(db.agent_did());
    let problems = rows(
        &db,
        &format!(
            "{{AgentMemoryProblem(filter: {{agent_did: {{_eq: \"{caller_did}\"}}}}) {{ _docID problem_key name carried_from source_problem_key }}}}"
        ),
    )
    .await;
    assert_eq!(
        problems.len(),
        2,
        "one source problem landed on the caller's"
    );
    let landed = problems
        .iter()
        .find(|row| row["problem_key"] == caller_problem["problem_key"])
        .expect("the existing caller problem");
    assert_eq!(landed["carried_from"], Value::Null);
    let copied = problems
        .iter()
        .find(|row| row["name"] == "startup race")
        .expect("the copied problem");
    assert_eq!(copied["carried_from"], source.binding.agent_did);
    assert_eq!(
        copied["source_problem_key"], source.merged["problem_key"],
        "the source key is kept"
    );
    assert_parented(
        &db,
        "AgentMemoryProblem",
        copied["_docID"].as_str().expect("copied problem document"),
    )
    .await;

    let attempts = rows(
        &db,
        &format!(
            "{{AgentMemoryAttempt(filter: {{agent_did: {{_eq: \"{caller_did}\"}}}}) {{ _docID attempt_key problem_key carried_from source_attempt_key provenance }}}}"
        ),
    )
    .await;
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        attempts[0]["problem_key"], caller_problem["problem_key"],
        "the carried attempt cites the caller's existing problem"
    );
    assert_eq!(attempts[0]["carried_from"], source.binding.agent_did);
    assert_eq!(
        attempts[0]["source_attempt_key"],
        source.attempt["attempt_key"]
    );
    assert_eq!(
        attempts[0]["provenance"]["request_id"], "request-memory-carry-source",
        "the original provenance is kept"
    );
    assert_parented(
        &db,
        "AgentMemoryAttempt",
        attempts[0]["_docID"].as_str().expect("attempt document"),
    )
    .await;

    let index = call(caller.as_ref(), json!({"action": "problems"}), None).await;
    let entry = index["problems"]
        .as_array()
        .expect("the problem index")
        .iter()
        .find(|entry| entry["problem_key"] == caller_problem["problem_key"])
        .expect("the caller problem in its index");
    assert!(
        entry["provenance_flags"]
            .as_array()
            .expect("provenance flags")
            .iter()
            .any(|flag| flag == "unknown model and other agent"),
        "the carried attempt is flagged as another agent's with an unknown model: {entry}"
    );

    let outcomes = rows(
        &db,
        &format!(
            "{{AgentMemoryOutcome(filter: {{agent_did: {{_eq: \"{caller_did}\"}}}}) {{ _docID outcome_key attempt_key carried_from source_outcome_key }}}}"
        ),
    )
    .await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0]["carried_from"], source.binding.agent_did);
    assert_eq!(
        outcomes[0]["source_outcome_key"],
        source.outcome["outcome_key"]
    );
    assert_eq!(outcomes[0]["attempt_key"], attempts[0]["attempt_key"]);
    assert_parented(
        &db,
        "AgentMemoryOutcome",
        outcomes[0]["_docID"].as_str().expect("outcome document"),
    )
    .await;

    let aliases = rows(
        &db,
        &format!(
            "{{AgentMemoryAlias(filter: {{agent_did: {{_eq: \"{caller_did}\"}}}}) {{ _docID from_problem_key to_problem_key }}}}"
        ),
    )
    .await;
    assert_eq!(aliases.len(), 1);
    assert_eq!(
        aliases[0]["from_problem_key"], copied["problem_key"],
        "the alias's source key is translated too"
    );
    assert_eq!(aliases[0]["to_problem_key"], landed["problem_key"]);
    assert_parented(
        &db,
        "AgentMemoryAlias",
        aliases[0]["_docID"].as_str().expect("alias document"),
    )
    .await;

    let facts = rows(
        &db,
        &format!(
            "{{AgentMemory(filter: {{agent_did: {{_eq: \"{caller_did}\"}}}}) {{ _docID text agent_did }}}}"
        ),
    )
    .await;
    assert!(
        facts.is_empty(),
        "a memory_reader grant carries no facts: {facts:?}"
    );

    let again = call(
        caller.as_ref(),
        json!({"action": "carry", "from_agent": source.binding.agent_did}),
        None,
    )
    .await;
    for (collection, stored) in [
        ("problems", again["stored"]["problems"].clone()),
        ("attempts", again["stored"]["attempts"].clone()),
        ("outcomes", again["stored"]["outcomes"].clone()),
        ("aliases", again["stored"]["aliases"].clone()),
        ("facts", again["stored"]["facts"].clone()),
    ] {
        assert_eq!(stored, json!(0), "re-running {collection} stored {stored}");
    }
    assert_eq!(count(&db, "AgentMemoryProblem").await, 2);
    assert_eq!(count(&db, "AgentMemoryAttempt").await, 1);
    assert_eq!(count(&db, "AgentMemoryOutcome").await, 1);
    assert_eq!(count(&db, "AgentMemoryAlias").await, 1);
    assert_eq!(count(&db, "AgentMemory").await, 0);
}

#[tokio::test]
async fn cloning_grants_memory_reader_and_the_clone_carries() {
    let db = AgentHome::start("memory-carry-clone").await;
    let source_binding = db
        .create_agent(
            "Carry source",
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await;
    let _source = seed_source(&db, &source_binding).await;
    let profile = db
        .user_access
        .execute(&format!(
            "{{Agent(filter: {{agent_did: {{_eq: \"{}\"}}}}, limit: 1) {{ inference_profile_id }}}}",
            escape_graphql_string(&source_binding.agent_did),
        ))
        .await
        .expect("the User reads the source agent")
        .pointer("/data/Agent/0/inference_profile_id")
        .and_then(Value::as_str)
        .expect("the source profile")
        .to_owned();

    let clone_binding = db.provision_agent().await;
    let receipt = gents_tools::builtin::self_config::apply_agent(
        &db.user_access,
        db.node_did(),
        &gents_tools::builtin::self_config::ConfigureAgentParams {
            action: "clone".to_owned(),
            identity_binding: Some(clone_binding.clone()),
            clone_from: Some(source_binding.agent_did.clone()),
            display_name: StringUpdate::Set("Carry clone".to_owned()),
            profile_id: StringUpdate::Set(profile),
            ..Default::default()
        },
        &SelfConfigProcessCeiling::default(),
    )
    .await
    .expect("the User clones the source agent");
    let memory = receipt
        .memory
        .as_ref()
        .expect("a clone receipt names memory");
    assert!(memory.shared, "{memory:?}");
    assert_eq!(
        memory.carry_call,
        format!("memory carry from_agent={}", source_binding.agent_did)
    );
    assert_eq!(memory.not_shared, None);

    let source_doc = user_agent_doc(&db, &source_binding.agent_did).await;
    let edges = db
        .user_access
        .list_document_relationships("Agent", &source_doc)
        .await
        .expect("the User reads the source relationships");
    assert!(
        edges.iter().any(|edge| {
            edge.relation == "memory_reader" && edge.subject.contains(&clone_binding.agent_did)
        }),
        "the clone holds memory_reader on the source: {edges:?}"
    );

    let clone_tool = agent_tool(&db, &clone_binding).await;
    let carried = call(
        clone_tool.as_ref(),
        json!({"action": "carry", "from_agent": source_binding.agent_did}),
        None,
    )
    .await;
    assert_eq!(carried["stored"]["problems"], 2, "{carried}");
    assert_eq!(carried["stored"]["facts"], 0, "{carried}");
}
