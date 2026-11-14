use super::*;
use gents_automation::test_support::self_config::*;
use gents_toolcalls::test_support::self_config::{config_call, node_identity};
use gents_tools::builtin::self_config::test_support::{
    agent_inspect, agent_request, anchored_request, backend_request, profile_create_request,
    profile_request, profile_target_request, protect_working_agent, tools_request,
};
#[path = "tests/live_documents.rs"]
mod live_documents;
#[path = "tests/pack_authoring.rs"]
mod pack_authoring;
#[cfg(target_os = "macos")]
static PLUGIN_BUILDER_STUB: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn effective_config_reports_inherited_workspace_network_as_disabled() {
    let fixture =
        gents_automation::test_support::self_config_fixture("workspace-network-report").await;
    fixture.grant_selected_configuration().await;
    let root = tempfile::tempdir().unwrap();
    fixture.publish_workspace_root(root.path()).await;
    let core =
        fixture
            .core()
            .with_process_ceiling(gents_tools::surface::SelfConfigProcessCeiling {
                file_mode: gents_documents::FileToolMode::ReadWrite,
                bash_mode: gents_documents::BashMode::Unrestricted,
                root: Some(root.path().into()),
            });
    core.apply(tools_request(
        &core,
        vec![(
            "host".into(),
            Some(json!({
                "root": root.path().to_str().unwrap(),
                "bash": {"mode": "Unrestricted", "execution_mode": "workspace_write"}
            })),
        )],
    ))
    .await
    .unwrap();

    let report = core
        .read_effective_config(&BTreeSet::new(), false, false)
        .await
        .unwrap();
    assert_eq!(
        report["tool_grants"]["configured"]["network_mode"],
        "inherit"
    );
    assert_eq!(
        report["runtime_effective"]["effective"]["network_mode"],
        "disabled"
    );
}

#[tokio::test]
async fn canonical_self_config_preview_and_apply_preserve_scope_and_reject_lockout() {
    let fixture =
        gents_automation::test_support::self_config_fixture("canonical-self-config").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_engineer_tool_sources().await;
    fixture.grant_node_reader().await;
    let sibling = fixture.install_sibling("unconfigured").await;
    let other = tokio::task::spawn_blocking(|| node_identity("other-self-config"))
        .await
        .unwrap();
    let foreign = other.did().to_owned();
    gents_documents::test_support::bootstrap_signed_test_node(fixture.user_access(), &foreign)
        .await;
    let tools_id = format!("{}:tools", fixture.agent_did());
    let document = json!({"node_did": foreign, "tools_id": tools_id});
    let plan = gents_documents::DesiredStateApplyPlan::new(vec![
        gents_documents::DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::Tools,
            add: document.clone(),
            update: document,
        },
    ])
    .unwrap();
    fixture
        .user_access()
        .transact_write("test.foreign_tools", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    let core = fixture.core();
    let patch = vec![(
        "self_config".into(),
        Some(json!({"enable_self_config":true})),
    )];
    let preview = core
        .preview(tools_request(&core, patch.clone()))
        .await
        .unwrap();
    assert!(!preview.committed);
    let read = core
        .read_effective_config(&BTreeSet::new(), false, true)
        .await
        .unwrap();
    assert!(read["documents"]["Tools"]["self_config"].is_null());
    core.apply(tools_request(&core, patch)).await.unwrap();
    let guarded = core.clone().with_no_lockout(true);
    assert!(guarded
        .apply(tools_request(&guarded, vec![("self_config".into(), None)],))
        .await
        .is_err());
    assert!(guarded
        .preview(agent_request(
            &guarded,
            vec![(
                "context_id".into(),
                Some(json!(format!("{sibling}:context")))
            )]
        ))
        .await
        .is_err());
    assert!(core
        .apply(anchored_request(
            SelfConfigTarget::AgentContext,
            "context_id",
            vec![("tools_id".into(), Some(json!("missing")))]
        ))
        .await
        .is_err());
    assert!(core
        .preview(tools_request(
            &core,
            vec![("host".into(), Some(json!({"unexpected":true})))],
        ))
        .await
        .is_err());
    let own = core
        .read_effective_config(&BTreeSet::new(), true, true)
        .await
        .unwrap();
    assert_eq!(own["context"]["tools_id"], tools_id);
    assert_eq!(
        own["documents"]["Tools"]["self_config"]["enable_self_config"],
        true
    );
    let foreign_read = fixture.user_access()
        .execute(&format!(r#"{{ Tools(filter: {{node_did: {{_eq: "{}"}}, tools_id: {{_eq: "{}"}}}}) {{ self_config }} }}"#,
            escape_graphql_string(&foreign), escape_graphql_string(&tools_id)))
        .await
        .unwrap();
    let foreign_tools = foreign_read["data"]["Tools"].as_array().unwrap();
    assert_eq!(foreign_tools.len(), 1);
    assert!(foreign_tools[0]["self_config"].is_null());
}

#[tokio::test]
async fn direct_tools_root_saves_follow_the_lean_model() {
    for case in gents_lean_vocab::families::self_config_tools_root_outcomes::cases::<String>() {
        let fixture = gents_automation::test_support::self_config_fixture("root-policy").await;
        fixture.grant_selected_configuration().await;
        let owner = fixture.node_did().to_owned();
        let agent_did = fixture.agent_did().to_owned();
        let access = fixture.user_access();
        let ceiling = tempfile::tempdir().unwrap();
        let selected = ceiling.path().join("selected");
        std::fs::create_dir_all(selected.join("project")).unwrap();
        std::fs::create_dir_all(ceiling.path().join("sibling")).unwrap();
        gents_tools::surface::set_workspace_root_with_readers(
            access,
            &owner,
            &selected.to_string_lossy(),
            None,
            true,
            std::slice::from_ref(&agent_did),
        )
        .await
        .unwrap();
        let core =
            fixture
                .core()
                .with_process_ceiling(gents_tools::surface::SelfConfigProcessCeiling {
                    file_mode: gents_documents::FileToolMode::ReadOnly,
                    bash_mode: gents_documents::BashMode::Off,
                    root: Some(ceiling.path().to_path_buf()),
                });
        let authored = case
            .input
            .authored
            .iter()
            .fold(ceiling.path().to_path_buf(), |path, component| {
                path.join(component)
            });
        let request = tools_request(
            &core,
            vec![(
                "host".into(),
                Some(json!({
                    "root": authored.to_string_lossy(),
                    "files": {"mode": "ReadOnly"}
                })),
            )],
        );
        let saved = match case.input.operation.as_str() {
            "preview" => core.preview(request).await,
            "apply" => core.apply(request).await,
            other => panic!("unknown operation {other}"),
        };
        let (admitted, committed) = match saved {
            Ok(outcome) => (true, outcome.committed),
            Err(error) => {
                let error = format!("{error:#}");
                assert!(
                    error.contains("is not admitted by published WorkspaceRoot policy"),
                    "{:?}: {error}",
                    case.input
                );
                (false, false)
            }
        };
        let tools_id = format!("{agent_did}:tools");
        let persisted: Option<gents_documents::Tools> = core
            .access()
            .unwrap()
            .transact_write("test.self_config.read_tools", |txn| {
                let owner = owner.clone();
                let tools_id = tools_id.clone();
                Box::pin(async move {
                    let value = gents_documents::read_desired_state_document_in_txn(
                        txn,
                        gents_store::collection::Collection::Tools,
                        &owner,
                        &tools_id,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(value.map(serde_json::from_value).transpose()?)
                })
            })
            .await
            .unwrap();
        let canonical_ceiling = std::fs::canonicalize(ceiling.path()).unwrap();
        let persisted = persisted
            .and_then(|tools| tools.host)
            .and_then(|host| host.root)
            .map(|root| {
                std::path::Path::new(&root)
                    .strip_prefix(&canonical_ceiling)
                    .unwrap_or_else(|_| panic!("{root} is outside the ceiling"))
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            });
        case.check(
            gents_lean_vocab::families::self_config_tools_root_outcomes::Outcome {
                admitted,
                committed,
                persisted,
            },
        );
    }
}

#[tokio::test]
async fn unrelated_backend_patch_keeps_environment_reference_unresolved() {
    const VARIABLE: &str = "GENTS_TEST_UNRELATED_PATCH_ENV_REFERENCE";
    const SENTINEL: &str = "sentinel-env-secret-never-stored";
    let fixture =
        gents_automation::test_support::self_config_fixture("self-config-env-reference").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_selected_backend_writer().await;
    let core = fixture.core();
    let reference = json!({"kind":"environment","variable":VARIABLE});
    core.apply(backend_request(vec![(
        "auth".into(),
        Some(reference.clone()),
    )]))
    .await
    .unwrap();
    let _variable = gents_store::test_support::EnvVarGuard::set(VARIABLE, SENTINEL);

    for patch in [
        vec![("name".into(), Some(json!("Renamed inference")))],
        vec![("connect_timeout_secs".into(), Some(json!(17)))],
    ] {
        let preview = core.preview(backend_request(patch.clone())).await.unwrap();
        let applied = core.apply(backend_request(patch)).await.unwrap();
        for outcome in [&preview, &applied] {
            let rendered = serde_json::to_string(outcome).unwrap();
            assert!(!rendered.contains(SENTINEL), "{rendered}");
            assert!(
                !rendered.contains("\"auth\""),
                "an unrelated patch must not report an auth delta: {rendered}"
            );
        }
    }

    let backend_id = escape_graphql_string(&format!("{}:backend", fixture.agent_did()));
    let response = fixture.user_access().execute(&format!(
        r#"{{ InferenceBackendMetadata(filter: {{backend_id: {{_eq: "{backend_id}"}}}}) {{ name connect_timeout_secs auth_kind environment_variable }} }}"#
    )).await.unwrap();
    let stored = &response["data"]["InferenceBackendMetadata"][0];
    assert_eq!(stored["name"], "Renamed inference");
    assert_eq!(stored["connect_timeout_secs"], 17);
    assert_eq!(stored["auth_kind"], "environment");
    assert_eq!(stored["environment_variable"], VARIABLE);
    assert!(!stored.to_string().contains(SENTINEL));
    let read = core
        .read_effective_config(&BTreeSet::new(), false, true)
        .await
        .unwrap();
    assert!(!read.to_string().contains(SENTINEL));
    assert!(read.to_string().contains(VARIABLE));
}

#[tokio::test]
async fn backend_self_config_cannot_set_or_change_an_oauth_account() {
    let fixture = gents_automation::test_support::self_config_fixture("self-config-account").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_selected_backend_writer().await;
    let owner = fixture.node_did();
    let backend_id = format!("{}:backend", fixture.agent_did());
    let core = fixture.core();
    let access = fixture.user_access();
    let store = |kind: &str, auth: Value| {
        let backend: gents_documents::InferenceBackend = serde_json::from_value(json!({
            "node_did": owner, "backend_id": backend_id, "name": "Test inference",
            "provider_kind": kind, "endpoint": "http://127.0.0.1:1/v1", "auth": auth,
        }))
        .unwrap();
        async move {
            gents_documents::write_inference_backend_document(
                access,
                &backend,
                &gents_inference::resolved::ProviderProfilePolicy,
            )
            .await
            .unwrap();
        }
    };
    let to_oauth = |auth: Value| {
        vec![
            ("provider_kind".into(), Some(json!("ChatGptCodex"))),
            ("auth".into(), Some(auth)),
        ]
    };
    let refused = |patch: SelfConfigPatch| {
        let core = &core;
        async move {
            let preview = core.preview(backend_request(patch.clone())).await;
            let apply = core.apply(backend_request(patch)).await;
            for result in [preview, apply] {
                let error = result.expect_err("account reference change must be refused");
                assert!(
                    format!("{error:#}").contains("OAuth account references are operator-managed"),
                    "{error:#}"
                );
            }
        }
    };

    store(
        "OpenAiCompatible",
        json!({"kind":"environment","variable":"KEY"}),
    )
    .await;
    refused(to_oauth(json!({"kind":"node_oauth","account_ref":"a1"}))).await;
    let original = to_oauth(json!({"kind":"node_oauth"}));
    core.preview(backend_request(original.clone()))
        .await
        .unwrap();
    core.apply(backend_request(original)).await.unwrap();

    store(
        "ChatGptCodex",
        json!({"kind":"node_oauth","account_ref":"a1"}),
    )
    .await;
    refused(to_oauth(json!({"kind":"node_oauth","account_ref":"a2"}))).await;
    refused(to_oauth(json!({"kind":"node_oauth"}))).await;
    let endpoint = vec![("endpoint".into(), Some(json!("http://127.0.0.1:2/v1")))];
    core.preview(backend_request(endpoint.clone()))
        .await
        .unwrap();
    core.apply(backend_request(endpoint)).await.unwrap();
}

#[tokio::test]
async fn profile_self_config_cannot_pick_another_account() {
    let (_fixture, core) = account_choice_core("pick").await;
    let owner = core.node_did().to_owned();
    let to = |backend: &str| vec![("backend_id".into(), Some(json!(backend)))];
    assert_account_choice_refused(&core, || profile_request(to("chat-b"))).await;
    core.preview(profile_request(to("grok-original")))
        .await
        .unwrap();
    core.apply(profile_request(to("grok-original")))
        .await
        .unwrap();
    assert_account_choice_refused(&core, || {
        let mut patch = to("chat-b");
        patch.push(("model_name".into(), Some(json!("test-model"))));
        profile_create_request(owner.clone(), "p-new".into(), patch)
    })
    .await;
}

#[tokio::test]
async fn agent_profile_pick_cannot_switch_account() {
    let (_fixture, core) = account_choice_core("agent-pick").await;
    let to = |profile: &str| vec![("inference_profile_id".into(), Some(json!(profile)))];
    assert_account_choice_refused(&core, || agent_request(&core, to("p-chat-b"))).await;
    core.apply(agent_request(&core, to("p-original")))
        .await
        .unwrap();
    core.preview(agent_request(&core, to("p-grok")))
        .await
        .unwrap();
    core.apply(agent_request(&core, to("p-grok")))
        .await
        .unwrap();
}

#[tokio::test]
async fn default_account_follows_the_resolver() {
    let to = |profile: &str| vec![("inference_profile_id".into(), Some(json!(profile)))];
    let accepted = |core: SelfConfigCore, profile: &'static str| async move {
        core.preview(agent_request(&core, to(profile)))
            .await
            .unwrap();
        core.apply(agent_request(&core, to(profile))).await.unwrap();
    };

    let (_fixture, core) = grok_accounts_core("default-g2", false, true).await;
    assert_account_choice_refused(&core, || agent_request(&core, to("p-grok"))).await;
    accepted(core, "p-grok-g2").await;

    let (_fixture, core) = grok_accounts_core("default-original", true, true).await;
    assert_account_choice_refused(&core, || agent_request(&core, to("p-grok-g2"))).await;
    accepted(core, "p-grok").await;

    let (_fixture, core) = grok_accounts_core("default-none", false, false).await;
    accepted(core, "p-grok").await;
}

#[tokio::test]
async fn compaction_profile_pick_cannot_switch_account() {
    let (_fixture, core) = account_choice_core("compaction-pick").await;
    let to = |profile: &str| {
        profile_target_request(
            Some("compaction"),
            vec![("inference_profile_id".into(), Some(json!(profile)))],
        )
        .unwrap()
    };
    assert_account_choice_refused(&core, || to("p-chat-b")).await;
    core.apply(to("p-original")).await.unwrap();
    core.preview(to("p-grok")).await.unwrap();
    core.apply(to("p-grok")).await.unwrap();
}

#[tokio::test]
async fn compaction_reference_pick_cannot_switch_account() {
    let (_fixture, core) = account_choice_core("compaction-ref-pick").await;
    let context = |compaction: &str| {
        protect_working_agent(
            anchored_request(
                SelfConfigTarget::AgentContext,
                "context_id",
                vec![("compaction_id".into(), Some(json!(compaction)))],
            ),
            false,
            true,
        )
    };
    let agent = |context: &str| {
        protect_working_agent(
            agent_request(&core, vec![("context_id".into(), Some(json!(context)))]),
            false,
            true,
        )
    };
    assert_account_choice_refused(&core, || context("c-chat-b")).await;
    assert_account_choice_refused(&core, || agent("ctx-chat-b")).await;
    core.preview(context("c-original")).await.unwrap();
    core.apply(context("c-original")).await.unwrap();
    core.preview(agent("ctx-original")).await.unwrap();
    core.apply(agent("ctx-original")).await.unwrap();
}

#[tokio::test]
async fn agent_management_profile_pick_cannot_switch_account() {
    let (fixture, core) = account_choice_core("agent-pick").await;
    let root = fixture.admitted_workspace_root().await;
    let node = &fixture.db.node;
    let owner = core.node_did().to_owned();
    let params = |action: &str, argv: &[&str]| {
        agent_params(
            action,
            None,
            &argv.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
        )
        .unwrap()
    };
    let create = |profile: &str| {
        params(
            "create",
            &[
                "--display-name",
                "Picker",
                "--description",
                "Picks",
                "--system-prompt",
                "Pick.",
                "--preset",
                "write",
                "--root",
                root.as_str(),
                "--profile",
                profile,
            ],
        )
    };
    let clone = |profile: &str| {
        params(
            "clone",
            &[
                "--from",
                fixture.agent_did(),
                "--display-name",
                "Picker clone",
                "--profile",
                profile,
            ],
        )
    };
    let edit = params(
        "edit",
        &["--id", fixture.agent_did(), "--profile", "p-chat-b"],
    );
    for refused in [create("p-chat-b"), clone("p-chat-b"), edit] {
        let error = model_agent_mutate(&core, fixture.agent.as_ref(), &refused)
            .await
            .expect_err("switching to another account must be refused");
        assert!(
            format!("{error:#}").contains("selects another"),
            "{error:#}"
        );
    }
    assert_eq!(
        gents_documents::list_agents(node, &owner)
            .await
            .unwrap()
            .len(),
        1
    );
    for accepted in [
        create("p-grok"),
        clone("p-grok"),
        params(
            "edit",
            &["--id", fixture.agent_did(), "--profile", "p-original"],
        ),
        params(
            "edit",
            &["--id", fixture.agent_did(), "--profile", "p-grok"],
        ),
    ] {
        model_agent_mutate(&core, fixture.agent.as_ref(), &accepted)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_created_agent_with_host_tools_names_the_users_root_reader_grant() {
    let (fixture, core) = account_choice_core("created-root-access").await;
    let root = fixture.admitted_workspace_root().await;
    let create = |argv: &[&str]| {
        agent_params(
            "create",
            None,
            &argv.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
        )
        .unwrap()
    };
    let hosted: Value = serde_json::from_str(
        &model_agent_mutate(
            &core,
            fixture.agent.as_ref(),
            &create(&[
                "--display-name",
                "Scout",
                "--system-prompt",
                "Find.",
                "--preset",
                "readonly",
                "--root",
                root.as_str(),
                "--profile",
                "p-grok",
            ]),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let created = hosted["agent_did"].as_str().unwrap();
    let access = &hosted["host_root_access"];
    assert_eq!(access["state"], "waiting_for_user", "{hosted}");
    assert_eq!(access["published_root"], root.as_str(), "{hosted}");
    assert_eq!(
        access["user_command"],
        format!("gents config workspace-root grant-reader --path {root} --agent-did {created}"),
        "{hosted}"
    );
    assert!(
        !access["how_to_proceed"]
            .as_str()
            .unwrap()
            .contains("policy grant"),
        "{hosted}"
    );
    let ceiled =
        core.clone()
            .with_process_ceiling(gents_tools::surface::SelfConfigProcessCeiling {
                file_mode: gents_documents::FileToolMode::ReadOnly,
                bash_mode: gents_documents::BashMode::ReadOnly,
                root: Some(std::path::PathBuf::from(&root)),
            });
    let inspected: Value =
        serde_json::from_str(&agent_inspect(&ceiled, created).await.unwrap()).unwrap();
    let observed = &inspected["effective_config"]["runtime_effective"]["root_access"];
    assert_eq!(observed["state"], "requires_user_grant", "{inspected}");
    assert_eq!(
        observed["user_command"], access["user_command"],
        "{inspected}"
    );
    let own: Value =
        serde_json::from_str(&agent_inspect(&ceiled, fixture.agent_did()).await.unwrap()).unwrap();
    assert!(
        own["effective_config"]["runtime_effective"]
            .get("root_access")
            .is_none(),
        "{own}"
    );
    let conversational: Value = serde_json::from_str(
        &model_agent_mutate(
            &core,
            fixture.agent.as_ref(),
            &create(&[
                "--display-name",
                "Planner",
                "--system-prompt",
                "Plan.",
                "--profile",
                "p-grok",
            ]),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(
        conversational.get("host_root_access").is_none(),
        "{conversational}"
    );
}

#[tokio::test]
async fn agent_preview_agrees_with_the_account_choice_fence() {
    let (fixture, core) = account_choice_core("preview-pick").await;
    let root = fixture.admitted_workspace_root().await;
    let create = [
        "--display-name",
        "Picker",
        "--description",
        "Picks",
        "--system-prompt",
        "Pick.",
        "--preset",
        "write",
        "--root",
        root.as_str(),
    ];
    let clone = [
        "--from",
        fixture.agent_did(),
        "--display-name",
        "Picker clone",
    ];
    let edit = ["--id", fixture.agent_did()];
    for (operation, argv, profile, admitted) in [
        ("create", &create[..], "p-chat-b", false),
        ("clone", &clone[..], "p-chat-b", false),
        ("edit", &edit[..], "p-chat-b", false),
        ("create", &create[..], "p-grok", true),
        ("clone", &clone[..], "p-grok", true),
        ("edit", &edit[..], "p-original", true),
    ] {
        let argv: Vec<String> = argv
            .iter()
            .chain(&["--profile", profile])
            .map(|arg| (*arg).to_owned())
            .collect();
        let params = agent_params("preview", Some(operation.into()), &argv).unwrap();
        let preview = model_agent_preview(&core, &params).await;
        if admitted {
            let preview: Value = serde_json::from_str(&preview.unwrap()).unwrap();
            assert_eq!(
                preview["admitted"], true,
                "{operation} {profile}: {preview}"
            );
        } else {
            let error = preview.expect_err("an account switch is not an admission refusal");
            assert!(
                format!("{error:#}").contains("selects another"),
                "{operation} {profile}: {error:#}"
            );
        }
    }
}

#[tokio::test]
async fn agent_clone_inherited_compaction_cannot_switch_account() {
    let (fixture, core) = account_choice_core("clone-pick").await;
    let node = &fixture.db.node;
    let owner = core.node_did().to_owned();
    let original = fixture.install_sibling("src-original").await;
    let chat_b = fixture.install_sibling("src-chat-b").await;
    fixture
        .user_access()
        .transact_write("test.sources", |txn| {
            let sources = [(&original, "ctx-original"), (&chat_b, "ctx-chat-b")];
            let owner = &owner;
            Box::pin(async move {
                let mut documents = Vec::new();
                for (id, context) in sources {
                    let (_, mut value) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Agent,
                        owner,
                        id,
                    )
                    .await?
                    .expect("source Agent exists");
                    value["context_id"] = json!(context);
                    value["inference_profile_id"] = json!("p-original");
                    documents.push(gents_documents::DesiredStateApplyDocument {
                        collection: gents_store::collection::Collection::Agent,
                        add: value.clone(),
                        update: value,
                    });
                }
                let plan = gents_documents::DesiredStateApplyPlan::new(documents)?;
                gents_documents::apply_desired_state_plan(
                    txn,
                    &plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    let before = gents_documents::list_agents(node, &owner).await.unwrap();
    let clone = |action: &str, source: &str, profile: &str| {
        let name = format!("{source} on {profile}");
        let argv = [
            "--from",
            source,
            "--display-name",
            &name,
            "--profile",
            profile,
        ];
        let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
        let operation = (action == "preview").then(|| "clone".to_owned());
        agent_params(action, operation, &argv).unwrap()
    };
    for (source, profile) in [(chat_b.as_str(), "p-original"), (chat_b.as_str(), "p-grok")] {
        let preview = model_agent_preview(&core, &clone("preview", source, profile))
            .await
            .expect_err("an inherited account switch is not an admission refusal");
        assert!(
            format!("{preview:#}").contains("selects another"),
            "{source} {profile}: {preview:#}"
        );
        let error = model_agent_mutate(
            &core,
            fixture.agent.as_ref(),
            &clone("clone", source, profile),
        )
        .await
        .expect_err("inheriting another account's compaction must be refused");
        assert!(
            format!("{error:#}").contains("selects another"),
            "{error:#}"
        );
    }
    assert_eq!(
        gents_documents::list_agents(node, &owner).await.unwrap(),
        before
    );
    for profile in ["p-original", "p-grok"] {
        model_agent_mutate(
            &core,
            fixture.agent.as_ref(),
            &clone("clone", &original, profile),
        )
        .await
        .unwrap();
    }
}

/// Lean `SelfConfig.yoloGroup` and `Identity.GrantSets.yoloRecipient`: an
/// Agent the User grants its own self-configuration edits its own Tools
/// through the self-config tool, writes no other Agent's configuration, and
/// holds no node permission beyond the agent baseline. Granting again changes
/// nothing.
#[tokio::test]
async fn an_agent_granted_its_own_self_configuration_edits_itself_and_nothing_else() {
    use gents_store::config_client::ConfigAccess;
    let fixture = gents_automation::test_support::self_config_fixture("own-self-config").await;
    let engineer = fixture.agent_did.clone();
    let node_did = fixture.node_did().to_owned();
    let tools_of = |agent: String| {
        let access = fixture.user_access.clone();
        let node_did = node_did.clone();
        async move {
            access
                .transact_readonly("test.own_self_config.tools", |txn| {
                    let (node_did, agent) = (&node_did, &agent);
                    Box::pin(async move {
                        gents_documents::load_agent_tools_in_txn(txn, node_did, agent).await
                    })
                })
                .await
                .unwrap()
                .expect("the Agent selects Tools")
        }
    };
    let mut engineer_tools = tools_of(engineer.clone()).await;
    engineer_tools.self_config = Some(engineer_self_config());
    gents_documents::write_tools_document(
        &fixture.user_access,
        &engineer_tools,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    let yolo = fixture.install_sibling("Yolo").await;
    for _ in 0..2 {
        grant_own_self_configuration(&fixture.user_access, &node_did, &yolo, &engineer)
            .await
            .unwrap();
    }
    let yolo_tools = tools_of(yolo.clone()).await;
    assert_eq!(
        serde_json::to_value(&yolo_tools.self_config).unwrap(),
        serde_json::to_value(&engineer_tools.self_config).unwrap()
    );

    let yolo_doc = gents_documents::load_agent(&fixture.db.node, fixture.node_did(), &yolo)
        .await
        .unwrap()
        .unwrap();
    let yolo_signer = gents_store::identity::resolve_agent_identity(
        fixture.db.node_identity.as_ref(),
        &yolo_doc.identity_binding(),
    )
    .unwrap();
    let core = SelfConfigCore::new(fixture.db.node.clone(), node_did.clone(), yolo.clone())
        .unwrap()
        .with_agent_signer(yolo_signer.managed_agent_signer())
        .with_runtime_identity(Some(fixture.db.node_identity.clone()))
        .with_node_binding(Some(fixture.node_binding.clone()));
    core.apply(tools_request(
        &core,
        vec![("display_name".into(), Some(json!("Yolo's own tools")))],
    ))
    .await
    .expect("the agent edits its own Tools through the self-config tool");
    assert_eq!(
        tools_of(yolo.clone()).await.display_name.as_deref(),
        Some("Yolo's own tools")
    );

    let as_yolo = ConfigAccess::local_with_optional_agent_signer(
        fixture.db.node.clone(),
        identity::Did::new(yolo.clone()).unwrap(),
        yolo_signer.managed_agent_signer(),
    )
    .unwrap();
    let mut foreign = tools_of(engineer.clone()).await;
    foreign.display_name = Some("written by the yolo agent".into());
    assert!(
        gents_documents::write_tools_document(
            &as_yolo,
            &foreign,
            &gents_inference::resolved::ProviderProfilePolicy
        )
        .await
        .is_err(),
        "the agent writes no other Agent's configuration"
    );
    assert_ne!(
        tools_of(engineer).await.display_name.as_deref(),
        Some("written by the yolo agent")
    );

    // Its self-config Tools project `delete-dac-relation` as a tool-sourced
    // node operation (Lean `Configuration.toolOperationSources`), as the
    // Engineer's do; it manages no NAC relation and reads no NAC status.
    let member = identity::Did::new(yolo).unwrap();
    for permission in gents_store::identity::GrantSet::PolicyDelegate.permissions() {
        if gents_store::identity::GrantSet::AgentBaseline
            .permissions()
            .contains(permission)
            || permission.as_str() == "delete-dac-relation"
        {
            continue;
        }
        assert!(
            !fixture
                .db
                .node
                .nac_manager()
                .check_permission(&member, *permission)
                .await
                .unwrap(),
            "the agent holds no policy-delegate permission {}",
            permission.as_str()
        );
    }
}

#[tokio::test]
async fn pack_install_uses_current_node_and_inference_chain() {
    let (home, plugins) =
        gents_toolcalls::test_support::home_with_fixture_pack("plugin_documents_fixture");
    let (fixture, tools) = signed_pack_tool("pack-install", plugins).await;
    let node_did = fixture.node_did();
    let coordinator = fixture.install_sibling("coordinator").await;
    let worker = fixture.install_sibling("worker").await;
    let verifier = fixture.install_sibling("verifier").await;
    fixture.grant_node_reader().await;
    // Binding a slot selects that sibling's profile, which native document
    // ACP admits only for a manager of the selection.
    for sibling in [&coordinator, &worker, &verifier] {
        fixture.grant_sibling_configuration(sibling).await;
    }
    let coordinator_profile = format!("{coordinator}:inference");
    let worker_profile = format!("{worker}:inference");
    let verifier_profile = format!("{verifier}:inference");
    let coordinator_slot = format!("coordinator={coordinator_profile}");
    let worker_slot = format!("worker={worker_profile}");
    let verifier_slot = format!("verifier={verifier_profile}");
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    let discovery: Value = serde_json::from_str(
        &tool
            .call(
                json!({"argv": ["pack", "preview", "install", "fixture/plugin_documents_fixture"]})
                    .to_string(),
            )
            .await
            .expect("incomplete preview remains readable"),
    )
    .unwrap();
    assert_eq!(discovery["committed"], false);
    assert_eq!(discovery["ready"], false);
    assert_eq!(
        discovery["missing_inference_slots"],
        json!(["coordinator", "verifier", "worker"])
    );
    assert_eq!(
        discovery["inference"][0]["profiles"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let slot_argv = [
        "--inference-slot",
        coordinator_slot.as_str(),
        "--inference-slot",
        worker_slot.as_str(),
        "--inference-slot",
        verifier_slot.as_str(),
    ];
    let mut preview_argv = vec![
        "pack",
        "preview",
        "install",
        "fixture/plugin_documents_fixture",
    ];
    preview_argv.extend(slot_argv);
    let preview: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": preview_argv}).to_string())
            .await
            .expect("complete preview succeeds"),
    )
    .unwrap();
    assert_eq!(preview["ready"], true);
    let digest = preview["artifact_digest"].as_str().unwrap();
    let mut install_argv = vec![
        "pack",
        "install",
        "fixture/plugin_documents_fixture",
        "--digest",
        digest,
    ];
    install_argv.extend(slot_argv);
    let output = tool
        .call(json!({"argv": install_argv}).to_string())
        .await
        .expect("a stored pack installs");
    let output: serde_json::Value = serde_json::from_str(&output).expect("JSON receipt");
    assert_eq!(
        output["effective"]["coordinate"],
        "fixture/plugin_documents_fixture"
    );
    assert_eq!(output["artifact_digest"], preview["artifact_digest"]);
    let record = gents_pack::plugin::store::read_record(home.path(), "fixture", "echo_fixture")
        .expect("the pack's plugin is in the home's plugin store");
    assert_eq!(
        record.owner_pack_coordinate.as_deref(),
        Some("fixture/plugin_documents_fixture")
    );
    assert!(record.granted.is_none(), "a sealed plugin holds no grant");
    assert_eq!(
        output["inference"]["bindings"],
        json!({
            "coordinator": coordinator_profile,
            "worker": worker_profile,
            "verifier": verifier_profile,
        })
    );
    let inspected: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": ["pack", "get", "fixture/plugin_documents_fixture"]}).to_string())
            .await
            .expect("installed pack is inspectable"),
    )
    .unwrap();
    assert_eq!(
        inspected["installed"]["digest"],
        output["effective"]["digest"]
    );

    let (_staged_root, staged) =
        gents_pack::test_support::fixture_pack_copy("plugin_documents_fixture", &json!({}));
    let manifest_path = staged.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["version"] = "1.1.0".into();
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (bytes, _) = gents_pack::pack::archive::pack_dir(&staged).unwrap();
    let staged_artifact = gents_pack::pack::store::PackStore::new(home.path())
        .import(&bytes[..], None)
        .unwrap();
    let mut staged_preview = vec![
        "pack",
        "preview",
        "update",
        staged_artifact.header.digest.as_str(),
    ];
    staged_preview.extend(slot_argv);
    let preview: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": staged_preview}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["committed"], false);
    let listed: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": ["pack", "list"]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    let entry = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "fixture/plugin_documents_fixture")
        .unwrap();
    assert_eq!(entry["version"], output["effective"]["version"]);
    assert_eq!(entry["installed_digest"], output["effective"]["digest"]);
    assert_eq!(entry["available_version"], "1.1.0");
    assert_eq!(entry["other_stored_versions"][0]["version"], "1.1.0");

    let bad_digest = format!("sha256:{}", "0".repeat(64));
    let rejected = tool
        .call(
            json!({"argv": [
                "pack", "update", "fixture/plugin_documents_fixture@1.0.0", "--digest", bad_digest,
                "--inference-slot", coordinator_slot.as_str(),
                "--inference-slot", worker_slot.as_str(),
                "--inference-slot", verifier_slot.as_str()
            ]})
            .to_string(),
        )
        .await
        .expect_err("a digest not returned by preview is rejected");
    assert!(rejected.to_string().contains("preview again"));

    let rows = fixture
        .user_access
        .execute(
            r#"{ Agent(filter: {display_name: {_eq: "Fixture worker"}}) { _docID agent_did } }"#,
        )
        .await
        .unwrap();
    let pack_worker = rows["data"]["Agent"][0]["_docID"].as_str().unwrap();
    let doc = gents_store::graphql::escape_graphql_string(pack_worker);
    fixture.user_access.transact_write("test.pack.tag_worker", |txn| {
        let doc = &doc;
        Box::pin(async move {
            txn.execute_client_signed_update(
                &format!(r#"mutation($input: AgentMutationInputArg!) {{ update_Agent(docID: "{doc}", input: $input) {{ _docID }} }}"#),
                &json!({"input": {"tags": ["gents:pack:fixture/plugin_documents_fixture", "user:favorite"]}}),
                gents_protocol::schemas::AGENT,
            ).await
        })
    }).await.unwrap();
    let update_preview: Value = serde_json::from_str(
        &tool
            .call(
                json!({"argv": [
                    "pack", "preview", "update", "fixture/plugin_documents_fixture@1.0.0",
                    "--inference-slot", coordinator_slot.as_str(),
                    "--inference-slot", worker_slot.as_str(),
                    "--inference-slot", verifier_slot.as_str()
                ]})
                .to_string(),
            )
            .await
            .expect("installed pack update previews"),
    )
    .unwrap();
    assert_eq!(update_preview["ready"], true);
    let update_digest = update_preview["artifact_digest"].as_str().unwrap();
    let updated: Value = serde_json::from_str(
        &tool
            .call(
                json!({"argv": [
                    "pack", "update", "fixture/plugin_documents_fixture@1.0.0", "--digest", update_digest,
                    "--inference-slot", coordinator_slot.as_str(),
                    "--inference-slot", worker_slot.as_str(),
                    "--inference-slot", verifier_slot.as_str()
                ]})
                .to_string(),
            )
            .await
            .expect("same distribution update is idempotent"),
    )
    .unwrap();
    assert_eq!(
        updated["effective"]["digest"],
        output["effective"]["digest"]
    );
    let tags = fixture
        .user_access
        .execute(&format!(
            r#"{{ Agent(docID: "{}") {{ tags }} }}"#,
            gents_store::graphql::escape_graphql_string(pack_worker),
        ))
        .await
        .unwrap();
    assert_eq!(
        tags["data"]["Agent"][0]["tags"],
        json!([
            "gents:pack:fixture/plugin_documents_fixture",
            "user:favorite"
        ])
    );
    let store = gents_pack::pack::store::PackStore::new(home.path());
    let installed_path = store
        .path(output["effective"]["digest"].as_str().unwrap())
        .unwrap();
    let installed_bytes = std::fs::read(&installed_path).unwrap();
    std::fs::remove_file(&installed_path).unwrap();
    std::fs::remove_dir_all(home.path().join("packs").join("unpacked")).unwrap();
    let listed: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": ["pack", "list"]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    let entry = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "fixture/plugin_documents_fixture")
        .unwrap();
    assert_eq!(entry["installed_digest"], output["effective"]["digest"]);
    assert_eq!(entry["installable"], false);
    assert!(entry["error"]
        .as_str()
        .unwrap()
        .contains("could not be opened"));
    std::fs::write(installed_path, installed_bytes).unwrap();
    // Nothing that survives the removal selects the pack's plugin, so the
    // pack is removed and its plugin retired (`ensure_plugins_unreferenced`).
    tool.call(json!({"argv": ["pack", "remove", "fixture/plugin_documents_fixture"]}).to_string())
        .await
        .expect("an unselected plugin retires with its pack");
    let response = fixture
        .user_access
        .execute(&format!(
            r#"{{ PackInstallation(filter: {{node_did: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            gents_store::graphql::escape_graphql_string(node_did),
        ))
        .await
        .unwrap();
    assert_eq!(
        response["data"]["PackInstallation"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn pack_documents_install_and_release_documents_dependencies() {
    let (home, plugins) =
        gents_toolcalls::test_support::home_with_fixture_pack("dependent_fixture");
    let (_fixture, directory) =
        gents_pack::test_support::fixture_pack_copy("dependee_fixture", &json!({}));
    let (bytes, _) = gents_pack::pack::archive::pack_dir(&directory).unwrap();
    gents_pack::pack::store::PackStore::new(home.path())
        .import(&bytes[..], None)
        .unwrap();
    let (fixture, tools) = signed_pack_tool("documents-dependencies", plugins).await;
    let did = fixture.db.node_identity.did();
    let slot = format!("worker={}:inference", fixture.agent_did());
    let slots = ["--inference-slot", slot.as_str()];
    let mut args = vec!["pack", "preview", "install", "fixture/dependent_fixture"];
    args.extend(slots);
    let preview: Value = serde_json::from_str(&config_call(&tools, &args).await.unwrap()).unwrap();
    assert_eq!(preview["ready"], true, "{preview}");
    assert_eq!(
        preview["dependencies"][0]["coordinate"],
        "fixture/dependee_fixture"
    );
    let digest = preview["artifact_digest"].as_str().unwrap();
    let mut args = vec![
        "pack",
        "install",
        "fixture/dependent_fixture",
        "--digest",
        digest,
    ];
    args.extend(slots);
    config_call(&tools, &args).await.unwrap();
    let access = &fixture.user_access;
    let rows = access
        .execute(
            r#"{ PackInstallation(filter: {coordinate: {_eq: "fixture/dependee_fixture"}}) { explicit required_by } }"#,
        )
        .await
        .unwrap();
    assert_eq!(
        rows["data"]["PackInstallation"],
        json!([{"explicit": false, "required_by": ["fixture/dependent_fixture"]}])
    );
    config_call(&tools, &["pack", "remove", "fixture/dependent_fixture"])
        .await
        .unwrap();
    assert!(
        gents_pack::pack::read_installed_pack(access, did, "fixture/dependee_fixture")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn pack_document_installation_uses_existing_lifecycle() {
    let (_fixture, directory) =
        gents_pack::test_support::fixture_pack_copy("documents_fixture", &json!({}));
    let manifest_path = directory.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest.as_object_mut().unwrap().remove("schemas");
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (home, plugins) = gents_toolcalls::test_support::home_with_pack_dir(&directory);
    let (fixture, tools) = signed_pack_tool("inspect-documents-pack", plugins).await;
    let did = fixture.db.node_identity.did();
    let slot = format!("worker={}:inference", fixture.agent_did());
    let result: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "get", "fixture/documents_fixture"])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["installable"], true);
    let missing: Value = serde_json::from_str(
        &config_call(
            &tools,
            &["pack", "preview", "install", "fixture/documents_fixture"],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(missing["ready"], false);
    assert_eq!(missing["missing_inference_slots"], json!(["worker"]));
    let preview: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "preview",
                "install",
                "fixture/documents_fixture",
                "--inference-slot",
                &slot,
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["ready"], true, "{preview}");
    let digest = preview["artifact_digest"].as_str().unwrap();
    let access = &fixture.user_access;
    assert!(
        gents_pack::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none(),
        "preview must not install documents"
    );
    let rejected = config_call(
        &tools,
        &[
            "pack",
            "install",
            "fixture/documents_fixture",
            "--digest",
            "sha256:wrong",
            "--inference-slot",
            &slot,
        ],
    )
    .await
    .unwrap_err();
    assert!(rejected.to_string().contains("digest"), "{rejected}");
    assert!(
        gents_pack::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none()
    );
    let installed: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "install",
                "fixture/documents_fixture",
                "--digest",
                digest,
                "--inference-slot",
                &slot,
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(installed["effective"]["digest"], digest);
    assert!(
        fixture
            .db
            .node
            .get_collection("FixtureJob")
            .unwrap()
            .is_some(),
        "custom schema asset must be published"
    );

    assert!(
        gents_pack::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_some()
    );
    let updated: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "update",
                "fixture/documents_fixture@1.0.0",
                "--digest",
                digest,
                "--inference-slot",
                &slot,
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(updated["effective"]["digest"], digest);
    config_call(&tools, &["pack", "remove", "fixture/documents_fixture"])
        .await
        .unwrap();
    assert!(
        gents_pack::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none()
    );
    let collection = || fixture.db.node.get_collection("FixtureJob").unwrap();
    assert!(
        collection().is_some(),
        "remove keeps collections by default"
    );

    let install = [
        "pack",
        "install",
        "fixture/documents_fixture",
        "--digest",
        digest,
        "--inference-slot",
        &slot,
    ];
    let with_collections = [
        "pack",
        "remove",
        "fixture/documents_fixture",
        "--with-collections",
    ];
    let admin = gents_store::config_client::ConfigAccess::local_as(
        fixture.db.node.clone(),
        identity::Did::new(did).unwrap(),
    );
    // A reinstall finds the collection the first install left behind. It did
    // not create it, so removing the pack with its collections keeps it.
    gents_pack::pack::store::PackStore::new(home.path())
        .import(
            &gents_pack::pack::archive::pack_dir(&directory).unwrap().0[..],
            None,
        )
        .unwrap();
    config_call(&tools, &install).await.unwrap();
    let adopted: Value =
        serde_json::from_str(&config_call(&tools, &with_collections).await.unwrap()).unwrap();
    assert_eq!(adopted["collections"][0]["removed"], false, "{adopted}");
    assert!(
        adopted["collections"][0]["kept_for"][0]["user"]
            .as_str()
            .unwrap()
            .contains("did not create it"),
        "{adopted}"
    );
    assert!(collection().is_some());
    let drop_collection = || async {
        admin.truncate_collection("FixtureJob").await.unwrap();
        admin
            .remove_collections(&["FixtureJob".to_owned()])
            .await
            .unwrap();
    };
    drop_collection().await;

    gents_pack::pack::store::PackStore::new(home.path())
        .import(
            &gents_pack::pack::archive::pack_dir(&directory).unwrap().0[..],
            None,
        )
        .unwrap();
    config_call(&tools, &install).await.unwrap();
    admin
        .write(
            "pack_test.seed",
            r#"mutation { create_FixtureJob(input:{job_id:"one"}) { _docID } }"#,
        )
        .await
        .unwrap();
    let denied = config_call(&tools, &with_collections).await.unwrap_err();
    assert!(
        denied
            .to_string()
            .contains("cleaning up its collections failed"),
        "{denied}"
    );
    assert!(
        collection().is_some(),
        "the Agent's own DID lacks truncate-collection"
    );
    drop_collection().await;

    admin
        .grant_node_permission(
            &identity::Did::new(fixture.agent_did()).unwrap(),
            gents_store::config_client::NodePermission::CollectionTruncate,
        )
        .await
        .unwrap();
    gents_pack::pack::store::PackStore::new(home.path())
        .import(
            &gents_pack::pack::archive::pack_dir(&directory).unwrap().0[..],
            None,
        )
        .unwrap();
    config_call(&tools, &install).await.unwrap();
    admin
        .write(
            "pack_test.seed",
            r#"mutation { create_FixtureJob(input:{job_id:"one"}) { _docID } }"#,
        )
        .await
        .unwrap();
    let removed: Value =
        serde_json::from_str(&config_call(&tools, &with_collections).await.unwrap()).unwrap();
    assert_eq!(
        removed["collections"],
        json!([{"collection":"FixtureJob","removed":true,"documents_removed":1}])
    );
    assert!(collection().is_none());
}

use gents_tools::builtin::self_config::agent_management::{
    model_agent_mutate, model_agent_preview,
};
use gents_tools::builtin::self_config::command::agents::agent_params;
