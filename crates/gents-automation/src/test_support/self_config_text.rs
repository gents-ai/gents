use anyhow::Result;
use gents_loop::tool::ToolDyn;
use gents_protocol::tool_names::CONFIG_TOOL_NAME;
use gents_toolcalls::test_support::self_config::{config, SelfConfigFixture};
use serde_json::Value;

pub async fn call(tools: &Vec<Box<dyn ToolDyn>>, args: Value) -> Result<Value, String> {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    match tool
        .call(
            args.to_string(),
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
    {
        Ok(text) => Ok(serde_json::from_str(&text).unwrap_or(Value::String(text))),
        Err(error) => Err(format!("{error:#}")),
    }
}

pub async fn ok(tools: &Vec<Box<dyn ToolDyn>>, args: Value) -> Value {
    call(tools, args.clone())
        .await
        .unwrap_or_else(|error| panic!("{args}: {error}"))
}

pub async fn refused(tools: &Vec<Box<dyn ToolDyn>>, args: Value) -> String {
    match call(tools, args.clone()).await {
        Ok(value) => panic!("{args} was accepted: {value}"),
        Err(error) => error,
    }
}

pub async fn authenticated_setup(
    label: &str,
    categories: &[&str],
) -> (SelfConfigFixture, Vec<Box<dyn ToolDyn>>) {
    let fixture = crate::test_support::self_config_fixture(label).await;
    fixture.grant_selected_configuration().await;
    let tools = fixture.tools(categories);
    (fixture, tools)
}

/// The Engineer's catalog as the shipped pack_builder pack declares it.
pub fn pack_builder_skills() -> Vec<gents_documents::skills::Skill> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../core-packs/pack_builder/pack_config.json");
    let config: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let text = |skill: &Value, field: &str| skill[field].as_str().unwrap_or_default().to_owned();
    config["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| gents_documents::skills::Skill {
            skill_id: text(skill, "skill_id"),
            node_did: String::new(),
            name: text(skill, "name"),
            description: text(skill, "description"),
            instructions: String::new(),
            source_directory: None,
            tool_refs: Vec::new(),
            covers: skill["covers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            display_name: None,
            enabled: true,
        })
        .collect()
}

/// `authenticated_setup` for a caller whose catalog holds the shipped
/// pack_builder skills.
pub async fn skilled_setup(
    label: &str,
    categories: &[&str],
) -> (SelfConfigFixture, Vec<Box<dyn ToolDyn>>) {
    let fixture = crate::test_support::self_config_fixture(label).await;
    fixture.grant_selected_configuration().await;
    let mut grants = config(categories);
    grants.agent_did = fixture.agent_did().to_owned();
    grants.preview = true;
    let tools = fixture.tools_with_skills(
        &grants,
        std::sync::Arc::new(gents_tools::skill_pointer::SkillPointers::new(
            pack_builder_skills(),
        )),
    );
    (fixture, tools)
}

pub async fn structured_failure(tools: &Vec<Box<dyn ToolDyn>>, args: Value) -> Value {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    match tool
        .call(
            args.to_string(),
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
    {
        Err(gents_loop::tool::ToolError::ToolCallError(error)) => {
            serde_json::from_str(&error.to_string()).unwrap()
        }
        other => panic!("expected structured failure for {args}: {other:?}"),
    }
}

pub async fn ux_preview_tools(
    label: &str,
    home: &std::path::Path,
) -> (SelfConfigFixture, Vec<Box<dyn ToolDyn>>) {
    let fixture = crate::test_support::self_config_fixture(label).await;
    let mut grants = config(&["tools"]);
    grants.agent_did = fixture.agent_did().to_owned();
    let tools = fixture.tools_with_plugins(
        &grants,
        std::sync::Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::new(
            Some(home.to_owned()),
        )),
    );
    (fixture, tools)
}

/// One row per model-facing text: its name, its length and its ceiling.
/// Ceilings sit a little above the text as written; a text that outgrows its
/// ceiling moves detail one layer down (a verb page, a field, an explicit
/// read) instead of raising the number.
pub fn assert_sizes(rows: &[(String, usize, usize)]) {
    let table = rows
        .iter()
        .map(|(name, len, ceiling)| format!("{name}: {len} chars (ceiling {ceiling})"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rows.iter().all(|(_, len, ceiling)| len <= ceiling),
        "model-facing text outgrew its ceiling:\n{table}"
    );
}

pub async fn text(tools: &Vec<Box<dyn ToolDyn>>, args: Value) -> String {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    tool.call(
        args.to_string(),
        &gents_loop::tool::ToolCallEnvelope::default(),
    )
    .await
    .unwrap_or_else(|error| panic!("{args}: {error:#}"))
}
