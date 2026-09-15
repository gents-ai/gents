//! Live model acceptance for the argv-style self-configuration product.
//!
//! The assertion surface is durable configuration, not prose: a real model
//! must discover/call `config` repeatedly and leave the requested behaviors,
//! profile bindings, prompts, tool roots, default, and pack graph in DefraDB.
//!
//! `GENTS_LIVE_CONFIG_MODELS` accepts a comma-separated model matrix and
//! `GENTS_LIVE_CONFIG_RUNS` selects 1-100 isolated trials per model. Each
//! prompt lives under `tests/fixtures/configurator_evals`; Rust assertions own
//! the corresponding durable success contract.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use futures::FutureExt;
use gents::document_config::{AgentContext, InferenceProfile, SelfConfigTools, Tools};
use gents::{AgentIdentity, Collection};
use serde::Serialize;
use serde_json::Value;

use super::steward_loop_live::{
    bind_d4f_backend_for_model, boot_d4f_agent_with_ceiling, wait_for_assistant_answer,
    wait_for_request_terminal,
};
use crate::support::fixtures::test_identity;
use crate::support::interrupt::create_runtime_request;
use crate::support::test_db;

const EVAL_CASE_ID: &str = "software-team-and-code-review";
const ONBOARDING_PROMPT: &str =
    include_str!("../fixtures/configurator_evals/software_team_and_code_review.md");

fn live_enabled() -> bool {
    std::env::var("GENTS_LIVE_CONFIG").as_deref() == Ok("1")
}

fn model_name() -> String {
    std::env::var("GENTS_D4F_MODEL").unwrap_or_else(|_| "GLM-5.3-Flash-NVFP4".to_owned())
}

fn eval_models() -> Vec<String> {
    std::env::var("GENTS_LIVE_CONFIG_MODELS")
        .ok()
        .map(|models| {
            models
                .split(',')
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|models| !models.is_empty())
        .unwrap_or_else(|| vec![model_name()])
}

fn eval_runs() -> usize {
    let runs = std::env::var("GENTS_LIVE_CONFIG_RUNS")
        .ok()
        .map(|value| {
            value
                .parse::<usize>()
                .expect("GENTS_LIVE_CONFIG_RUNS must be an integer")
        })
        .unwrap_or(1);
    assert!(
        (1..=100).contains(&runs),
        "GENTS_LIVE_CONFIG_RUNS must be between 1 and 100"
    );
    runs
}

fn onboarding_prompt(user_home: &str) -> String {
    ONBOARDING_PROMPT.replace("{{USER_HOME}}", user_home)
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|message| (*message).to_owned())
        })
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

#[derive(Debug, Serialize)]
struct ConfiguratorEvalResult {
    case_id: &'static str,
    model: String,
    trial: usize,
    passed: bool,
    terminal_state: Option<String>,
    error: Option<String>,
    assistant_answer_excerpt: Option<String>,
}

fn excerpt(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let excerpt = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{excerpt}…")
    } else {
        excerpt
    }
}

async fn install_setup_configurator(
    node: &gents::defra_node::EmbeddedNode,
    agent_did: &str,
    setup_behavior_id: &str,
) {
    let context_id = format!("{setup_behavior_id}:config-context");
    let tools_id = format!("{setup_behavior_id}:config-tools");
    let prompt = r#"You are a configuration acceptance steward. Carry out the user's requested configuration changes with the single argv-style config tool; do not merely describe commands. Start with config help for relevant resources and use preview before writes. Use exact IDs supplied by reads or by the user. Verify each durable result with config reads. Never edit Setup itself. Nested --set values replace the complete top-level group, so inspect and preserve unrelated fields."#;
    let context = AgentContext {
        context_id: context_id.clone(),
        agent_did: agent_did.to_owned(),
        display_name: Some("Live Setup".into()),
        description: Some("Live configurator acceptance".into()),
        system_prompt: Some(prompt.into()),
        tools_id: Some(tools_id.clone()),
        compaction_id: None,
        skill_ids: Vec::new(),
        tags: Vec::new(),
    };
    let tools = Tools {
        tools_id,
        agent_did: agent_did.to_owned(),
        display_name: Some("Live Setup tools".into()),
        self_config: Some(SelfConfigTools {
            enable_self_config: Some(true),
            self_config_categories: Some(vec![
                "behavior".into(),
                "tools".into(),
                "profile".into(),
                "backend".into(),
                "persona".into(),
            ]),
            self_config_no_lockout: Some(true),
            self_config_dry_run: Some(true),
            enable_pack_install: Some(true),
            timeout_secs: Some(30),
        }),
        ..Default::default()
    };
    let behavior = gents::list_agent_behaviors(node, agent_did)
        .await
        .unwrap()
        .into_iter()
        .find(|behavior| behavior.behavior_id == setup_behavior_id)
        .expect("setup behavior");
    let behavior = gents::document_config::AgentBehavior {
        context_id: Some(context_id),
        tags: vec![gents::agent::persona_ops::SETUP_STEWARD_BEHAVIOR_TAG.into()],
        ..behavior
    };
    let plan = gents::config_client::DesiredStateApplyPlan::new(
        [
            (
                Collection::AgentContext,
                serde_json::to_value(context).unwrap(),
            ),
            (Collection::Tools, serde_json::to_value(tools).unwrap()),
            (
                Collection::AgentBehavior,
                serde_json::to_value(behavior).unwrap(),
            ),
        ]
        .into_iter()
        .map(
            |(collection, value)| gents::config_client::DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            },
        )
        .collect(),
    )
    .unwrap();
    gents::ConfigAccess::transact_local(node, None, "test.live_configurator", |txn| {
        let plan = &plan;
        Box::pin(async move {
            gents::config_client::apply_desired_state_plan(txn, plan)
                .await
                .map(|_| ())
        })
    })
    .await
    .unwrap();
}

async fn install_eval_profiles(
    node: &gents::defra_node::EmbeddedNode,
    agent_did: &str,
    model: &str,
) {
    let documents = [("high", "High"), ("medium", "Medium"), ("low", "Low")]
        .into_iter()
        .map(|(profile_id, display_name)| InferenceProfile {
            agent_did: agent_did.to_owned(),
            profile_id: profile_id.to_owned(),
            backend_id: "backend-d4f-live".to_owned(),
            model_name: model.to_owned(),
            display_name: Some(display_name.to_owned()),
            ..Default::default()
        })
        .map(|profile| {
            let value = serde_json::to_value(profile).expect("serialize eval profile");
            gents::config_client::DesiredStateApplyDocument {
                collection: Collection::InferenceProfile,
                add: value.clone(),
                update: value,
            }
        })
        .collect();
    let plan = gents::config_client::DesiredStateApplyPlan::new(documents)
        .expect("build eval profile plan");
    gents::ConfigAccess::transact_local(node, None, "test.live_configurator_profiles", |txn| {
        let plan = &plan;
        Box::pin(async move {
            gents::config_client::apply_desired_state_plan(txn, plan)
                .await
                .map(|_| ())
        })
    })
    .await
    .expect("install eval profiles");
}

async fn install_eval_workspace_root(node: &gents::defra_node::EmbeddedNode, user_home: &str) {
    let root = gents::graphql::escape_graphql_string(user_home);
    let updated_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_WorkspaceRoot(input: {{
                root_path: "{root}",
                display_name: "Configurator eval user home",
                enabled: true,
                updated_at: "{updated_at}"
            }}) {{ _docID }}
        }}"#
    );
    gents::ConfigAccess::write_local(node, "test.live_configurator_workspace_root", &mutation)
        .await
        .expect("publish eval user home as an operator-owned workspace root");
}

async fn rows(
    node: &gents::defra_node::EmbeddedNode,
    query: &str,
    collection: &str,
) -> Result<Vec<Value>> {
    let response = node.execute(query).await;
    ensure!(
        !response.has_errors(),
        "{collection} query failed: {:?}",
        response.errors
    );
    Ok(response.data.context("query returned no data")?[collection]
        .as_array()
        .with_context(|| format!("{collection} was not an array"))?
        .clone())
}

fn exact_named_behavior<'a>(behaviors: &'a [Value], display_name: &str) -> Result<&'a Value> {
    let matches = behaviors
        .iter()
        .filter(|behavior| behavior["display_name"] == display_name)
        .collect::<Vec<_>>();
    ensure!(
        matches.len() == 1,
        "expected exactly one {display_name:?} behavior, found {}",
        matches.len()
    );
    Ok(matches[0])
}

async fn verify_configuration(
    node: &gents::defra_node::EmbeddedNode,
    agent_did: &str,
    setup_behavior_id: &str,
    user_home: &str,
    model: &str,
) -> Result<()> {
    let owner = gents::graphql::escape_graphql_string(agent_did);
    let profiles = rows(
        node,
        &format!(
            r#"{{ InferenceProfile(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{profile_id backend_id model_name}} }}"#
        ),
        "InferenceProfile",
    )
    .await?;
    for profile_id in ["high", "medium", "low"] {
        let profile = profiles
            .iter()
            .find(|profile| profile["profile_id"] == profile_id)
            .with_context(|| format!("seeded profile {profile_id:?} disappeared"))?;
        ensure!(profile["backend_id"] == "backend-d4f-live");
        ensure!(profile["model_name"] == model);
    }

    let behaviors = rows(
        node,
        &format!(
            r#"{{ AgentBehavior(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{behavior_id display_name context_id inference_profile_id tags enabled}} }}"#
        ),
        "AgentBehavior",
    )
    .await?;
    let expected = [
        ("Builder", "medium", "ReadWrite"),
        ("Explorer", "low", "ReadOnly"),
        ("Reviewer", "high", "ReadOnly"),
    ];
    let contexts = rows(
        node,
        &format!(
            r#"{{ AgentContext(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{context_id system_prompt tools_id}} }}"#
        ),
        "AgentContext",
    )
    .await?;
    let tool_rows = rows(
        node,
        &format!(
            r#"{{ Tools(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{tools_id host self_config}} }}"#
        ),
        "Tools",
    )
    .await?;
    let mut builder_id = None;
    for (display_name, profile_id, file_mode) in expected {
        let behavior = exact_named_behavior(&behaviors, display_name)?;
        ensure!(
            behavior["inference_profile_id"] == profile_id,
            "{display_name} uses the wrong inference profile"
        );
        if display_name == "Builder" {
            builder_id = behavior["behavior_id"].as_str().map(str::to_owned);
        }
        let context_id = behavior["context_id"]
            .as_str()
            .with_context(|| format!("{display_name} has no context"))?;
        let context = contexts
            .iter()
            .find(|context| context["context_id"] == context_id)
            .with_context(|| format!("{display_name} context is missing"))?;
        ensure!(
            context["system_prompt"]
                .as_str()
                .is_some_and(|prompt| !prompt.trim().is_empty()),
            "{display_name} has no role prompt"
        );
        let tools_id = context["tools_id"]
            .as_str()
            .with_context(|| format!("{display_name} has no Tools reference"))?;
        let tools = tool_rows
            .iter()
            .find(|tools| tools["tools_id"] == tools_id)
            .with_context(|| format!("{display_name} Tools document is missing"))?;
        ensure!(
            tools["host"]["root"] == user_home,
            "{display_name} tool root does not equal the requested user home"
        );
        ensure!(
            tools["host"]["files"]["mode"] == file_mode,
            "{display_name} file mode does not match its requested preset"
        );
        ensure!(
            tools["self_config"]["enable_self_config"] != true,
            "{display_name} was unnecessarily granted self-configuration"
        );
    }

    let principals = rows(
        node,
        &format!(
            r#"{{ AgentPrincipal(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{default_behavior_id}} }}"#
        ),
        "AgentPrincipal",
    )
    .await?;
    ensure!(principals.len() == 1, "principal is missing or duplicated");
    ensure!(
        principals[0]["default_behavior_id"] == builder_id.context("Builder has no behavior ID")?,
        "Builder was not selected as the default"
    );

    let setup = behaviors
        .iter()
        .find(|behavior| behavior["behavior_id"] == setup_behavior_id)
        .context("Setup behavior disappeared")?;
    ensure!(setup["enabled"] == true, "Setup was disabled");
    ensure!(
        setup["tags"] == serde_json::json!([gents::agent::persona_ops::SETUP_STEWARD_BEHAVIOR_TAG]),
        "Setup protection tag changed"
    );

    let graphs = rows(
        node,
        &format!(
            r#"{{ GraphDefinition(filter: {{agent_did: {{_eq: "{owner}"}}, graph_id: {{_eq: "code-review"}}}}) {{graph_id active_revision_digest}} }}"#
        ),
        "GraphDefinition",
    )
    .await?;
    ensure!(
        graphs.len() == 1,
        "code_review graph was not installed exactly once"
    );
    ensure!(
        graphs[0]["active_revision_digest"]
            .as_str()
            .is_some_and(|digest| !digest.is_empty()),
        "code_review graph has no active revision"
    );
    for (behavior_id, profile_id) in [
        ("review-recon", "high"),
        ("review-scan", "medium"),
        ("review-verify", "high"),
        ("review-triage", "high"),
    ] {
        let installed = behaviors
            .iter()
            .find(|behavior| behavior["behavior_id"] == behavior_id)
            .with_context(|| format!("installed pack behavior {behavior_id:?} is missing"))?;
        ensure!(
            installed["inference_profile_id"] == profile_id,
            "installed pack behavior {behavior_id:?} has the wrong slot binding"
        );
    }
    Ok(())
}

async fn run_eval_trial(model: String, trial: usize) -> ConfiguratorEvalResult {
    let label = format!("live-configurator-{trial}");
    let db = test_db(&label).await;
    let user_home = tempfile::tempdir().expect("isolated eval user home");
    let user_home = user_home.path().to_string_lossy().into_owned();
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity(&label));
    let (agent_did, setup_behavior_id) =
        bind_d4f_backend_for_model(db.node.as_ref(), identity.as_ref(), &model).await;
    install_eval_profiles(db.node.as_ref(), &agent_did, &model).await;
    install_eval_workspace_root(db.node.as_ref(), &user_home).await;
    install_setup_configurator(db.node.as_ref(), &agent_did, &setup_behavior_id).await;
    let agent =
        boot_d4f_agent_with_ceiling(&db, identity, gents::ToolCeiling::readwrite(&user_home))
            .await
            .expect("boot live configurator");

    let request_id = format!("request-live-configurator-{trial}");
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &setup_behavior_id,
        &request_id,
        "session-live-configurator",
        &onboarding_prompt(&user_home),
    )
    .await;
    let terminal =
        wait_for_request_terminal(db.node.as_ref(), &request_id, Duration::from_secs(600)).await;
    let answer =
        wait_for_assistant_answer(db.node.as_ref(), &request_id, Duration::from_secs(30)).await;
    let verification = if terminal == "completed" {
        verify_configuration(
            db.node.as_ref(),
            &agent_did,
            &setup_behavior_id,
            &user_home,
            &model,
        )
        .await
    } else {
        Err(anyhow::anyhow!("request terminalized as {terminal}"))
    };
    agent.shutdown().await;
    ConfiguratorEvalResult {
        case_id: EVAL_CASE_ID,
        model,
        trial,
        passed: verification.is_ok(),
        terminal_state: Some(terminal),
        error: verification.err().map(|error| format!("{error:#}")),
        assistant_answer_excerpt: Some(excerpt(&answer, 2_000)),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_CONFIG=1 and pass --ignored"]
async fn live_configurator_onboarding_eval_matrix() {
    assert!(
        live_enabled(),
        "set GENTS_LIVE_CONFIG=1 and pass --ignored to run live configurator acceptance"
    );
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "off,gents::configurator_eval=info",
        ))
        .try_init();
    let models = eval_models();
    let runs = eval_runs();
    let mut results = Vec::with_capacity(models.len() * runs);
    for model in models {
        for trial in 1..=runs {
            let failed_model = model.clone();
            let report = match AssertUnwindSafe(run_eval_trial(model.clone(), trial))
                .catch_unwind()
                .await
            {
                Ok(report) => report,
                Err(error) => ConfiguratorEvalResult {
                    case_id: EVAL_CASE_ID,
                    model: failed_model,
                    trial,
                    passed: false,
                    terminal_state: None,
                    error: Some(format!(
                        "eval trial panicked: {}",
                        panic_message(error.as_ref())
                    )),
                    assistant_answer_excerpt: None,
                },
            };
            tracing::info!(
                target: "gents::configurator_eval",
                result = %serde_json::to_string(&report).expect("serialize eval report"),
                "configurator eval trial"
            );
            results.push(report);
        }
    }
    let failures = results
        .iter()
        .filter(|result| !result.passed)
        .collect::<Vec<_>>();
    tracing::info!(
        target: "gents::configurator_eval",
        case_id = EVAL_CASE_ID,
        total = results.len(),
        passed = results.len() - failures.len(),
        failed = failures.len(),
        "configurator eval summary"
    );
    assert!(
        failures.is_empty(),
        "{} of {} configurator eval trials failed:\n{}",
        failures.len(),
        results.len(),
        serde_json::to_string_pretty(&results).expect("serialize eval summary")
    );
}
