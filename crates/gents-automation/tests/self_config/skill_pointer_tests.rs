use super::*;
use gents_lean_vocab::families::skill_pointer_named as family;
use gents_loop::tool_call_lifecycle::runtime::{ToolRequestIdentity, ToolScopeEntry};
use gents_toolcalls::test_support::self_config::config;
use std::sync::Arc;

/// The skill a help page or failure text names, as the model reads it.
fn named(text: &str) -> Option<String> {
    let (_, rest) = text.split_once("First load `")?;
    rest.split_once('`').map(|(name, _)| name.to_owned())
}

/// Each case's catalog is the caller's own; each help event reads that page
/// and each failure event makes a call at that path that fails, in its
/// request, through the config and schema tools a surface builds for it.
#[tokio::test]
async fn help_pages_and_first_failures_name_the_callers_covering_skill() {
    gents_lean_vocab::snapshot().await;
    for (index, case) in family::cases::<String>().into_iter().enumerate() {
        let fixture =
            gents_automation::test_support::self_config_fixture(&format!("skill-pointer-{index}"))
                .await;
        fixture.grant_selected_configuration().await;
        let catalog = case
            .input
            .catalog
            .iter()
            .map(|entry| gents_documents::skills::Skill {
                skill_id: entry.name.clone(),
                node_did: fixture.node_did().to_owned(),
                name: entry.name.clone(),
                description: String::new(),
                instructions: String::new(),
                source_directory: None,
                tool_refs: Vec::new(),
                covers: entry.covers.iter().map(|cover| cover.join(" ")).collect(),
                display_name: None,
                enabled: true,
            })
            .collect();
        let pointers = Arc::new(gents_tools::skill_pointer::SkillPointers::new(catalog));
        let mut grants = config(&["node", "agent", "tools", "automation", "policy"]);
        grants.agent_did = fixture.agent_did().to_owned();
        let tools = fixture.tools_with_skills(&grants, pointers.clone());
        let config_tool = tools
            .iter()
            .find(|tool| tool.name() == CONFIG_TOOL_NAME)
            .unwrap();
        let schema = gents_tools::builtin::schema_help::SchemaTool::for_managed_agent(
            fixture.db.node.clone(),
            fixture.node_did(),
            None,
            fixture.agent.as_ref(),
        )
        .unwrap()
        .with_skill_pointers(pointers);
        let mut observed = Vec::new();
        for event in &case.input.events {
            let (tool, words) = event.path.split_first().unwrap();
            let mut argv = words.to_vec();
            let help = event.kind == "help";
            if help {
                argv.insert(0, "help".into());
            } else {
                argv.push("missing".into());
            }
            let mut entry = ToolScopeEntry::new(None, tokio_util::sync::CancellationToken::new());
            entry.identity = Some(ToolRequestIdentity::new(
                None,
                None,
                None,
                Some(event.request.clone()),
            ));
            let envelope = entry.call();
            let args = json!({"argv": argv}).to_string();
            let result = match tool.as_str() {
                "config" => config_tool
                    .call(args, &envelope)
                    .await
                    .map_err(|error| error.to_string()),
                "schema" => ToolDyn::call(&schema, args, &envelope)
                    .await
                    .map_err(|error| error.to_string()),
                other => panic!("no tool {other}"),
            };
            let text = match result {
                Ok(page) if help => page,
                Err(failure) if !help => failure,
                other => panic!("{:?} at {:?}: {other:?}", event.kind, event.path),
            };
            observed.push(named(&text));
        }
        case.check(observed);
    }
}

#[tokio::test]
async fn every_engineer_help_page_names_its_covering_skill() {
    let fixture =
        gents_automation::test_support::self_config_fixture("skill-pointer-all-pages").await;
    fixture.grant_selected_configuration().await;
    let pointers = Arc::new(gents_tools::skill_pointer::SkillPointers::new(
        gents_automation::test_support::self_config_text::pack_builder_skills(),
    ));
    let mut grants = config(&[
        "node",
        "agent",
        "tools",
        "profile",
        "backend",
        "mcp_service",
        "automation",
        "policy",
    ]);
    grants.agent_did = fixture.agent_did().to_owned();
    grants.enable_pack_install = true;
    let tools = fixture.tools_with_skills(&grants, pointers.clone());
    let config_tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let schema = gents_tools::builtin::schema_help::SchemaTool::for_managed_agent(
        fixture.db.node.clone(),
        fixture.node_did(),
        None,
        fixture.agent.as_ref(),
    )
    .unwrap()
    .with_skill_pointers(pointers);
    let envelope = ToolScopeEntry::new(None, tokio_util::sync::CancellationToken::new()).call();

    let index = config_tool
        .call(json!({"argv": ["help"]}).to_string(), &envelope)
        .await
        .unwrap();
    assert_eq!(named(&index), None, "config help index names no skill");

    for resource in gents_tools::builtin::self_config::command::help_resources() {
        let page = config_tool
            .call(json!({"argv": ["help", resource]}).to_string(), &envelope)
            .await
            .unwrap_or_else(|error| panic!("config help {resource}: {error}"));
        let expected = match resource {
            "datastore" => Some("design-data"),
            "agent-target" => Some("use-helpers"),
            "mcp-service" => Some("import-data"),
            "task" | "trigger" | "schedule" | "event-source" | "automation" => Some("automate"),
            "pack" => Some("package-or-install"),
            "page" | "ux" => Some("design-page"),
            "plugin" | "plugin-source" => Some("exact-rules"),
            "policy" => Some("control-access"),
            _ => None,
        };
        assert_eq!(
            named(&page).as_deref(),
            expected,
            "config help {resource} pointer mismatch"
        );
    }

    for (argv, expected) in [
        (vec!["ux", "preview", "--help"], Some("design-page")),
        (vec!["agent", "create", "--help"], Some("use-helpers")),
        (vec!["agent", "clone", "--help"], Some("use-helpers")),
        (vec!["agent", "get", "--help"], None),
        (vec!["help", "trigger", "explain"], Some("troubleshoot")),
        (vec!["trigger", "explain", "--help"], Some("troubleshoot")),
    ] {
        let page = config_tool
            .call(json!({"argv": argv}).to_string(), &envelope)
            .await
            .unwrap_or_else(|error| panic!("config {argv:?}: {error}"));
        assert_eq!(
            named(&page).as_deref(),
            expected,
            "config {argv:?} pointer mismatch"
        );
    }

    let schema_index = ToolDyn::call(&schema, json!({"argv": ["help"]}).to_string(), &envelope)
        .await
        .unwrap();
    assert_eq!(
        named(&schema_index),
        None,
        "schema help index names no skill"
    );

    for path in gents_tools::builtin::schema_help::help_paths() {
        let mut argv = vec!["help"];
        argv.extend(path.split(' '));
        let page = ToolDyn::call(&schema, json!({"argv": argv}).to_string(), &envelope)
            .await
            .unwrap_or_else(|error| panic!("schema help {path}: {error}"));
        let expected = if *path == "collections policy" {
            Some("control-access")
        } else {
            Some("design-data")
        };
        assert_eq!(
            named(&page).as_deref(),
            expected,
            "schema help {path} pointer mismatch"
        );
    }
}
