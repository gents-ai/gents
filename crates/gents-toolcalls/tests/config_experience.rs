// A committed self-config write records its experience attempt in the same
// transaction, through the real config tool and the real memory rows.

use std::collections::BTreeMap;

use gents_documents::patch::SelfConfigTarget;
use gents_loop::tool_call_lifecycle::runtime::{
    CurrentToolRuntimeContext, ToolRequestIdentity, ToolScopeEntry,
};
use gents_protocol::tool_names::CONFIG_TOOL_NAME;
use gents_store::graphql::escape_graphql_string;
use gents_toolcalls::test_support::self_config::{config, SelfConfigFixture};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

fn config_runtime(
    agent_did: &str,
    request_id: &str,
    session_id: &str,
) -> CurrentToolRuntimeContext {
    let entry = ToolScopeEntry {
        identity: Some(ToolRequestIdentity::new(
            None,
            None,
            Some(agent_did.to_owned()),
            Some(request_id.to_owned()),
        )),
        deadline_at: None,
        cancellation_token: CancellationToken::new(),
        workspace_cwd: None,
        live_output: None,
        session_id: Some(session_id.to_owned()),
        correlation: Some("config-experience-test".to_owned()),
        source_fields: BTreeMap::new(),
        background: false,
    };
    CurrentToolRuntimeContext::entered(None, entry)
}

fn config_tool_config(
    fixture: &SelfConfigFixture,
    categories: &[&str],
) -> gents_tools::surface::SelfConfigToolConfig {
    let mut grants = config(categories);
    grants.agent_did = fixture.agent_did().to_owned();
    grants
}

async fn call_config(
    fixture: &SelfConfigFixture,
    categories: &[&str],
    args: Value,
    runtime: &CurrentToolRuntimeContext,
) -> Result<Value, String> {
    let grants = config_tool_config(fixture, categories);
    call_config_with_grants(fixture, &grants, args, runtime).await
}

async fn call_config_with_grants(
    fixture: &SelfConfigFixture,
    grants: &gents_tools::surface::SelfConfigToolConfig,
    args: Value,
    runtime: &CurrentToolRuntimeContext,
) -> Result<Value, String> {
    let tools = fixture.tools_with_config(grants);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    let text = tool
        .call(
            args.to_string(),
            &gents_loop::tool::ToolCallEnvelope::new(Some(runtime.clone())),
        )
        .await
        .map_err(|error| format!("{error:#}"))?;
    serde_json::from_str(&text).map_err(|error| format!("{error}: {text}"))
}

async fn agent_rows(fixture: &SelfConfigFixture, query: &str) -> Vec<Value> {
    let value = fixture
        .agent_access()
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

async fn attempts(fixture: &SelfConfigFixture) -> Vec<Value> {
    agent_rows(
        fixture,
        "{AgentMemoryAttempt { _docID attempt_key problem_key what rationale accepted targets provenance }}",
    )
    .await
}

async fn assert_attempt_parented_to_agent(fixture: &SelfConfigFixture, doc_id: &str) {
    let agent = agent_rows(
        fixture,
        &format!(
            "{{Agent(filter: {{agent_did: {{_eq: \"{}\"}}}}, limit: 1) {{ _docID }}}}",
            escape_graphql_string(fixture.agent_did()),
        ),
    )
    .await;
    let agent_doc = agent[0]["_docID"].as_str().expect("the agent document");
    let edges = fixture
        .agent_access()
        .list_document_relationships("AgentMemoryAttempt", doc_id)
        .await
        .expect("the agent lists its document relationships");
    assert!(
        edges
            .iter()
            .any(|edge| edge.relation == "parent" && edge.subject.contains(agent_doc)),
        "AgentMemoryAttempt {doc_id} is not parented to agent {agent_doc}: {edges:?}"
    );
}

fn targets_of(row: &Value) -> Value {
    match &row["targets"] {
        Value::String(text) => serde_json::from_str(text).expect("targets parses"),
        other => other.clone(),
    }
}

#[tokio::test]
async fn a_committed_tools_update_records_one_parented_attempt() {
    let fixture = SelfConfigFixture::new("config-experience-tools").await;
    fixture.grant_selected_configuration().await;
    let runtime = config_runtime(
        fixture.agent_did(),
        "request-config-experience",
        "session-config-experience",
    );
    let changed = "Experience recorded";
    let response = call_config(
        &fixture,
        &["tools"],
        json!({
            "argv": ["tools", "update"],
            "set": {"display_name": changed},
            "experience": {
                "new_problem": "a committed configuration write kept no attempt",
                "rationale": "the attempt joins the write transaction",
                "predicted": ["one attempt names the changed field"]
            }
        }),
        &runtime,
    )
    .await
    .expect("the tools update commits with its attempt");
    assert_eq!(response["committed"], true, "{response}");
    assert!(response["experience"].is_object(), "{response}");
    let attempt_key = response["experience"]["attempt_key"]
        .as_str()
        .expect("attempt_key");
    assert!(response["experience"]["problem_key"].is_string());

    let rows = agent_rows(
        &fixture,
        &format!(
            "{{AgentMemoryAttempt(filter: {{attempt_key: {{_eq: \"{}\"}}}}) {{ _docID problem_key what accepted targets provenance }}}}",
            escape_graphql_string(attempt_key),
        ),
    )
    .await;
    assert_eq!(rows.len(), 1, "exactly one attempt: {rows:?}");
    let row = &rows[0];
    assert_eq!(row["accepted"], true);
    assert_eq!(
        row["problem_key"].as_str(),
        response["experience"]["problem_key"].as_str()
    );
    assert_eq!(row["provenance"]["request_id"], "request-config-experience");
    assert_eq!(row["provenance"]["session_id"], "session-config-experience");
    let what = row["what"].as_str().expect("what");
    assert!(
        what.contains(SelfConfigTarget::Tools.collection_name()),
        "{what}"
    );
    assert!(what.contains("display_name"), "{what}");
    let targets = targets_of(row);
    assert_eq!(
        targets[0]["collection"],
        SelfConfigTarget::Tools.collection_name()
    );
    assert_eq!(
        targets[0]["target_id"],
        format!("{}:tools", fixture.agent_did())
    );
    let fields = targets[0]["fields"].as_array().expect("fields");
    assert!(
        fields.iter().any(|field| field["field"] == "display_name"),
        "{targets}"
    );
    for field in fields {
        for digest in [&field["from_sha256"], &field["to_sha256"]] {
            let digest = digest.as_str().expect("sha256 text");
            assert_eq!(digest.len(), 64);
            assert!(digest.chars().all(|digit| digit.is_ascii_hexdigit()));
        }
    }
    assert!(
        !targets.to_string().contains(changed),
        "raw values stay out of targets: {targets}"
    );
    assert_attempt_parented_to_agent(&fixture, row["_docID"].as_str().expect("attempt doc")).await;
}

#[tokio::test]
async fn a_cited_problem_receives_the_attempt_under_its_key() {
    let fixture = SelfConfigFixture::new("config-experience-cited").await;
    fixture.grant_selected_configuration().await;
    let memory = gents_tools::memory::build_memory_tool(
        fixture.db.node.clone(),
        fixture.node_did().to_owned(),
        fixture.agent_did().to_owned(),
        fixture.agent.as_ref(),
    )
    .await
    .expect("build the memory tool");
    let opened: Value = serde_json::from_str(
        &memory
            .call(
                json!({"action": "problem", "name": "the cited problem"}).to_string(),
                &gents_loop::tool::ToolCallEnvelope::default(),
            )
            .await
            .expect("the problem opens"),
    )
    .expect("the problem is JSON");
    let problem_key = opened["problem_key"].as_str().expect("problem_key");

    let runtime = config_runtime(
        fixture.agent_did(),
        "request-config-cited",
        "session-config-cited",
    );
    let response = call_config(
        &fixture,
        &["tools"],
        json!({
            "argv": ["tools", "update"],
            "set": {"display_name": "Cited problem"},
            "experience": {
                "problem_key": problem_key,
                "rationale": "the same problem explains the failure"
            }
        }),
        &runtime,
    )
    .await
    .expect("the tools update commits under the cited problem");
    assert_eq!(response["experience"]["problem_key"], problem_key);
    let rows = attempts(&fixture).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["problem_key"], problem_key);
}

#[tokio::test]
async fn a_preview_records_no_attempt() {
    let fixture = SelfConfigFixture::new("config-experience-preview").await;
    fixture.grant_selected_configuration().await;
    let runtime = config_runtime(
        fixture.agent_did(),
        "request-config-preview",
        "session-config-preview",
    );
    let mut grants = config_tool_config(&fixture, &["tools"]);
    grants.preview = true;
    let response = call_config_with_grants(
        &fixture,
        &grants,
        json!({
            "argv": ["tools", "preview", "update"],
            "set": {"display_name": "Preview only"},
            "experience": {
                "new_problem": "a preview must not record experience",
                "rationale": "nothing commits"
            }
        }),
        &runtime,
    )
    .await
    .expect("the preview answers");
    assert_eq!(response["committed"], false, "{response}");
    assert_eq!(response["experience"], "not recorded on preview");
    assert!(attempts(&fixture).await.is_empty());
}

#[tokio::test]
async fn an_unknown_problem_refuses_the_whole_write() {
    let fixture = SelfConfigFixture::new("config-experience-unknown").await;
    fixture.grant_selected_configuration().await;
    let before = fixture
        .user_access
        .execute("{ Tools { _docID display_name } }")
        .await
        .expect("the user reads Tools");
    let display_name = before["data"]["Tools"][0]["display_name"].clone();
    let runtime = config_runtime(
        fixture.agent_did(),
        "request-config-unknown",
        "session-config-unknown",
    );
    let error = call_config(
        &fixture,
        &["tools"],
        json!({
            "argv": ["tools", "update"],
            "set": {"display_name": "Must not land"},
            "experience": {
                "problem_key": "a-key-that-does-not-exist",
                "rationale": "an unknown problem cannot carry the attempt"
            }
        }),
        &runtime,
    )
    .await
    .expect_err("the write is refused");
    assert!(error.contains("memory problems"), "{error}");
    let after = fixture
        .user_access
        .execute("{ Tools { _docID display_name } }")
        .await
        .expect("the user reads Tools");
    assert_eq!(after["data"]["Tools"][0]["display_name"], display_name);
    assert!(attempts(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_batch_records_an_attempt_for_each_item_that_carries_one() {
    let fixture = SelfConfigFixture::new("config-experience-batch").await;
    fixture.grant_selected_configuration().await;
    let runtime = config_runtime(
        fixture.agent_did(),
        "request-config-batch",
        "session-config-batch",
    );
    let operation = |name: &str, problem: &str| {
        json!({
            "argv": ["tools", "update"],
            "set": {"display_name": name},
            "experience": {
                "new_problem": problem,
                "rationale": "each item records its own attempt"
            }
        })
    };
    let response = call_config(
        &fixture,
        &["tools"],
        json!({
            "argv": ["batch"],
            "options": {
                "operations": [
                    operation("Batch one", "the first batch item lost its attempt"),
                    operation("Batch two", "the second batch item lost its attempt")
                ]
            }
        }),
        &runtime,
    )
    .await
    .expect("the batch commits");
    assert_eq!(response["completed"], true, "{response}");
    assert_eq!(response["results"].as_array().expect("results").len(), 2);
    let keys = response["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|result| {
            result["result"]["experience"]["attempt_key"]
                .as_str()
                .unwrap_or_else(|| panic!("item records its attempt: {result}"))
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_ne!(keys[0], keys[1], "two distinct attempts");
    let rows = attempts(&fixture).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    let stored = rows
        .iter()
        .map(|row| row["attempt_key"].as_str().expect("attempt_key"))
        .collect::<Vec<_>>();
    for key in keys {
        assert!(stored.contains(&key.as_str()), "{stored:?} lacks {key}");
    }
}

#[tokio::test]
async fn experience_is_refused_when_memory_is_disabled() {
    let fixture = SelfConfigFixture::new("config-experience-disabled").await;
    fixture.grant_selected_configuration().await;
    let mut grants = config_tool_config(&fixture, &["tools"]);
    grants.memory_enabled = false;
    let tools = fixture.tools_with_config(&grants);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    let runtime = config_runtime(
        fixture.agent_did(),
        "request-config-disabled",
        "session-config-disabled",
    );
    let error = tool
        .call(
            json!({
                "argv": ["tools", "update"],
                "set": {"display_name": "Memory disabled"},
                "experience": {
                    "new_problem": "memory is disabled",
                    "rationale": "the call is refused"
                }
            })
            .to_string(),
            &gents_loop::tool::ToolCallEnvelope::new(Some(runtime)),
        )
        .await
        .expect_err("the call is refused");
    let error = format!("{error:#}");
    assert!(
        error.contains("memory is disabled for this agent"),
        "{error}"
    );
    assert!(attempts(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_skill_write_records_its_attempt_in_the_same_transaction() {
    let fixture = SelfConfigFixture::new("config-experience-skill").await;
    fixture.grant_selected_configuration().await;
    let runtime = config_runtime(
        fixture.agent_did(),
        "request-config-skill",
        "session-config-skill",
    );
    let response = call_config(
        &fixture,
        &["tools", "agent"],
        json!({
            "argv": ["skill", "create", "experience-skill"],
            "set": {
                "name": "Experience skill",
                "instructions": "Do the reviewed thing.",
                "enabled": true
            },
            "experience": {
                "new_problem": "a skill write kept no attempt",
                "rationale": "the skill handler stages its attempt in the write transaction"
            }
        }),
        &runtime,
    )
    .await
    .expect("the skill write commits with its attempt");
    let attempt_key = response["experience"]["attempt_key"]
        .as_str()
        .expect("attempt_key");
    let rows = agent_rows(
        &fixture,
        &format!(
            "{{AgentMemoryAttempt(filter: {{attempt_key: {{_eq: \"{}\"}}}}) {{ _docID what targets }}}}",
            escape_graphql_string(attempt_key),
        ),
    )
    .await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let targets = targets_of(&rows[0]);
    assert_eq!(
        targets[0]["collection"],
        SelfConfigTarget::Skill.collection_name()
    );
    assert_eq!(targets[0]["target_id"], "experience-skill");
    let fields = targets[0]["fields"].as_array().expect("fields");
    assert!(
        fields.iter().any(|field| field["field"] == "instructions"),
        "{targets}"
    );
    assert!(
        !targets.to_string().contains("Do the reviewed thing"),
        "{targets}"
    );
}

#[tokio::test]
async fn an_agent_update_records_its_attempt_against_the_caller() {
    let fixture = SelfConfigFixture::new("config-experience-agent").await;
    fixture.grant_selected_configuration().await;
    let runtime = config_runtime(
        fixture.agent_did(),
        "request-config-agent",
        "session-config-agent",
    );
    let response = call_config(
        &fixture,
        &["agent", "tools"],
        json!({
            "argv": ["agent", "edit"],
            "target_id": fixture.agent_did(),
            "set": {"display_name": "Experience agent"},
            "experience": {
                "new_problem": "an agent write kept no attempt",
                "rationale": "the agent handler stages its attempt in the write transaction"
            }
        }),
        &runtime,
    )
    .await
    .expect("the agent update commits with its attempt");
    let attempt_key = response["experience"]["attempt_key"]
        .as_str()
        .unwrap_or_else(|| panic!("the receipt carries its attempt: {response}"));
    let rows = agent_rows(
        &fixture,
        &format!(
            "{{AgentMemoryAttempt(filter: {{attempt_key: {{_eq: \"{}\"}}}}) {{ _docID what targets }} }}",
            escape_graphql_string(attempt_key),
        ),
    )
    .await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let targets = targets_of(&rows[0]);
    assert_eq!(
        targets[0]["collection"],
        SelfConfigTarget::Agent.collection_name()
    );
    assert_eq!(targets[0]["target_id"], fixture.agent_did());
    assert!(
        targets[0]["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .any(|field| field["field"] == "display_name"),
        "{targets}"
    );
    assert_attempt_parented_to_agent(&fixture, rows[0]["_docID"].as_str().expect("attempt doc"))
        .await;
}

#[tokio::test]
async fn an_agent_preview_says_the_attempt_was_not_recorded() {
    let fixture = SelfConfigFixture::new("config-experience-agent-preview").await;
    fixture.grant_selected_configuration().await;
    let runtime = config_runtime(
        fixture.agent_did(),
        "request-config-agent-preview",
        "session-config-agent-preview",
    );
    let mut grants = config_tool_config(&fixture, &["agent", "tools"]);
    grants.preview = true;
    let response = call_config_with_grants(
        &fixture,
        &grants,
        json!({
            "argv": ["agent", "preview", "edit"],
            "target_id": fixture.agent_did(),
            "set": {"display_name": "Previewed agent"},
            "experience": {
                "new_problem": "an agent preview must not record experience",
                "rationale": "nothing commits"
            }
        }),
        &runtime,
    )
    .await
    .expect("the agent preview answers");
    assert_eq!(response["committed"], false, "{response}");
    assert_eq!(response["experience"], "not recorded on preview");
    assert!(attempts(&fixture).await.is_empty());
}
