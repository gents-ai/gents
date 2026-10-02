//! The config tool's model-facing contract: short behavior IDs, receipts that
//! name their target, refused silent Tools drops, the validation gaps the
//! factory-setup audit found, and help recipes that run as written.

use super::tests::{build_persona_node, call_config_tool, config, persona_identity};
use super::*;

type Tools = Vec<Box<dyn ToolDyn>>;

async fn call(tools: &Tools, args: Value) -> Result<Value, String> {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    match tool.call(args.to_string()).await {
        Ok(text) => Ok(serde_json::from_str(&text).unwrap_or(Value::String(text))),
        Err(error) => Err(format!("{error:#}")),
    }
}

async fn ok(tools: &Tools, args: Value) -> Value {
    call(tools, args.clone())
        .await
        .unwrap_or_else(|error| panic!("{args}: {error}"))
}

async fn refused(tools: &Tools, args: Value) -> String {
    match call(tools, args.clone()).await {
        Ok(value) => panic!("{args} was accepted: {value}"),
        Err(error) => error,
    }
}

async fn setup(label: &str, categories: &[&str]) -> (Arc<EmbeddedNode>, String, Tools) {
    let node = build_persona_node().await;
    let identity = persona_identity(label);
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "beh-test").await;
    let mut grants = config(categories);
    grants.preview = true;
    let tools = build_self_config_tools(node.clone(), owner.clone(), Some(identity), &grants);
    (node, owner, tools)
}

#[tokio::test]
async fn behavior_ids_resolve_from_their_principal_local_slug() {
    let (node, owner, tools) = setup("slug", &["persona", "tools", "profile"]).await;
    let worker = format!("{owner}:worker-a");
    crate::test_support::install_test_behavior(&node, &owner, &worker).await;

    let read = ok(
        &tools,
        json!({"argv":["tools","get"],"options":{"behavior":"worker-a"}}),
    )
    .await;
    assert_eq!(read["document"]["tools_id"], format!("{worker}:tools"));
    let read = ok(&tools, json!({"argv":["behavior","get","worker-a"]})).await;
    assert!(read.to_string().contains(&worker), "{read}");

    // The receipt names the behavior and document the short ID selected.
    let receipt = ok(
        &tools,
        json!({"argv":["profile","preview"],"options":{"behavior":"worker-a"},"set":{"display_name":"Worker"}}),
    )
    .await;
    assert_eq!(receipt["behavior_id"], worker);
    assert_eq!(receipt["target_id"], format!("{worker}:inference"));

    // With several behaviors, a profile edit must name its target.
    let error = refused(
        &tools,
        json!({"argv":["profile","preview"],"set":{"display_name":"Mine"}}),
    )
    .await;
    assert!(
        error.contains("needs options.behavior") && error.contains(&worker),
        "{error}"
    );

    // A local SubagentTarget stores the resolved ID.
    ok(
        &tools,
        json!({"argv":["subagent-target","create"],"target_id":"worker","set":{"name":"worker","target_agent_did":owner,"behavior_id":"worker-a"}}),
    )
    .await;
    let target = ok(
        &tools,
        json!({"argv":["subagent-target","get"],"target_id":"worker"}),
    )
    .await;
    assert_eq!(target["document"]["behavior_id"], worker);

    // An exact ID wins over a slug that happens to match it.
    crate::test_support::install_test_behavior(&node, &owner, "worker-b").await;
    crate::test_support::install_test_behavior(&node, &owner, &format!("{owner}:worker-b")).await;
    let read = ok(
        &tools,
        json!({"argv":["tools","get"],"options":{"behavior":"worker-b"}}),
    )
    .await;
    assert_eq!(read["document"]["tools_id"], "worker-b:tools");

    // An unknown ID names the next call, with no connected-preview detour.
    let error = refused(
        &tools,
        json!({"argv":["tools","get"],"options":{"behavior":"nope"}}),
    )
    .await;
    assert!(
        error.contains("[\\\"behavior\\\",\\\"list\\\"]") && error.contains("next_call"),
        "{error}"
    );

    // Create derives the ID; an explicit one would be silently ignored.
    let error = refused(
        &tools,
        json!({"argv":["behavior","create"],"options":{"id":"x","display-name":"X","system-prompt":"p","preset":"readonly","profile":"beh-test:inference"}}),
    )
    .await;
    assert!(error.contains("takes no id"), "{error}");
}

#[tokio::test]
async fn profile_receipts_name_the_default_target_and_preview_edit_means_preview() {
    let (_node, _owner, tools) = setup("profile-target", &["persona", "profile"]).await;
    let receipt = ok(
        &tools,
        json!({"argv":["profile","preview"],"set":{"display_name":"Mine"}}),
    )
    .await;
    assert_eq!(receipt["behavior_id"], "beh-test");
    assert_eq!(receipt["target_id"], "beh-test:inference");
    let aliased = ok(
        &tools,
        json!({"argv":["profile","preview","edit"],"set":{"display_name":"Mine"}}),
    )
    .await;
    assert_eq!(aliased["committed"], false);
    assert_eq!(aliased["target_id"], "beh-test:inference");
}

/// #2133: verbs a model guesses resolve to the command they mean, or the
/// refusal shows the exact working form.
#[tokio::test]
async fn guessed_verbs_resolve_or_name_the_working_form() {
    let (_node, _owner, tools) = setup("verbs", &["persona", "tools", "automation"]).await;
    let task = |verb: &[&str]| {
        let mut argv = vec!["automation"];
        argv.extend_from_slice(verb);
        argv.push("task");
        json!({"argv":argv,"target_id":"review","set":{"prompt_template":"Review."}})
    };
    for verb in [&["preview", "edit"][..], &["preview", "create"][..]] {
        let receipt = ok(&tools, task(verb)).await;
        assert_eq!(receipt["committed"], false, "{verb:?}: {receipt}");
    }
    let created = ok(&tools, task(&["create"])).await;
    assert_eq!(created["committed"], true, "{created}");
    assert_eq!(created["target_id"], "review");

    let tools_preview = ok(
        &tools,
        json!({"argv":["tools","preview","edit"],"set":{"tags":["x"]}}),
    )
    .await;
    assert_eq!(tools_preview["committed"], false);
    for argv in [
        json!(["tools", "create"]),
        json!(["tools", "preview", "create"]),
    ] {
        let error = refused(&tools, json!({"argv":argv,"set":{"tags":["x"]}})).await;
        assert!(
            error.contains("tools create requires a new document ID"),
            "{error}"
        );
    }
    let schema = refused(
        &tools,
        json!({"argv":["schema","apply","install"],"options":{"sdl":"type A { a: String }"}}),
    )
    .await;
    assert!(schema.contains("schema tool"), "{schema}");
    let automation = refused(&tools, json!({"argv":["automation","apply","task"]})).await;
    assert!(
        automation.contains(r#"automation has no apply verb; use the previewed call with its target_id, options and set, removing \"preview\" from argv and putting \"edit\" in its place when no create or edit follows it"#),
        "{automation}"
    );
}

/// #2133: a decode error names the field path and its advertised shape, not
/// the Rust type serde expected.
#[tokio::test]
async fn type_errors_name_the_field_path_and_shape() {
    let (_node, _owner, tools) = setup("shapes", &["tools"]).await;
    let error = refused(
        &tools,
        json!({"argv":["tools","preview"],"set":{"integrations":{"lsp":"gents-mailbox"}}}),
    )
    .await;
    assert!(
        error.contains(r#"Tools field integrations.lsp: invalid type: string \"gents-mailbox\"; expected {\"config\":\"JSON encoded as a string|null\""#),
        "{error}"
    );
    assert!(!error.contains("LspTools"), "{error}");
    let nested = refused(
        &tools,
        json!({"argv":["tools","preview"],"set":{"host":{"cli":[{"name":7}]}}}),
    )
    .await;
    assert!(
        nested.contains(r#"Tools field host.cli[0].name: invalid type: integer `7`; expected string; host-registered CLI tool name"#),
        "{nested}"
    );
}

/// #2133: a target ID sent in argv and target_id is one ID when they agree.
#[test]
fn target_id_may_repeat_the_argv_id_but_not_conflict() {
    let params = |target_id: &str| {
        serde_json::from_value::<command::ConfigCommandParams>(json!({
            "argv": ["datastore", "edit", "handoff-tools"],
            "target_id": target_id,
        }))
        .unwrap()
    };
    assert_eq!(
        json!(params("handoff-tools").into_argv_for_test().unwrap()),
        json!(["datastore", "edit", "handoff-tools"])
    );
    let conflict = params("other")
        .into_argv_for_test()
        .unwrap_err()
        .to_string();
    assert_eq!(
        conflict,
        r#"target ID "handoff-tools" in argv conflicts with target_id "other"; send one of them"#
    );
}

#[tokio::test]
async fn own_tools_refuse_a_group_set_that_silently_drops_existing_settings() {
    let node = build_persona_node().await;
    let identity = persona_identity("tools-drop");
    let owner = identity.did().to_string();
    for behavior in ["setup", "worker"] {
        crate::test_support::install_test_behavior(&node, &owner, behavior).await;
    }
    let mut grants = config(&["persona", "tools"]);
    grants.behavior_id = "setup".into();
    grants.preview = true;
    grants.no_lockout = true;
    // Surfaces exist before the invoker's Tools gain the lockout-guarded grant.
    let mut unguarded = grants.clone();
    unguarded.no_lockout = false;
    let setup_tools = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity.clone()),
        &unguarded,
    );
    let tools = build_self_config_tools(node.clone(), owner.clone(), Some(identity), &grants);
    for surface in ["engineer-mailbox", "worker-surface"] {
        ok(
            &setup_tools,
            json!({"argv":["datastore","create"],"target_id":surface,"set":{"display_name":surface}}),
        )
        .await;
    }
    let setup_core = SelfConfigCore::new(node.clone(), owner.clone(), "setup".into()).unwrap();
    setup_core
        .apply(tools_request(
            &setup_core,
            vec![
                (
                    "self_config".into(),
                    Some(json!({"enable_self_config":true,"self_config_no_lockout":true})),
                ),
                ("subagents".into(), Some(json!({"enabled":true}))),
                (
                    "datastore".into(),
                    Some(json!({"enable_defra_query":true,"datastore_tool_surface_ids":["engineer-mailbox"]})),
                ),
            ],
            false,
        ))
        .await
        .unwrap();

    let partial = json!({"datastore_tool_surface_ids":["worker-surface"]});
    let error = refused(
        &tools,
        json!({"argv":["tools","preview"],"set":{"datastore":partial}}),
    )
    .await;
    assert!(
        error.contains("datastore.enable_defra_query")
            && error.contains("datastore.datastore_tool_surface_ids item \\\"engineer-mailbox\\\"")
            && error.contains("allow-drop"),
        "{error}"
    );
    let exact_error = refused(&tools, json!({"argv":["tools","preview","update"],"target_id":"setup:tools","set":{"datastore":partial}})).await;
    assert!(exact_error.contains("allow-drop"), "{exact_error}");
    let exact_lockout = refused(&tools, json!({"argv":["tools","update"],"target_id":"setup:tools","set":{"self_config":{"enable_self_config":false}}})).await;
    assert!(exact_lockout.contains("no-lockout"), "{exact_lockout}");
    // The lockout guard still reports a lockout as one.
    let error = refused(
        &tools,
        json!({"argv":["tools","preview"],"set":{"self_config":{"enable_self_config":false}}}),
    )
    .await;
    assert!(error.contains("no-lockout guard"), "{error}");
    // Carrying the group forward, or naming it, is accepted.
    ok(
        &tools,
        json!({"argv":["tools","preview"],"set":{"datastore":{"enable_defra_query":true,"datastore_tool_surface_ids":["engineer-mailbox","worker-surface"]}}}),
    )
    .await;
    ok(
        &tools,
        json!({"argv":["tools","preview"],"options":{"allow-drop":"datastore"},"set":{"datastore":partial}}),
    )
    .await;
    // Another behavior's Tools keep plain replacement semantics.
    ok(
        &tools,
        json!({"argv":["tools","preview"],"options":{"behavior":"worker"},"set":{"datastore":partial}}),
    )
    .await;
}

#[tokio::test]
async fn automation_validation_refuses_what_every_fire_would_reject() {
    let (node, _owner, tools) = setup("automation-gaps", &["automation"]).await;
    node.add_schema("type GapInput { message: String reply_session_id: String }")
        .await
        .unwrap();
    let edit = |kind: &str, id: &str, set: Value| json!({"argv":["automation","edit",kind],"target_id":id,"set":set});

    // An event source on a collection that does not exist never fires.
    let error = refused(
        &tools,
        edit(
            "event-source",
            "missing",
            json!({"source_collection":"NoSuchCollection"}),
        ),
    )
    .await;
    assert!(error.contains("not an installed collection"), "{error}");
    // A stringified object where a native one belongs names the field.
    let error = refused(
        &tools,
        edit(
            "event-source",
            "grouped",
            json!({"source_collection":"GapInput","correlation_field":"message","group":"{\"expected_count\":2}"}),
        ),
    )
    .await;
    assert!(
        error.contains("field \\\"group\\\" holds a JSON string; send a native JSON object"),
        "{error}"
    );
    // A native JSON object is not a filter string; the error names the field.
    let error = refused(
        &tools,
        edit(
            "event-source",
            "input",
            json!({"source_collection":"GapInput","filter":{"message":{"_eq":"x"}}}),
        ),
    )
    .await;
    assert!(
        error.contains("filter must be a string holding a GraphQL object literal"),
        "{error}"
    );
    // A JSON-quoted filter key parses as a string but fails every query.
    let error = refused(
        &tools,
        edit(
            "event-source",
            "input",
            json!({"source_collection":"GapInput","filter":"{\"message\": {\"_eq\": \"x\"}}"}),
        ),
    )
    .await;
    assert!(error.contains("unquoted GraphQL names"), "{error}");
    ok(
        &tools,
        edit(
            "event-source",
            "input",
            json!({"source_collection":"GapInput","filter":"{message: {_ne: \"\"}}"}),
        ),
    )
    .await;

    // #1970: a doc field the source collection lacks fails every fire.
    ok(
        &tools,
        edit(
            "task",
            "work",
            json!({"prompt_template":"Do {{ doc.message }}"}),
        ),
    )
    .await;
    ok(
        &tools,
        edit(
            "trigger",
            "on-input",
            json!({"task_id":"work","source":{"kind":"event","event_source_id":"input"},"session_id_template":"{{ doc.reply_session_id }}"}),
        ),
    )
    .await;
    let error = refused(
        &tools,
        edit(
            "task",
            "work",
            json!({"prompt_template":"Do {{ doc.not_a_field }}"}),
        ),
    )
    .await;
    assert!(
        !error.contains("COUNT")
            && error.contains("doc.not_a_field")
            && error.contains("message, reply_session_id"),
        "{error}"
    );
    // A defaulted field may be absent from this collection.
    ok(
        &tools,
        edit(
            "task",
            "work",
            json!({"prompt_template":"Do {{ doc.message }} {{ doc.priority | default('normal') }}"}),
        ),
    )
    .await;
    let error = refused(
        &tools,
        edit(
            "trigger",
            "on-input",
            json!({"session_id_template":"{{ doc.session }}"}),
        ),
    )
    .await;
    assert!(error.contains("doc.session"), "{error}");

    // #2080: schedule fires cannot target a session or queue.
    ok(
        &tools,
        edit(
            "schedule",
            "hourly",
            json!({"cadence":{"kind":"interval","interval_secs":3600}}),
        ),
    )
    .await;
    ok(
        &tools,
        edit("task", "tick", json!({"prompt_template":"Tick"})),
    )
    .await;
    for (field, value) in [
        ("session_id_template", json!("abc-session")),
        ("concurrency", json!("queued_serial")),
    ] {
        let mut set = json!({"task_id":"tick","source":{"kind":"schedule","schedule_id":"hourly"}});
        set[field] = value;
        let error = refused(&tools, edit("trigger", "hourly-tick", set)).await;
        assert!(error.contains("schedule source"), "{field}: {error}");
    }
    ok(
        &tools,
        edit(
            "trigger",
            "hourly-tick",
            json!({"task_id":"tick","source":{"kind":"schedule","schedule_id":"hourly"}}),
        ),
    )
    .await;
}

#[tokio::test]
async fn execution_deadlines_are_bounded_where_a_claim_can_represent_them() {
    let (_node, _owner, tools) = setup("deadline", &["profile"]).await;
    let create = |deadline: i64| json!({"argv":["execution","preview","create"],"target_id":"long","set":{"deadline_duration_secs":deadline}});
    let error = refused(&tools, create(i64::MAX)).await;
    assert!(error.contains("at most 3153600000"), "{error}");
    ok(
        &tools,
        create(crate::document_config::MAX_DEADLINE_DURATION_SECS),
    )
    .await;
}

#[tokio::test]
async fn help_is_layered_and_its_recipes_run_as_written() {
    let node = build_persona_node().await;
    let identity = persona_identity("recipes");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "setup").await;
    crate::test_support::install_test_behavior(&node, &owner, &format!("{owner}:worker")).await;
    let mut grants = config(&["persona", "tools", "profile", "automation"]);
    grants.behavior_id = "setup".into();
    grants.preview = true;
    let tools =
        build_self_config_tools(node.clone(), owner.clone(), Some(identity.clone()), &grants);

    // Pages are plain text of skill size; nothing from the index repeats.
    let index = call_config_tool(&tools, vec!["help".into()]).await.unwrap();
    assert!(index.len() < 2_000, "help index is {} chars", index.len());
    for resource in [
        "get",
        "behavior",
        "tools",
        "datastore",
        "subagent-target",
        "profile",
        "execution",
        "automation",
        "cleanup",
        "skill",
        "discovery",
        "validate",
    ] {
        let page = call_config_tool(&tools, vec!["help".into(), resource.into()])
            .await
            .unwrap();
        assert!(page.starts_with(&format!("{resource}: ")), "{page}");
        assert!(page.contains("\nNext: "), "{resource}: {page}");
        assert!(!page.contains("config_execution") && !page.contains("config resources."));
        assert!(
            page.len() <= 2_600,
            "{resource} help is {} chars",
            page.len()
        );
    }
    for alias in ["task", "trigger", "event-source", "agent", "graph"] {
        call_config_tool(&tools, vec!["help".into(), alias.into()])
            .await
            .unwrap_or_else(|error| panic!("{alias}: {error}"));
    }

    // Behavior create is applied by the persona reconciler.
    let store = crate::agent::p2p_reconcile::GraphqlPersonaRequestStore::with_local_identity(
        node.clone(),
        None,
        identity,
    );
    let ticker_node = node.clone();
    let ticker = tokio::spawn(async move {
        loop {
            let _ = crate::agent::p2p_reconcile::reconcile_persona_tick(&store, &ticker_node).await;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });
    let substitute = |call: &Value| -> Value {
        let text = call
            .to_string()
            .replace("<BACKEND_ID>", "setup:backend")
            .replace("<BEHAVIOR_ID>", "setup")
            .replace("<MODEL>", "test-model")
            .replace("<PROMPT>", "Lead the work.")
            .replace("<DID>", &owner);
        serde_json::from_str(&text).unwrap()
    };
    for resource in [
        "behavior",
        "execution",
        "datastore",
        "automation",
        "profile",
    ] {
        for (title, steps) in command::help::recipes(resource) {
            for (call, _) in steps {
                let call = substitute(&call);
                let result = if call["tool"] == "schema" {
                    let args = call["args"].clone();
                    let schema_tool = crate::schema_tool::SchemaTool::new(node.clone());
                    let preview = Tool::call(&schema_tool, serde_json::from_value(args).unwrap())
                        .await
                        .unwrap();
                    let preview: Value = serde_json::from_str(&preview).unwrap();
                    let applied = Tool::call(
                        &schema_tool,
                        serde_json::from_value(preview["next_call"]["args"].clone()).unwrap(),
                    )
                    .await
                    .unwrap();
                    serde_json::from_str(&applied).unwrap()
                } else {
                    ok(&tools, call.clone()).await
                };
                assert!(!result.is_null(), "{resource} recipe {title}");
            }
        }
    }
    ticker.abort();
    let lead = ok(&tools, json!({"argv":["behavior","get","lead"]})).await;
    assert!(
        lead.to_string().contains(&format!("{owner}:lead")),
        "{lead}"
    );
}

/// Model-facing results read top to bottom: the answer, then how to proceed,
/// then metadata. Pins top-level key order of help, receipts and errors.
#[tokio::test]
async fn results_put_the_answer_first_and_execution_metadata_last() {
    let (_node, _owner, tools) = setup("key-order", &["persona", "profile"]).await;
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let order = |text: &str, keys: &[&str]| {
        let positions = keys
            .iter()
            .map(|key| {
                text.find(&format!("\"{key}\""))
                    .unwrap_or_else(|| panic!("{key} missing: {text}"))
            })
            .collect::<Vec<_>>();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "{keys:?} out of order: {text}"
        );
    };

    let help = tool
        .call(json!({"argv":["help","profile"]}).to_string())
        .await
        .unwrap();
    assert!(help.starts_with("profile: "), "{help}");

    let receipt = tool
        .call(json!({"argv":["profile","preview"],"set":{"display_name":"Mine"}}).to_string())
        .await
        .unwrap();
    order(
        &receipt,
        &[
            "committed",
            "collection",
            "target_id",
            "behavior_id",
            "changed",
            "effect",
            "config_execution",
        ],
    );
    assert!(receipt.trim_end().ends_with('}') && serde_json::from_str::<Value>(&receipt).is_ok());

    let error = match tool
        .call(json!({"argv":["profile","preview"],"options":{"behavior":"nope"},"set":{"display_name":"x"}}).to_string())
        .await
    {
        Err(crate::llm::tool::ToolError::ToolCallError(error)) => error.to_string(),
        other => panic!("expected an error: {other:?}"),
    };
    order(&error, &["error", "recovery", "config_execution"]);
}

#[test]
fn a_list_of_strings_in_options_is_the_repeated_flag() {
    let params: command::ConfigCommandParams = serde_json::from_value(json!({
        "argv": ["cleanup", "preview"],
        "options": {"target": ["task=a", "trigger=b"]}
    }))
    .unwrap();
    assert_eq!(
        json!(params.into_argv_for_test().unwrap()),
        json!([
            "cleanup",
            "preview",
            "--target",
            "task=a",
            "--target",
            "trigger=b"
        ])
    );
}

#[tokio::test]
async fn lists_include_unselected_documents() {
    let (_node, _owner, tools) = setup("lists", &["persona", "tools", "automation"]).await;
    ok(
        &tools,
        json!({"argv":["datastore","create","unselected"],"set":{"entries":[]}}),
    )
    .await;
    for argv in [
        json!(["tools", "list"]),
        json!(["datastore", "list"]),
        json!(["automation", "list", "task"]),
    ] {
        let result = ok(&tools, json!({"argv":argv})).await;
        assert!(result["items"].is_array(), "{result}");
    }
    let surfaces = ok(&tools, json!({"argv":["datastore","list"]})).await;
    assert!(surfaces["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["surface_id"] == "unselected"));
}

/// Ladder finding: target_id on a command with a positional ID is that ID.
#[tokio::test]
async fn target_id_fills_a_positional_id() {
    let (_node, _owner, tools) = setup("positional", &["persona", "tools"]).await;
    for args in [
        json!({"argv":["behavior","get"],"target_id":"beh-test"}),
        json!({"argv":["behavior","get","beh-test"],"target_id":"beh-test"}),
    ] {
        let read = ok(&tools, args.clone()).await;
        assert_eq!(read["behavior_id"], "beh-test", "{args}");
    }
    let conflict = refused(
        &tools,
        json!({"argv":["behavior","get","beh-test"],"target_id":"other"}),
    )
    .await;
    assert!(
        conflict.contains(r#"target ID \"beh-test\" in argv conflicts with target_id \"other\""#),
        "{conflict}"
    );
    let unsupported = refused(&tools, json!({"argv":["tools","get"],"target_id":"x"})).await;
    assert!(unsupported.contains("no owned Tools"), "{unsupported}");
    let recovered = ok(
        &tools,
        json!({"argv":["tools","get"],"options":{"behavior":"beh-test"}}),
    )
    .await;
    assert_eq!(recovered["document"]["tools_id"], "beh-test:tools");
}

/// Mailbox suite: help states the identity choice where a policy is written,
/// and the monitor example uses condition identity.
#[tokio::test]
async fn mailbox_help_states_the_identity_choice() {
    let (_node, _owner, tools) = setup("mailbox-help", &["tools"]).await;
    let choice = "condition identity: one open item per stable finding, updated across requests; event identity: a new item for every request";
    for argv in [
        json!(["help", "datastore"]),
        json!(["datastore", "create", "--help"]),
    ] {
        let help = ok(&tools, json!({"argv":argv})).await;
        let help = help.as_str().unwrap();
        assert!(help.contains(choice), "{argv}: {help}");
    }
    let page = ok(&tools, json!({"argv":["help","datastore"]})).await;
    assert!(
        page.as_str().unwrap().contains(r#"A monitor uses condition identity: {"argv":["datastore","create"],"target_id":"monitor-mailbox","options":{"mailbox":{"identity":{"mode":"condition","key":"host-health"}"#),
        "{page}"
    );
}

/// L3 datastore finding: the page says what fill means, with a caller key
/// named correlation and a runtime-filled request_correlation.
#[tokio::test]
async fn datastore_help_says_what_fill_means() {
    let (_node, _owner, tools) = setup("fill-help", &["tools"]).await;
    let page = ok(&tools, json!({"argv":["help","datastore"]})).await;
    let page = page.as_str().unwrap();
    for line in [
        "fill: correlation uses the request/trigger correlation ID",
        "Omit fill for caller-supplied values",
        "omitted or empty means none",
        "current request keeps its existing tools",
        r#"Caller value: {"name":"correlation"}. Runtime ID: {"name":"request_correlation","fill":"correlation"}"#,
    ] {
        assert!(page.contains(line), "{line}\n{page}");
    }
    let shapes = ok(&tools, json!({"argv":["datastore","create","--help"]})).await;
    assert!(
        shapes
            .as_str()
            .unwrap()
            .contains(r#"fill fields are runtime-filled and never model arguments (what fill means: [\"help\",\"datastore\"])"#),
        "{shapes}"
    );
}

#[tokio::test]
async fn crud_documents_share_verbs_and_preserve_reference_and_identity_checks() {
    let (_node, owner, tools) = setup(
        "crud",
        &[
            "persona",
            "behavior",
            "tools",
            "profile",
            "automation",
            "mcp_service",
        ],
    )
    .await;
    for (resource, fields) in [
        ("sampling", json!({"temperature":0.8})),
        ("retry-policy", json!({"display_name":"Retry"})),
        ("compaction", json!({"threshold":0.8})),
        (
            "mcp-service",
            json!({"hostname":"localhost","mcp_port":9000}),
        ),
        (
            "skill",
            json!({"name":"Review","instructions":"Review carefully."}),
        ),
        ("datastore", json!({"entries":[]})),
        ("tools", json!({})),
        ("context", json!({"system_prompt":"Analyze."})),
    ] {
        let id = format!("crud-{resource}");
        let create = json!({"argv":[resource,"create"],"target_id":id,"set":fields});
        let mut preview = create.clone();
        preview["argv"] = json!([resource, "preview", "create"]);
        assert_eq!(ok(&tools, preview).await["committed"], false);
        assert!(
            refused(&tools, json!({"argv":[resource,"get"],"target_id":id}))
                .await
                .contains("no owned")
        );
        assert_eq!(ok(&tools, create.clone()).await["created"], true);
        assert!(refused(&tools, create).await.contains("already exists"));
        let changed = ok(
            &tools,
            json!({"argv":[resource,"update"],"target_id":id,"set":{"tags":["crud"]}}),
        )
        .await;
        assert_eq!(changed["created"], false);
        let document = ok(&tools, json!({"argv":[resource,"get"],"target_id":id})).await;
        assert_eq!(document["document"]["agent_did"], owner);
        assert_eq!(document["document"]["tags"], json!(["crud"]));
        let inventory = ok(
            &tools,
            json!({"argv":[resource,"list"],"options":{"limit":50}}),
        )
        .await;
        assert!(inventory["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["tags"] == json!(["crud"])));
        assert!(refused(
            &tools,
            json!({"argv":[resource,"update"],"target_id":id,"set":{"agent_did":"other"}})
        )
        .await
        .contains("protected"));
        assert!(refused(
            &tools,
            json!({"argv":[resource,"update"],"target_id":"absent","set":{"tags":["no"]}})
        )
        .await
        .contains("no owned"));
        let preview = ok(
            &tools,
            json!({"argv":[resource,"preview","delete"],"target_id":id}),
        )
        .await;
        ok(
            &tools,
            json!({"argv":[resource,"update"],"target_id":id,"set":{"tags":["changed"]}}),
        )
        .await;
        assert!(refused(&tools,json!({"argv":[resource,"delete"],"target_id":id,"options":{"digest":preview["plan_digest"]}})).await.contains("changed"));
        let preview = ok(
            &tools,
            json!({"argv":[resource,"preview","delete"],"target_id":id}),
        )
        .await;
        ok(&tools, preview["apply_with"].clone()).await;
        assert!(
            refused(&tools, json!({"argv":[resource,"get"],"target_id":id}))
                .await
                .contains("no owned")
        );
    }
    ok(
        &tools,
        json!({"argv":["sampling","create","selected"],"set":{"temperature":1}}),
    )
    .await;
    let profile = ok(&tools, json!({"argv":["profile","get"]})).await;
    let id = profile["document"]["profile_id"].as_str().unwrap();
    ok(
        &tools,
        json!({"argv":["profile","update"],"target_id":id,"set":{"sampling_id":"selected"}}),
    )
    .await;
    let refused_delete = refused(
        &tools,
        json!({"argv":["sampling","preview","delete","selected"]}),
    )
    .await;
    assert!(
        refused_delete.contains("Deletion would leave a broken reference"),
        "{refused_delete}"
    );
    assert!(refused_delete.contains("remove or redirect the reference"));
    assert!(!refused_delete.contains("create it"));
    let before = ok(&tools, json!({"argv":["profile","get",id]})).await;
    assert!(refused(
        &tools,
        json!({"argv":["profile","update",id],"set":{"sampling_id":"missing"}})
    )
    .await
    .contains("missing"));
    assert_eq!(
        ok(&tools, json!({"argv":["profile","get",id]})).await,
        before
    );
}

#[tokio::test]
async fn crud_batch_reports_partial_commits_and_stops_before_later_operations() {
    let (_node, _owner, tools) = setup("crud-batch", &["profile"]).await;
    let error = refused(
        &tools,
        json!({"argv":["batch"],"options":{"operations":[
            {"argv":["sampling","create","first"],"set":{"temperature":1}},
            {"argv":["sampling","update","missing"],"set":{"temperature":0.5}},
            {"argv":["sampling","create","last"],"set":{"temperature":0.5}}
        ]}}),
    )
    .await;
    assert!(
        error.contains("earlier items") && error.contains("unattempted"),
        "{error}"
    );
    assert_eq!(
        ok(&tools, json!({"argv":["sampling","get","first"]})).await["document"]["temperature"],
        1.0
    );
    assert!(refused(&tools, json!({"argv":["sampling","get","last"]}))
        .await
        .contains("no owned"));
    let resumed = ok(
        &tools,
        json!({"argv":["batch"],"options":{"operations":[
            {"argv":["sampling","update","first"],"set":{"temperature":0.5}},
            {"argv":["sampling","create","last"],"set":{"temperature":0.5}},
            {"argv":["sampling","list"],"options":{"limit":1}}
        ]}}),
    )
    .await;
    assert_eq!(resumed["completed"], true);
    assert_eq!(resumed["results"].as_array().unwrap().len(), 3);
    assert_eq!(
        resumed["results"][2]["config_execution"]["mutation_entered"],
        false
    );
    let cursor = &resumed["results"][2]["result"]["page"]["next_cursor"];
    assert!(cursor.is_string());
    let next = ok(
        &tools,
        json!({"argv":["sampling","list"],"options":{"limit":1,"cursor":cursor}}),
    )
    .await;
    assert_eq!(next["items"].as_array().unwrap().len(), 1);
    let denied = refused(&tools,json!({"argv":["batch"],"options":{"operations":[{"argv":["mcp-service","create","denied"]}]}})).await;
    assert!(denied.contains("not granted"));
    let malformed = refused(
        &tools,
        json!({"argv":["batch"],"options":{"operations":[
            {"argv":["sampling","create","never"]}, {"argv":["batch"],"options":{"operations":[]}}
        ]}}),
    )
    .await;
    assert!(malformed.contains("no operations ran"));
    assert!(refused(&tools, json!({"argv":["sampling","get","never"]}))
        .await
        .contains("no owned"));
}

#[tokio::test]
async fn crud_automation_keeps_strict_creation_and_selected_task_owner() {
    let (node, owner, tools) = setup("crud-routing", &["persona", "automation"]).await;
    let worker = format!("{owner}:worker");
    crate::test_support::install_test_behavior(&node, &owner, &worker).await;
    for (resource, id, fields) in [
        (
            "task",
            "job",
            json!({"prompt_template":"Reply with the result."}),
        ),
        (
            "schedule",
            "clock",
            json!({"cadence":{"kind":"interval","interval_secs":60}}),
        ),
        (
            "event-source",
            "messages",
            json!({"source_collection":"AgentRequest"}),
        ),
        (
            "trigger",
            "tick",
            json!({"task_id":"job","source":{"kind":"schedule","schedule_id":"clock"}}),
        ),
    ] {
        let create = json!({"argv":[resource,"create"],"target_id":id,"options":{"behavior":worker},"set":fields});
        let mut preview = create.clone();
        preview["argv"] = json!([resource, "preview", "create"]);
        assert_eq!(ok(&tools, preview).await["committed"], false);
        ok(&tools, create.clone()).await;
        assert!(refused(&tools, create).await.contains("already exists"));
        ok(&tools,json!({"argv":[resource,"update",id],"options":{"behavior":worker},"set":{"tags":["routing"]}})).await;
        assert_eq!(
            ok(&tools, json!({"argv":[resource,"get",id]})).await["document"]["tags"],
            json!(["routing"])
        );
    }
    for (resource, id) in [("task", "job"), ("trigger", "tick")] {
        let changed = ok(
            &tools,
            json!({"argv":[resource,"update",id],"set":{"tags":["inferred-owner"]}}),
        )
        .await;
        assert_eq!(changed["behavior_id"], worker);
    }
    assert!(refused(
        &tools,
        json!({"argv":["task","update","job"],"options":{"behavior":"beh-test"},"set":{"prompt_template":"Wrong owner."}})
    )
    .await
    .contains("belongs to behavior"));
    let failure = structured_failure(
        &tools,
        json!({"argv":["trigger","update","tick"],"options":{"behavior":"beh-test"},"set":{"tags":["wrong"]}})
    ).await;
    let message = failure["error"].as_str().unwrap();
    assert!(message.contains("selected behavior"), "{failure}");
    assert!(!message.contains("\"plan\""), "{failure}");
    assert!(message.contains("separate Trigger"), "{failure}");
    ok(&tools, failure["recovery"]["next_call"].clone()).await;
    assert!(
        refused(&tools, json!({"argv":["task","preview","delete","job"]}))
            .await
            .contains("job")
    );
    for (resource, id) in [
        ("trigger", "tick"),
        ("task", "job"),
        ("schedule", "clock"),
        ("event-source", "messages"),
    ] {
        let preview = ok(&tools, json!({"argv":[resource,"preview","delete",id]})).await;
        ok(
            &tools,
            json!({"argv":[resource,"delete",id],"options":{"digest":preview["plan_digest"]}}),
        )
        .await;
    }
}

#[tokio::test]
async fn crud_exact_context_and_tools_preserve_shared_document_guards() {
    let (node, owner, tools) = setup("crud-sharing", &["persona", "tools", "behavior"]).await;
    let worker = format!("{owner}:worker");
    crate::test_support::install_test_behavior(&node, &owner, &worker).await;
    let context_id = format!("{worker}:context");
    let tools_id = format!("{worker}:tools");
    ok(&tools,json!({"argv":["context","update"],"target_id":context_id,"set":{"system_prompt":"Review clearly."}})).await;
    ok(
        &tools,
        json!({"argv":["tools","update"],"target_id":tools_id,"set":{"tags":["review"]}}),
    )
    .await;
    ok(
        &tools,
        json!({"argv":["behavior","update","beh-test"],"set":{"context_id":context_id}}),
    )
    .await;
    for (resource, id) in [("context", context_id), ("tools", tools_id)] {
        assert!(refused(
            &tools,
            json!({"argv":[resource,"update",id],"set":{"tags":["shared"]}})
        )
        .await
        .contains("unshared"));
    }
}

#[tokio::test]
async fn crud_backend_creation_preserves_auth_boundary() {
    let (_node, _owner, tools) = setup("crud-backend", &["backend"]).await;
    ok(
        &tools,
        json!({"argv":["backend","create","local"],"set":{"endpoint":"http://127.0.0.1:8000/v1"}}),
    )
    .await;
    ok(
        &tools,
        json!({"argv":["backend","update","local"],"set":{"name":"Local inference"}}),
    )
    .await;
    let read = ok(&tools, json!({"argv":["backend","get","local"]})).await;
    assert_eq!(read["document"]["name"], "Local inference");
    assert_eq!(read["document"]["auth"]["redacted"], true);
    assert!(refused(&tools,json!({"argv":["backend","create","keyed"],"set":{"endpoint":"http://127.0.0.1:8000/v1","auth":{"kind":"api_key","key":"test-only"}}})).await.contains("unauthenticated"));
}

#[tokio::test]
async fn crud_help_exposes_parameters_for_the_named_resource_and_verb() {
    let (_node, _owner, tools) =
        setup("crud-help", &["persona", "tools", "profile", "automation"]).await;
    for (resource, field) in [
        ("context", "system_prompt"),
        ("sampling", "temperature"),
        ("retry-policy", "max_transport_retries"),
        ("compaction", "threshold"),
        ("task", "prompt_template"),
    ] {
        let text = call_config_tool(
            &tools,
            vec![resource.into(), "update".into(), "--help".into()],
        )
        .await
        .unwrap();
        assert!(text.contains(field), "{text}");
    }
    let nested = call_config_tool(
        &tools,
        vec![
            "behavior".into(),
            "context".into(),
            "edit".into(),
            "--help".into(),
        ],
    )
    .await
    .unwrap();
    assert!(
        nested.contains("system_prompt") && !nested.contains("inference_profile_id"),
        "{nested}"
    );
    let delete = call_config_tool(
        &tools,
        vec!["skill".into(), "delete".into(), "--help".into()],
    )
    .await
    .unwrap();
    assert!(delete.contains("options.digest"));
    let list = call_config_tool(
        &tools,
        vec!["sampling".into(), "list".into(), "--help".into()],
    )
    .await
    .unwrap();
    assert!(list.contains("options.cursor") && list.contains("1..50"));
    for resource in [
        "session",
        "request",
        "message",
        "AgentSession",
        "AgentRequest",
    ] {
        assert!(refused(&tools, json!({"argv":[resource,"list"]}))
            .await
            .contains("unknown config resource"));
    }
}

#[tokio::test]
async fn local_target_creation_defaults_identity_but_updates_preserve_remote_destination() {
    let (node, owner, tools) = setup("target-default", &["persona", "tools"]).await;
    let helper = format!("{owner}:helper");
    crate::test_support::install_test_behavior(&node, &owner, &helper).await;
    let receipt = ok(&tools, json!({"argv":["subagent-target","create"],"target_id":"local","set":{"name":"helper","behavior_id":"helper"}})).await;
    assert_eq!(receipt["connection"]["kind"], "local");
    assert_eq!(receipt["connection"]["target_agent_did"], owner);
    assert_eq!(receipt["connection"]["behavior_id"], helper);
    assert_eq!(receipt["connection"]["runtime_verified"], false);
    let local = ok(&tools, json!({"argv":["subagent-target","get","local"]})).await;
    assert_eq!(local["document"]["target_agent_did"], owner);
    assert_eq!(local["document"]["behavior_id"], helper);

    assert!(call(&tools, json!({"argv":["subagent-target","create"],"target_id":"missing","set":{"name":"missing","behavior_id":"does-not-exist"}})).await.is_err());
    assert!(
        call(&tools, json!({"argv":["subagent-target","get","missing"]}))
            .await
            .is_err()
    );
    let remote = persona_identity("remote-target-owner").did().to_string();
    ok(&tools, json!({"argv":["subagent-target","create"],"target_id":"remote","set":{"name":"remote","target_agent_did":remote,"behavior_id":"helper"}})).await;
    let changed = ok(&tools, json!({"argv":["subagent-target","update"],"target_id":"remote","set":{"description":"Remote helper"}})).await;
    assert_eq!(changed["connection"]["kind"], "remote");
    assert_eq!(changed["connection"]["target_agent_did"], remote);
    assert_eq!(changed["connection"]["behavior_id"], "helper");
    let changed = ok(&tools, json!({"argv":["subagent-target","update"],"target_id":"remote","set":{"behavior_id":"helper-2"}})).await;
    assert_eq!(changed["connection"]["target_agent_did"], remote);
    assert_eq!(changed["connection"]["behavior_id"], "helper-2");
    assert!(call(&tools, json!({"argv":["subagent-target","update"],"target_id":"remote","clear":["target_agent_did"]})).await.is_err());
    let retained = ok(&tools, json!({"argv":["subagent-target","get","remote"]})).await;
    assert_eq!(retained["document"]["target_agent_did"], remote);
}

#[tokio::test]
async fn profile_receipt_selects_current_behavior_without_changing_original_profile() {
    let (_node, _owner, tools) = setup("selection-receipt", &["persona", "profile"]).await;
    let before = ok(
        &tools,
        json!({"argv":["profile","get","beh-test:inference"]}),
    )
    .await;
    let receipt = ok(&tools, json!({"argv":["profile","create"],"target_id":"research","set":{"backend_id":"beh-test:backend","model_name":"test-model"}})).await;
    assert_eq!(receipt["connection"]["behavior_id"], "beh-test");
    assert_eq!(
        receipt["connection"]["selected_profile_id"],
        "beh-test:inference"
    );
    assert_eq!(receipt["connection"]["selected"], false);
    ok(&tools, receipt["connection"]["select_with"].clone()).await;
    let changed = ok(&tools, json!({"argv":["profile","update"],"target_id":"research","set":{"display_name":"Research"}})).await;
    assert_eq!(changed["connection"]["selected"], true);
    assert!(changed["connection"].get("select_with").is_none());
    let after = ok(
        &tools,
        json!({"argv":["profile","get","beh-test:inference"]}),
    )
    .await;
    assert_eq!(before["document"], after["document"]);
}

async fn structured_failure(tools: &Tools, args: Value) -> Value {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    match tool.call(args.to_string()).await {
        Err(crate::llm::tool::ToolError::ToolCallError(error)) => {
            serde_json::from_str(&error.to_string()).unwrap()
        }
        other => panic!("expected structured failure for {args}: {other:?}"),
    }
}

#[tokio::test]
async fn config_usage_errors_return_executable_resource_verb_recovery() {
    let (_node, _owner, tools) = setup("usage-recovery", &["persona", "tools", "automation"]).await;
    for args in [
        json!({"argv":["get"],"target_id":"beh-test"}),
        json!({"argv":["get","beh-test"]}),
        json!({"argv":["trigger","state","t"]}),
        json!({"argv":["schema","list"]}),
        json!({"argv":["behavior","clone","beh-test"]}),
        json!({"argv":["behavior","clone"],"options":{"source":"beh-test"}}),
    ] {
        let failure = structured_failure(&tools, args).await;
        assert_eq!(failure["config_execution"]["mutation_entered"], false);
        let recovery = &failure["recovery"]["next_call"];
        assert_ne!(recovery["argv"][0], "get");
        ok(&tools, recovery.clone()).await;
    }
    let failure = structured_failure(
        &tools,
        json!({"argv":["batch"],"options":{"operations":[{"argv":["get","beh-test"]}]}}),
    )
    .await;
    ok(
        &tools,
        failure["batch"]["results"][0]["recovery"]["next_call"].clone(),
    )
    .await;
}

#[tokio::test]
async fn saved_config_audit_reports_broken_references_without_mutation_and_accepts_repair() {
    let (node, owner, tools) = setup("saved-audit", &["persona", "tools"]).await;
    let valid = ok(&tools, json!({"argv":["validate"]})).await;
    assert_eq!(valid["valid"], true);
    assert!(valid["checked_documents"].as_u64().unwrap() > 0);
    assert_eq!(valid["collections"]["AgentBehavior"], 1);
    let owner = crate::graphql::escape_graphql_string(&owner);
    crate::ConfigAccess::write_local(&node, "test.audit.corrupt", &format!(
        r#"mutation {{ update_AgentContext(filter: {{agent_did: {{_eq: "{owner}"}}, context_id: {{_eq: "beh-test:context"}}}}, input: {{tools_id: "absent-tools"}}) {{context_id}} }}"#
    )).await.unwrap();
    let access = crate::ConfigAccess::Local(node.clone());
    let query = format!(
        r#"{{ AgentContext(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{context_id tools_id}} }}"#
    );
    let before = access.execute(&query).await.unwrap();
    let invalid = ok(&tools, json!({"argv":["validate"]})).await;
    assert_eq!(invalid["valid"], false, "{invalid}");
    assert_eq!(invalid["committed"], false);
    assert_eq!(invalid["config_execution"]["mutation_entered"], false);
    assert_eq!(invalid["errors"][0]["collection"], "AgentContext");
    assert_eq!(invalid["errors"][0]["field"], "tools_id");
    assert_eq!(invalid["errors"][0]["missing"]["id"], "absent-tools");
    assert_eq!(before, access.execute(&query).await.unwrap());
    ok(&tools, invalid["errors"][0]["inspect_with"].clone()).await;
    ok(&tools, json!({"argv":["context","update"],"target_id":"beh-test:context","set":{"tools_id":"beh-test:tools"}})).await;
    assert_eq!(
        ok(&tools, json!({"argv":["validate"]})).await["valid"],
        true
    );
}

#[tokio::test]
async fn saved_config_audit_is_principal_scoped_and_does_not_require_preview() {
    let (node, owner, tools) = setup("audit-scope", &["persona"]).await;
    let foreign = persona_identity("audit-foreign").did().to_string();
    crate::ConfigAccess::write_local(&node, "test.audit.foreign", &format!(
        r#"mutation {{create_AgentContext(input: {{agent_did: "{}", context_id: "foreign-context", tools_id: "missing-foreign-tools"}}) {{context_id}}}}"#,
        crate::graphql::escape_graphql_string(&foreign)
    )).await.unwrap();
    let valid = ok(&tools, json!({"argv":["validate"]})).await;
    assert_eq!(valid["valid"], true, "{valid}");
    assert_eq!(valid["collections"]["AgentContext"], 1);
    assert!(!valid.to_string().contains("foreign-context"));
    let identity = persona_identity("audit-scope");
    for (categories, allowed) in [(&["persona"][..], true), (&["tools"][..], false)] {
        let mut grants = config(categories);
        grants.preview = false;
        let tools =
            build_self_config_tools(node.clone(), owner.clone(), Some(identity.clone()), &grants);
        assert_eq!(
            call(&tools, json!({"argv":["validate"]})).await.is_ok(),
            allowed
        );
        let help = ok(&tools, json!({"argv":["help"]})).await;
        assert_eq!(help.as_str().unwrap().contains("  validate:"), allowed);
        assert!(!help.as_str().unwrap().contains("  plan:"));
    }
    assert!(call(
        &tools,
        json!({"argv":["validate"],"set":{"display_name":"ignored"}})
    )
    .await
    .is_err());
    crate::ConfigAccess::write_local(&node, "test.audit.restricted", &format!(
        r#"mutation {{update_InferenceProfile(filter: {{agent_did: {{_eq: "{}"}}, profile_id: {{_eq: "beh-test:inference"}}}}, input: {{backend_id: "missing-backend"}}) {{profile_id}}}}"#,
        crate::graphql::escape_graphql_string(&owner)
    )).await.unwrap();
    let invalid = ok(&tools, json!({"argv":["validate"]})).await;
    assert_eq!(invalid["valid"], false);
    assert_eq!(invalid["errors"][0]["collection"], "InferenceProfile");
    assert!(invalid["errors"][0].get("inspect_with").is_none());
}

#[tokio::test]
async fn config_error_recovery_is_executable_for_single_and_batch_calls() {
    let (_node, _owner, tools) = setup(
        "error-guidance",
        &["persona", "tools", "profile", "automation"],
    )
    .await;
    let before = ok(&tools, json!({"argv":["behavior","get"]})).await;
    for args in [
        json!({"argv":["behavior","create"],"set":{"display_name":"Helper"}}),
        json!({"argv":["context","get"],"target_id":"absent-context"}),
        json!({"argv":["tools","update"],"target_id":"absent-tools","set":{"display_name":"Missing"}}),
        json!({"argv":["tools","preview","update"],"target_id":"absent-tools","set":{"display_name":"Missing"}}),
        json!({"argv":["task","preview","remove","absent-task"]}),
        json!({"argv":["tools","update"],"set":{"bash":{}}}),
        json!({"argv":["behavior","update"],"target_id":"beh-test","set":{"system_prompt":"new instructions"}}),
    ] {
        let failure = structured_failure(&tools, args.clone()).await;
        let recovery = failure["recovery"]["next_call"].clone();
        assert!(recovery.is_object(), "{failure}");
        ok(&tools, recovery.clone()).await;
        let batch = structured_failure(&tools, json!({"argv":["batch"],"options":{"operations":[args, {"argv":["behavior","update"],"target_id":"beh-test","set":{"display_name":"must not execute"}}]}})).await;
        assert_eq!(batch["batch"]["failed_index"], 0);
        assert_eq!(batch["batch"]["unattempted"], 1);
        assert_eq!(
            batch["batch"]["results"][0]["recovery"]["next_call"],
            recovery
        );
        ok(
            &tools,
            batch["batch"]["results"][0]["recovery"]["next_call"].clone(),
        )
        .await;
    }
    let after = ok(&tools, json!({"argv":["behavior","get"]})).await;
    assert_eq!(
        before, after,
        "failed calls and recovery reads must preserve configuration"
    );
}

#[tokio::test]
async fn trigger_decode_errors_name_the_nested_field_and_offer_working_help() {
    let (_node, _owner, tools) = setup("nested-guidance", &["persona", "automation"]).await;
    ok(&tools, json!({"argv":["task","create"],"target_id":"inspect","set":{"prompt_template":"Inspect the input"}})).await;
    let failure = structured_failure(&tools, json!({"argv":["trigger","create"],"target_id":"inspect-trigger","set":{"task_id":"inspect","source":{"event_source_id":"input"}}})).await;
    let error = failure["error"].as_str().unwrap();
    assert!(
        error.contains("source") && error.contains("kind"),
        "{failure}"
    );
    let help = ok(&tools, failure["recovery"]["next_call"].clone()).await;
    assert!(help.as_str().unwrap().contains("kind"), "{help}");
    let saved = ok(&tools, json!({"argv":["trigger","list"]})).await;
    assert!(
        saved["items"].as_array().unwrap().is_empty(),
        "invalid trigger was not published"
    );
}

#[tokio::test]
async fn delete_recovery_previews_the_same_resource_before_committing() {
    let (_node, _owner, tools) = setup("delete-recovery", &["persona", "automation"]).await;
    ok(&tools, json!({"argv":["task","create"],"target_id":"disposable","set":{"prompt_template":"Review the input"}})).await;
    let failure = structured_failure(
        &tools,
        json!({"argv":["task","delete"],"target_id":"disposable"}),
    )
    .await;
    assert_eq!(
        failure["recovery"]["next_call"],
        json!({"argv":["task","preview","delete"],"target_id":"disposable"})
    );
    let preview = ok(&tools, failure["recovery"]["next_call"].clone()).await;
    ok(
        &tools,
        json!({"argv":["task","get"],"target_id":"disposable"}),
    )
    .await;
    ok(&tools, preview["apply_with"].clone()).await;
    assert!(call(
        &tools,
        json!({"argv":["task","get"],"target_id":"disposable"})
    )
    .await
    .is_err());
}

#[tokio::test]
async fn composition_help_and_field_placement_match_the_accepted_calls() {
    let (_node, _owner, tools) = setup(
        "composition-help",
        &["persona", "tools", "profile", "automation"],
    )
    .await;
    let bound = ok(
        &tools,
        json!({"argv":["context","get"],"options":{"behavior":"beh-test"}}),
    )
    .await;
    let exact = ok(
        &tools,
        json!({"argv":["context","get"],"target_id":bound["document"]["context_id"]}),
    )
    .await;
    assert_eq!(bound["document"], exact["document"]);
    for call in [
        json!({"argv":["execution","create"],"target_id":"limits","options":{"display_name":"Limits"}}),
        json!({"argv":["context","update"],"target_id":"absent-context","set":{"system-prompt":"Instructions"}}),
        json!({"argv":["behavior","create"],"target_id":"helper","options":{"display-name":"Helper"}}),
    ] {
        let error = structured_failure(&tools, call).await;
        ok(&tools, error["recovery"]["next_call"].clone()).await;
    }
    for verb in ["create", "clone"] {
        let help = ok(&tools, json!({"argv":["behavior",verb,"--help"]}))
            .await
            .to_string();
        assert!(help.contains("ID is allocated"), "{help}");
        assert!(
            !help.contains("AgentBehavior fields") && !help.contains("AgentContext fields"),
            "{help}"
        );
    }
    for argv in [
        json!(["help", "tools"]),
        json!(["tools", "update", "--help"]),
    ] {
        let help = ok(&tools, json!({"argv":argv})).await.to_string();
        assert!(help.contains("Do not combine them"), "{help}");
    }
    let error = structured_failure(&tools, json!({"argv":["execution","create"],"target_id":"limits","options":{"display_name":"Limits"}})).await;
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("set.display_name"),
        "{error}"
    );
    ok(
        &tools,
        json!({"argv":["execution","create"],"target_id":"limits","set":{"display_name":"Limits"}}),
    )
    .await;
    let error = structured_failure(&tools, json!({"argv":["event-source","create"],"target_id":"source","set":{"concurrency":"parallel"}})).await;
    assert!(
        error["error"].as_str().unwrap().contains("Trigger"),
        "{error}"
    );
}

#[tokio::test]
async fn saved_config_audit_checks_selected_schema_fields_and_accepts_updates() {
    let (node, _owner, tools) = setup("saved-schema-audit", &["persona", "tools"]).await;
    node.add_schema("type AuditRecord { message: String }")
        .await
        .unwrap();
    let entries = json!([
        {"kind":"create","tool_name":"write_audit_record","collection":"AuditRecord","description":"Write a record","fields":[{"name":"message","required":true},{"name":"correlation","fill":"correlation"}]},
        {"kind":"query","tool_name":"find_audit_records","collection":"AuditRecord","description":"Read records","fields":["message","missing_projection"],"filter_fields":[{"name":"missing_filter"}]}
    ]);
    ok(
        &tools,
        json!({"argv":["datastore","create","audit"],"set":{"entries":entries}}),
    )
    .await;
    // Unselected declarations may precede schema installation.
    assert_eq!(
        ok(&tools, json!({"argv":["validate"]})).await["valid"],
        true
    );
    ok(&tools, json!({"argv":["tools","update","beh-test:tools"],"set":{"datastore":{"datastore_tool_surface_ids":["audit"]}}})).await;
    let before = ok(&tools, json!({"argv":["datastore","get","audit"]})).await;
    let invalid = ok(&tools, json!({"argv":["validate"]})).await;
    assert_eq!(invalid["valid"], false, "{invalid}");
    assert_eq!(invalid["committed"], false);
    let errors = invalid["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 2, "{invalid}");
    assert!(errors[0]["error"]
        .as_str()
        .unwrap()
        .contains("field `correlation` is absent from `AuditRecord`"));
    assert!(errors[1]["error"]
        .as_str()
        .unwrap()
        .contains("missing_projection"));
    assert_eq!(errors[0]["tool_name"], "write_audit_record");
    ok(&tools, errors[0]["inspect_with"].clone()).await;
    assert_eq!(
        before,
        ok(&tools, json!({"argv":["datastore","get","audit"]})).await
    );
    let corrected = json!([
        {"kind":"create","tool_name":"write_audit_record","collection":"AuditRecord","description":"Write a record","fields":[{"name":"message","required":true}]},
        {"kind":"query","tool_name":"find_audit_records","collection":"AuditRecord","description":"Read records","fields":["message"],"filter_fields":[{"name":"missing_filter"}]}
    ]);
    ok(
        &tools,
        json!({"argv":["datastore","update","audit"],"set":{"entries":corrected}}),
    )
    .await;
    let invalid = ok(&tools, json!({"argv":["validate"]})).await;
    assert!(invalid["errors"][0]["error"]
        .as_str()
        .unwrap()
        .contains("missing_filter"));
    let mut corrected = corrected;
    corrected[1]["filter_fields"] = json!([{"name":"message"}]);
    ok(
        &tools,
        json!({"argv":["datastore","update","audit"],"set":{"entries":corrected}}),
    )
    .await;
    assert_eq!(
        ok(&tools, json!({"argv":["validate"]})).await["valid"],
        true
    );
}
