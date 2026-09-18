use serde_json::json;

use super::*;

const OWNER: &str = "did:key:starter-recipe-test";
const TOOLS_ID: &str = "starter-recipe-tools";

fn explicit_scope(root: &str) -> StarterWorkspaceScope {
    StarterWorkspaceScope::ExplicitRoot {
        root: root.to_owned(),
    }
}

fn input_for(id: StarterRecipeId) -> StarterRecipeRenderInput {
    match id {
        StarterRecipeId::Coding | StarterRecipeId::CodeReview => StarterRecipeRenderInput {
            workspace_scope: Some(explicit_scope("/workspace/project")),
            ..StarterRecipeRenderInput::default()
        },
        StarterRecipeId::Research | StarterRecipeId::GeneralAssistant => {
            StarterRecipeRenderInput::default()
        }
        StarterRecipeId::Custom => StarterRecipeRenderInput {
            custom_role: Some("a careful policy analyst".to_owned()),
            custom_goal: Some("compare the available options".to_owned()),
            custom_success_criteria: Some(
                "the recommendation identifies evidence, tradeoffs, and uncertainty".to_owned(),
            ),
            ..StarterRecipeRenderInput::default()
        },
    }
}

#[test]
fn catalog_snapshot_is_small_stable_and_complete() {
    let snapshot = serde_json::to_value(starter_recipe_catalog()).unwrap();
    assert_eq!(
        snapshot,
        json!([
            {
                "id": "gents.coding",
                "version": 1,
                "display_name": "Coding",
                "summary": "Build and change software inside one effective workspace.",
                "inputs": [{
                    "key": "workspace_scope",
                    "kind": "workspace_scope",
                    "required": true,
                    "description": "An admitted absolute directory or the managed runtime's effective root."
                }]
            },
            {
                "id": "gents.code-review",
                "version": 1,
                "display_name": "Code review",
                "summary": "Inspect code and report evidence-backed defects without modifying it.",
                "inputs": [{
                    "key": "workspace_scope",
                    "kind": "workspace_scope",
                    "required": true,
                    "description": "An admitted absolute directory or the managed runtime's effective root."
                }]
            },
            {
                "id": "gents.research",
                "version": 1,
                "display_name": "Research",
                "summary": "Investigate a question using available read-only sources and clear evidence standards.",
                "inputs": [{
                    "key": "workspace_scope",
                    "kind": "workspace_scope",
                    "required": false,
                    "description": "Optional admitted local source directory. Network retrieval is a separate capability."
                }]
            },
            {
                "id": "gents.general-assistant",
                "version": 1,
                "display_name": "General assistant",
                "summary": "Help with conversation, planning, writing, and explanation without host authority.",
                "inputs": []
            },
            {
                "id": "gents.custom",
                "version": 1,
                "display_name": "Custom",
                "summary": "Create a bounded behavior from an explicit role, goal, and success criteria.",
                "inputs": [
                    {
                        "key": "custom_role",
                        "kind": "text",
                        "required": true,
                        "description": "A concrete role describing the intended work."
                    },
                    {
                        "key": "custom_goal",
                        "kind": "text",
                        "required": true,
                        "description": "The outcome this behavior should pursue."
                    },
                    {
                        "key": "custom_success_criteria",
                        "kind": "text",
                        "required": true,
                        "description": "Observable conditions that mean the work is complete."
                    }
                ]
            }
        ])
    );

    let mut ids = starter_recipe_catalog()
        .iter()
        .map(|recipe| recipe.id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), starter_recipe_catalog().len());
}

#[test]
fn every_recipe_renders_a_complete_literal_prompt_and_provenance() {
    for definition in starter_recipe_catalog() {
        let resolved =
            resolve_starter_recipe(definition.id, OWNER, TOOLS_ID, &input_for(definition.id))
                .unwrap();
        assert_eq!(resolved.recipe_id, definition.id);
        assert_eq!(resolved.recipe_version, 1);
        assert_eq!(
            resolved.provenance_tag,
            format!("recipe:{}@1", definition.id)
        );
        assert_eq!(resolved.recommended_tools.tags, [resolved.provenance_tag]);
        assert!(!resolved.system_prompt.contains("{{"));
        assert!(!resolved.system_prompt.contains("}}"));
        assert!(resolved.system_prompt.len() < MAX_RENDERED_PROMPT_BYTES);
        assert!(resolved
            .recommended_tools
            .validation_violations()
            .is_empty());
    }
}

#[test]
fn coding_tools_snapshot_is_workspace_write_without_network_or_background() {
    let resolved = resolve_starter_recipe(
        StarterRecipeId::Coding,
        OWNER,
        TOOLS_ID,
        &input_for(StarterRecipeId::Coding),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&resolved.recommended_tools).unwrap(),
        json!({
            "tools_id": TOOLS_ID,
            "agent_did": OWNER,
            "display_name": "Coding starter tools",
            "host": {
                "root": "/workspace/project",
                "files": {"mode": "ReadWrite"},
                "bash": {
                    "mode": "Unrestricted",
                    "execution_mode": "workspace_write",
                    "network_mode": "disabled"
                }
            },
            "tags": ["recipe:gents.coding@1"]
        })
    );
    assert!(resolved.system_prompt.contains("actual language"));
    assert!(resolved.system_prompt.contains("workspace-write"));
}

#[test]
fn code_review_tools_snapshot_is_read_only() {
    let resolved = resolve_starter_recipe(
        StarterRecipeId::CodeReview,
        OWNER,
        TOOLS_ID,
        &input_for(StarterRecipeId::CodeReview),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&resolved.recommended_tools).unwrap(),
        json!({
            "tools_id": TOOLS_ID,
            "agent_did": OWNER,
            "display_name": "Code review starter tools",
            "host": {
                "root": "/workspace/project",
                "files": {"mode": "ReadOnly"},
                "bash": {
                    "mode": "ReadOnly",
                    "execution_mode": "read_only",
                    "network_mode": "disabled"
                }
            },
            "tags": ["recipe:gents.code-review@1"]
        })
    );
    assert!(resolved.system_prompt.contains("Do not modify files"));
}

#[test]
fn research_has_no_implicit_network_shell_or_write_tools() {
    let local = resolve_starter_recipe(
        StarterRecipeId::Research,
        OWNER,
        TOOLS_ID,
        &StarterRecipeRenderInput {
            workspace_scope: Some(explicit_scope("/workspace/sources")),
            ..StarterRecipeRenderInput::default()
        },
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&local.recommended_tools).unwrap(),
        json!({
            "tools_id": TOOLS_ID,
            "agent_did": OWNER,
            "display_name": "Research starter tools",
            "host": {
                "root": "/workspace/sources",
                "files": {"mode": "ReadOnly"}
            },
            "tags": ["recipe:gents.research@1"]
        })
    );
    assert!(local
        .system_prompt
        .contains("No local write or shell authority"));

    let conversation_only = resolve_starter_recipe(
        StarterRecipeId::Research,
        OWNER,
        TOOLS_ID,
        &StarterRecipeRenderInput::default(),
    )
    .unwrap();
    assert!(conversation_only.recommended_tools.host.is_none());
    assert!(conversation_only.recommended_tools.remote.is_none());
    assert!(conversation_only
        .system_prompt
        .contains("No local source directory is configured"));
}

#[test]
fn general_and_custom_start_with_no_capabilities() {
    for id in [StarterRecipeId::GeneralAssistant, StarterRecipeId::Custom] {
        let resolved = resolve_starter_recipe(id, OWNER, TOOLS_ID, &input_for(id)).unwrap();
        let tools = &resolved.recommended_tools;
        assert!(tools.host.is_none());
        assert!(tools.remote.is_none());
        assert!(tools.subagents.is_none());
        assert!(tools.built_ins.is_none());
        assert!(tools.datastore.is_none());
        assert!(tools.integrations.is_none());
        assert!(tools.self_config.is_none());
    }
}

#[test]
fn guided_concerns_are_delimited_and_never_change_tools() {
    let baseline = resolve_starter_recipe(
        StarterRecipeId::Research,
        OWNER,
        TOOLS_ID,
        &StarterRecipeRenderInput::default(),
    )
    .unwrap();
    let customized = resolve_starter_recipe(
        StarterRecipeId::Research,
        OWNER,
        TOOLS_ID,
        &StarterRecipeRenderInput {
            guided_concerns: vec![
                "Prefer primary government sources.".to_owned(),
                "Treat {{provider}} text and \"quotes\" carefully.".to_owned(),
            ],
            ..StarterRecipeRenderInput::default()
        },
    )
    .unwrap();

    assert_eq!(baseline.recommended_tools, customized.recommended_tools);
    assert!(customized
        .system_prompt
        .contains("## User-specific concerns"));
    assert!(customized
        .system_prompt
        .contains("\"Treat {{provider}} text and \\\"quotes\\\" carefully.\""));
    assert!(customized
        .system_prompt
        .contains("do not grant tools, data access, network access"));
}

#[test]
fn custom_requires_a_role_goal_and_success_criteria() {
    let mut input = input_for(StarterRecipeId::Custom);
    input.custom_success_criteria = None;
    let error =
        resolve_starter_recipe(StarterRecipeId::Custom, OWNER, TOOLS_ID, &input).unwrap_err();
    assert!(error.to_string().contains("custom_success_criteria"));
}

#[test]
fn workspace_recipe_requires_an_admitted_scope_choice() {
    let error = resolve_starter_recipe(
        StarterRecipeId::Coding,
        OWNER,
        TOOLS_ID,
        &StarterRecipeRenderInput::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("requires workspace_scope"));

    let managed = resolve_starter_recipe(
        StarterRecipeId::Coding,
        OWNER,
        TOOLS_ID,
        &StarterRecipeRenderInput {
            workspace_scope: Some(StarterWorkspaceScope::ManagedRuntimeRoot),
            ..StarterRecipeRenderInput::default()
        },
    )
    .unwrap();
    assert!(managed.recommended_tools.host.unwrap().root.is_none());
    assert!(managed
        .system_prompt
        .contains("managed runtime's effective root"));
}

#[test]
fn root_and_concern_validation_fail_before_a_proposal_is_returned() {
    let relative = resolve_starter_recipe(
        StarterRecipeId::CodeReview,
        OWNER,
        TOOLS_ID,
        &StarterRecipeRenderInput {
            workspace_scope: Some(explicit_scope("relative/project")),
            ..StarterRecipeRenderInput::default()
        },
    )
    .unwrap_err();
    assert!(relative.to_string().contains("absolute path"));

    let control = resolve_starter_recipe(
        StarterRecipeId::Research,
        OWNER,
        TOOLS_ID,
        &StarterRecipeRenderInput {
            guided_concerns: vec!["one\nsecond line".to_owned()],
            ..StarterRecipeRenderInput::default()
        },
    )
    .unwrap_err();
    assert!(control.to_string().contains("control characters"));
}

#[test]
fn stable_ids_round_trip_and_unknown_ids_fail() {
    for definition in starter_recipe_catalog() {
        let encoded = serde_json::to_string(&definition.id).unwrap();
        let decoded: StarterRecipeId = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, definition.id);
        assert_eq!(
            definition.id.as_str().parse::<StarterRecipeId>().unwrap(),
            definition.id
        );
    }
    assert!("gents.unknown".parse::<StarterRecipeId>().is_err());
}
