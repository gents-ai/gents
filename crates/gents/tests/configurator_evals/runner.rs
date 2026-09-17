//! Progressive live acceptance: model-authored configuration, native task execution,
//! and independent checks of durable state and generated artifacts.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use futures::{FutureExt, StreamExt};
use gents::document_config::{AgentContext, InferenceProfile, InferenceSampling, Tools};
use gents::{AgentIdentity, Collection};
use serde_json::Value;
use tracing::Instrument;

use crate::support::live_inference::{
    bind_d4f_backend_for_model, bind_openrouter_backend_for_model, boot_d4f_agent_with_options,
    D4F_BACKEND_ID, OPENROUTER_BACKEND_ID,
};
use crate::support::test_db_in;

#[path = "stages.rs"]
mod stages;

#[path = "access.rs"]
mod access;

#[path = "host.rs"]
mod host;

#[path = "host_scenarios.rs"]
mod host_scenarios;

#[path = "cases.rs"]
mod cases;

#[path = "reporting.rs"]
mod reporting;

#[path = "readiness.rs"]
mod readiness;

#[path = "onboarding_scenarios.rs"]
mod onboarding_scenarios;

const EVAL_CASE_ID: &str = "progressive-configurator";

fn monitor_suite() -> bool {
    std::env::var("GENTS_EVAL_SUITE").as_deref() == Ok("monitor-mailbox")
}

fn host_suite() -> bool {
    std::env::var("GENTS_EVAL_SUITE").as_deref() == Ok("host-steward")
}

fn suite_cases() -> &'static [stages::CaseId] {
    if host_suite() {
        host_scenarios::CASES
    } else if monitor_suite() {
        onboarding_scenarios::MONITOR_CASES
    } else {
        stages::PROGRESSIVE_CASES
    }
}

fn suite_id() -> &'static str {
    if host_suite() {
        "host-steward"
    } else if monitor_suite() {
        "monitor-mailbox"
    } else {
        EVAL_CASE_ID
    }
}
const EVAL_COHORT: &str = "configurator-temperature-1-top-p-0.95-v1";
const EVAL_GRADER: &str = "configurator-process-receipts-v3-no-artwork";
const EVAL_SAMPLING_ID: &str = "configurator-eval-sampling-v1";
const EVAL_TEMPERATURE: f64 = 1.0;
const EVAL_TOP_P: f64 = 0.95;

fn parse_eval_reasoning_effort(
    value: Option<&str>,
) -> Result<Option<gents::config::ReasoningEffort>> {
    value.map(gents::config::ReasoningEffort::parse).transpose()
}

fn eval_reasoning_effort() -> Result<Option<gents::config::ReasoningEffort>> {
    let value = match std::env::var("GENTS_LIVE_CONFIG_REASONING_EFFORT") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => return Err(error.into()),
    };
    parse_eval_reasoning_effort(value.as_deref())
        .context("invalid GENTS_LIVE_CONFIG_REASONING_EFFORT")
}

#[test]
fn eval_reasoning_override_preserves_unset_and_validates_canonical_values() {
    use gents::config::ReasoningEffort;
    assert_eq!(parse_eval_reasoning_effort(None).unwrap(), None);
    for effort in ReasoningEffort::ALL {
        assert_eq!(
            parse_eval_reasoning_effort(Some(effort.as_str())).unwrap(),
            Some(effort)
        );
    }
    assert!(parse_eval_reasoning_effort(Some("")).is_err());
    assert!(parse_eval_reasoning_effort(Some("hgh")).is_err());
}
const ONBOARDING_PROMPT: &str =
    include_str!("../fixtures/configurator_evals/software_team_and_code_review.md");
const EVAL_GRADER_SOURCES: &[reporting::EvidenceSource] = &[
    reporting::EvidenceSource::new("cases.rs", include_bytes!("cases.rs")),
    reporting::EvidenceSource::new("readiness.rs", include_bytes!("readiness.rs")),
    reporting::EvidenceSource::new("stages.rs", include_bytes!("stages.rs")),
];
const EVAL_FIXTURES: &[reporting::EvidenceSource] = &[
    reporting::EvidenceSource::new(
        "builder_readiness.md",
        include_bytes!("../fixtures/configurator_evals/builder_readiness.md"),
    ),
    reporting::EvidenceSource::new(
        "document_automation.md",
        include_bytes!("../fixtures/configurator_evals/document_automation.md"),
    ),
    reporting::EvidenceSource::new(
        "skill_setup.md",
        include_bytes!("../fixtures/configurator_evals/skill_setup.md"),
    ),
    reporting::EvidenceSource::new(
        "skill_approve.md",
        include_bytes!("../fixtures/configurator_evals/skill_approve.md"),
    ),
    reporting::EvidenceSource::new(
        "skill_use.md",
        include_bytes!("../fixtures/configurator_evals/skill_use.md"),
    ),
    reporting::EvidenceSource::new(
        "software_team_and_code_review.md",
        include_bytes!("../fixtures/configurator_evals/software_team_and_code_review.md"),
    ),
    reporting::EvidenceSource::new(
        "tool_surface_audit.md",
        include_bytes!("../fixtures/configurator_evals/tool_surface_audit.md"),
    ),
];

#[derive(Clone, Copy, Debug)]
enum LiveProvider {
    D4f,
    OpenRouter,
}

impl LiveProvider {
    fn from_env() -> Self {
        match std::env::var("GENTS_LIVE_CONFIG_PROVIDER")
            .unwrap_or_else(|_| "d4f".to_owned())
            .to_ascii_lowercase()
            .as_str()
        {
            "d4f" => Self::D4f,
            "openrouter" => {
                assert!(
                    std::env::var("OPENROUTER_API_KEY").is_ok_and(|key| !key.trim().is_empty()),
                    "OPENROUTER_API_KEY must be non-empty for the OpenRouter configurator eval"
                );
                Self::OpenRouter
            }
            value => panic!(
                "unsupported GENTS_LIVE_CONFIG_PROVIDER {value:?}; expected d4f or openrouter"
            ),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::D4f => "d4f",
            Self::OpenRouter => "openrouter",
        }
    }

    fn backend_id(self) -> &'static str {
        match self {
            Self::D4f => D4F_BACKEND_ID,
            Self::OpenRouter => OPENROUTER_BACKEND_ID,
        }
    }

    fn endpoint(self) -> String {
        match self {
            Self::D4f => crate::support::live_inference::d4f_endpoint(),
            Self::OpenRouter => gents::inference_setup::OPENROUTER_ENDPOINT.to_owned(),
        }
    }
}

fn live_enabled() -> bool {
    std::env::var("GENTS_LIVE_CONFIG").as_deref() == Ok("1")
}

fn model_name() -> String {
    std::env::var("GENTS_D4F_MODEL").unwrap_or_else(|_| "GLM-5.3-Flash-NVFP4".to_owned())
}

fn eval_models() -> Vec<String> {
    let mut models = std::env::var("GENTS_LIVE_CONFIG_MODELS")
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
        .unwrap_or_else(|| vec![model_name()]);
    let mut seen = std::collections::HashSet::new();
    models.retain(|model| seen.insert(model.clone()));
    models
}

fn eval_runs() -> usize {
    let runs = std::env::var("GENTS_LIVE_CONFIG_RUNS")
        .ok()
        .map(|value| {
            value
                .parse::<usize>()
                .expect("GENTS_LIVE_CONFIG_RUNS must be an integer")
        })
        .unwrap_or(10);
    assert!(
        (1..=100).contains(&runs),
        "GENTS_LIVE_CONFIG_RUNS must be between 1 and 100"
    );
    runs
}

fn eval_concurrency() -> usize {
    let concurrency = std::env::var("GENTS_LIVE_CONFIG_CONCURRENCY")
        .ok()
        .map(|value| {
            value
                .parse::<usize>()
                .expect("GENTS_LIVE_CONFIG_CONCURRENCY must be an integer")
        })
        .unwrap_or(1);
    validate_eval_concurrency(concurrency).expect("valid eval concurrency");
    concurrency
}

fn validate_eval_concurrency(concurrency: usize) -> Result<()> {
    ensure!(
        (1..=30).contains(&concurrency),
        "GENTS_LIVE_CONFIG_CONCURRENCY must be between 1 and 30"
    );
    Ok(())
}

#[test]
fn eval_concurrency_accepts_thirty_and_rejects_out_of_bounds() {
    for concurrency in [1, 10, 20, 30] {
        assert!(validate_eval_concurrency(concurrency).is_ok());
    }
    for concurrency in [0, 31, usize::MAX] {
        assert!(validate_eval_concurrency(concurrency).is_err());
    }
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
    user_home: &str,
) {
    let context_id = format!("{setup_behavior_id}:config-context");
    let tools_id = format!("{setup_behavior_id}:config-tools");
    let prompt = gents_protocol::SETUP_STEWARD_PROMPT;
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
        host: Some(gents::document_config::HostTools {
            root: Some(user_home.to_owned()),
            files: Some(gents::document_config::FileTools {
                mode: gents::tool_surface::FileToolMode::ReadOnly,
                ..Default::default()
            }),
            ..Default::default()
        }),
        self_config: Some(gents::agent::persona_ops::setup_steward_self_config()),
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
    backend_id: &str,
    model: &str,
    setup_behavior_id: &str,
) {
    let reasoning_effort = eval_reasoning_effort().expect("valid eval reasoning effort");
    let sampling = InferenceSampling {
        agent_did: agent_did.to_owned(),
        sampling_id: EVAL_SAMPLING_ID.to_owned(),
        display_name: Some("Configurator eval sampling".to_owned()),
        temperature: Some(EVAL_TEMPERATURE),
        top_p: Some(EVAL_TOP_P),
        ..Default::default()
    };
    let setup_profile = gents::default_inference_profile_id_for_behavior(setup_behavior_id);
    let profiles = [("high", "High"), ("medium", "Medium"), ("low", "Low")]
        .into_iter()
        .map(|(profile_id, display_name)| InferenceProfile {
            agent_did: agent_did.to_owned(),
            profile_id: profile_id.to_owned(),
            backend_id: backend_id.to_owned(),
            model_name: model.to_owned(),
            display_name: Some(display_name.to_owned()),
            sampling_id: Some(EVAL_SAMPLING_ID.to_owned()),
            reasoning_effort,
            ..Default::default()
        })
        .chain(std::iter::once(InferenceProfile {
            agent_did: agent_did.to_owned(),
            profile_id: setup_profile,
            backend_id: backend_id.to_owned(),
            model_name: model.to_owned(),
            display_name: Some("Live default behavior".to_owned()),
            sampling_id: Some(EVAL_SAMPLING_ID.to_owned()),
            reasoning_effort,
            ..Default::default()
        }));
    let documents = std::iter::once((
        Collection::InferenceSampling,
        serde_json::to_value(sampling).expect("serialize eval sampling"),
    ))
    .chain(profiles.map(|profile| {
        let value = serde_json::to_value(profile).expect("serialize eval profile");
        (Collection::InferenceProfile, value)
    }))
    .map(
        |(collection, value)| gents::config_client::DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        },
    )
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
    backend_id: &str,
    model: &str,
) -> Result<()> {
    let owner = gents::graphql::escape_graphql_string(agent_did);
    let profiles = rows(
        node,
        &format!(
            r#"{{ InferenceProfile(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{profile_id backend_id model_name sampling_id}} }}"#
        ),
        "InferenceProfile",
    )
    .await?;
    for profile_id in ["high", "medium", "low"] {
        let profile = profiles
            .iter()
            .find(|profile| profile["profile_id"] == profile_id)
            .with_context(|| format!("seeded profile {profile_id:?} disappeared"))?;
        ensure!(profile["backend_id"] == backend_id);
        ensure!(profile["model_name"] == model);
        ensure!(profile["sampling_id"] == EVAL_SAMPLING_ID);
    }
    let sampling = rows(
        node,
        &format!(
            r#"{{ InferenceSampling(filter: {{agent_did: {{_eq: "{owner}"}}, sampling_id: {{_eq: "{EVAL_SAMPLING_ID}"}}}}) {{temperature top_p seed}} }}"#
        ),
        "InferenceSampling",
    )
    .await?;
    ensure!(
        sampling.len() == 1,
        "eval sampling document is missing or duplicated"
    );
    ensure!(sampling[0]["temperature"] == EVAL_TEMPERATURE);
    ensure!(sampling[0]["top_p"] == EVAL_TOP_P);
    ensure!(sampling[0]["seed"].is_null());

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

async fn run_eval_trial(
    provider: LiveProvider,
    model: String,
    trial: usize,
    artifacts: &std::path::Path,
) -> reporting::TrialResult {
    let db = retained_trial_db(artifacts).await;
    let access = gents::ConfigAccess::Local(db.node.clone());
    let schema = gents::config_client::preview_schema_install(&access, stages::INPUT_SCHEMA)
        .await
        .expect("preview runner schema");
    gents::config_client::apply_schema_install(
        &access,
        stages::INPUT_SCHEMA,
        &schema.artifact_digest,
    )
    .await
    .expect("install runner schema");
    let evidence = artifacts.join("evidence");
    let workspace = artifacts.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create isolated workspace");
    let user_home = workspace.to_string_lossy().into_owned();
    tracing::info!(target: "gents::configurator_eval", model, trial, artifacts = %artifacts.display(), "retained eval artifacts");
    let identity: Arc<dyn AgentIdentity> = Arc::new(
        gents::KeyIdentity::load_or_create(db.data_path().join("agent.key"), None)
            .expect("retained trial principal identity"),
    );
    let (agent_did, setup_behavior_id) = match provider {
        LiveProvider::D4f => {
            bind_d4f_backend_for_model(db.node.as_ref(), identity.as_ref(), &model).await
        }
        LiveProvider::OpenRouter => {
            bind_openrouter_backend_for_model(db.node.as_ref(), identity.as_ref(), &model).await
        }
    };
    install_eval_profiles(
        db.node.as_ref(),
        &agent_did,
        provider.backend_id(),
        &model,
        &setup_behavior_id,
    )
    .await;
    install_eval_workspace_root(db.node.as_ref(), &user_home).await;
    install_setup_configurator(db.node.as_ref(), &agent_did, &setup_behavior_id, &user_home).await;
    let observer = Arc::new(stages::ActivationObserver::default());
    let (agent, runtime) = boot_d4f_agent_with_options(
        &db,
        identity,
        gents::DocumentRuntimeOptions {
            tool_ceiling: gents::ToolCeiling::readwrite(&user_home),
            runtime_snapshot_observer: Some(observer.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("boot live configurator");
    let activation = stages::ActivationFence::new(runtime, observer, db.node.clone());

    // A fixture/assertion panic must not leave this trial's runtime consuming
    // inference capacity while the matrix moves on to another isolated trial.
    let result = AssertUnwindSafe(async {
        let mut terminal = None;
        let mut answer = String::new();
        let verification = stages::checked(
            stages::CaseId::Onboarding,
            &evidence,
            stages::acceptance(async {
                let onboarding = stages::execute(
                    &activation,
                    db.node.as_ref(),
                    &agent_did,
                    &setup_behavior_id,
                    "onboarding",
                    &onboarding_prompt(&user_home),
                    &evidence,
                )
                .await?;
                terminal = Some(onboarding.terminal_state.clone());
                answer = onboarding.answer.clone();
                onboarding.ensure_completed()?;
                verify_configuration(
                    db.node.as_ref(),
                    &agent_did,
                    &setup_behavior_id,
                    &user_home,
                    provider.backend_id(),
                    &model,
                )
                .await
            }),
        )
        .await;
        let configured = verification.is_ok();
        let mut failures = verification.err().into_iter().collect::<Vec<_>>();
        if configured {
            let result = stages::checked(
                stages::CaseId::BuilderReadiness,
                &evidence,
                stages::acceptance(cases::verify_builder_execution(
                    &activation,
                    db.node.as_ref(),
                    &agent_did,
                    &user_home,
                    &evidence,
                )),
            )
            .await;
            failures.extend(result.err());
        }
        if configured {
            let result = stages::checked(
                stages::CaseId::SkillWorkflow,
                &evidence,
                stages::acceptance(async {
                    cases::verify_skill_workflow(
                        &activation,
                        db.node.as_ref(),
                        &agent_did,
                        &setup_behavior_id,
                        &workspace,
                        &evidence,
                    )
                    .await?;
                    verify_configuration(
                        db.node.as_ref(),
                        &agent_did,
                        &setup_behavior_id,
                        &user_home,
                        provider.backend_id(),
                        &model,
                    )
                    .await
                }),
            )
            .await;
            failures.extend(result.err());
        }
        if configured {
            let result = stages::checked(
                stages::CaseId::DocumentAutomation,
                &evidence,
                stages::acceptance(async {
                    cases::verify_document_automation(
                        &activation,
                        db.node.as_ref(),
                        &agent_did,
                        &setup_behavior_id,
                        &evidence,
                    )
                    .await?;
                    verify_configuration(
                        db.node.as_ref(),
                        &agent_did,
                        &setup_behavior_id,
                        &user_home,
                        provider.backend_id(),
                        &model,
                    )
                    .await
                }),
            )
            .await;
            failures.extend(result.err());
        }
        let report = reporting::TrialResult {
            case_id: EVAL_CASE_ID,
            provider: provider.name(),
            model,
            trial,
            passed: failures.is_empty(),
            trial_failure_kind: None,
            terminal_state: terminal,
            error: (!failures.is_empty()).then(|| {
                failures
                    .iter()
                    .map(|error| format!("{error:#}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            }),
            assistant_answer_excerpt: Some(excerpt(&answer, 2_000)),
            artifacts: Some(artifacts.to_string_lossy().into_owned()),
            cases: stages::case_results(stages::PROGRESSIVE_CASES, &evidence)
                .expect("collect independent case results"),
        };
        report
    })
    .catch_unwind()
    .await;
    agent.shutdown().await;
    db.node.shutdown().await;
    match result {
        Ok(report) => report,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

async fn retained_trial_db(artifacts: &std::path::Path) -> crate::support::TestDb {
    let mut home = tempfile::Builder::new()
        .prefix("home-")
        .tempdir_in(artifacts)
        .expect("isolated trial database home");
    home.disable_cleanup(true);
    test_db_in(home).await
}

#[tokio::test]
async fn trial_database_homes_are_isolated_and_retained() {
    let artifacts = tempfile::tempdir().unwrap();
    let first = retained_trial_db(artifacts.path()).await;
    let second = retained_trial_db(artifacts.path()).await;
    assert_ne!(first.node_identity.did(), second.node_identity.did());
    let first_home = first.data_path().to_path_buf();
    let first_did = first.node_identity.did().to_string();
    install_eval_workspace_root(first.node.as_ref(), artifacts.path().to_str().unwrap()).await;
    assert!(rows(
        second.node.as_ref(),
        "{ WorkspaceRoot { root_path } }",
        "WorkspaceRoot"
    )
    .await
    .unwrap()
    .is_empty());
    first.node.shutdown().await;
    second.node.shutdown().await;
    drop((first, second));
    let homes = std::fs::read_dir(artifacts.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(homes.len(), 2);
    for home in homes {
        assert!(home.join("node.key").is_file());
        assert!(std::fs::read_dir(home).unwrap().count() > 1);
    }
    let reopened = gents::defra_node::EmbeddedNode::builder()
        .data_path(first_home)
        .with_node_identity_did(&first_did)
        .build()
        .await
        .unwrap();
    let retained = rows(
        &reopened,
        "{ WorkspaceRoot { root_path } }",
        "WorkspaceRoot",
    )
    .await
    .unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0]["root_path"], artifacts.path().to_str().unwrap());
    reopened.shutdown().await;
}

async fn run_retained_trial(
    provider: LiveProvider,
    model: String,
    trial: usize,
    artifacts: std::path::PathBuf,
) -> reporting::TrialResult {
    let evidence = artifacts.join("evidence");
    std::fs::create_dir_all(&evidence).expect("create evidence directory");
    tracing::info!(target: "gents::configurator_eval", artifacts = %artifacts.display(), "starting trial");
    let work = async {
        if host_suite() {
            host_scenarios::run_trial(model.clone(), trial, &artifacts).await
        } else if monitor_suite() {
            onboarding_scenarios::run_monitor_trial(model.clone(), trial, &artifacts).await
        } else {
            Ok(run_eval_trial(provider, model.clone(), trial, &artifacts).await)
        }
    };
    let report = match AssertUnwindSafe(work).catch_unwind().await {
        Ok(Ok(report)) => report,
        failure => reporting::TrialResult {
            case_id: suite_id(),
            provider: provider.name(),
            model,
            trial,
            passed: false,
            trial_failure_kind: Some("infrastructure".into()),
            terminal_state: None,
            error: Some(match failure {
                Ok(Err(error)) => format!("{error:#}"),
                Err(error) => format!("eval trial panicked: {}", panic_message(error.as_ref())),
                Ok(Ok(_)) => unreachable!(),
            }),
            assistant_answer_excerpt: None,
            artifacts: Some(artifacts.to_string_lossy().into_owned()),
            cases: stages::case_results(suite_cases(), &evidence).unwrap_or_default(),
        },
    };
    reporting::write_json_new(&evidence.join("trial.json"), &report)
        .expect("retain immutable trial result, including fixture failures");
    tracing::info!(target: "gents::configurator_eval", passed = report.passed, "completed trial");
    report
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_CONFIG=1 and pass --ignored"]
async fn live_configurator_progressive_eval_matrix() {
    assert!(
        matches!(
            std::env::var("GENTS_EVAL_SUITE").as_deref(),
            Err(_) | Ok("progressive-configurator") | Ok("monitor-mailbox") | Ok("host-steward")
        ),
        "unsupported eval suite"
    );
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
    let provider = LiveProvider::from_env();
    assert!(
        !(monitor_suite() || host_suite()) || matches!(provider, LiveProvider::D4f),
        "monitor-mailbox currently supports the local D4F provider only"
    );
    let concurrency = eval_concurrency();
    let stage_timeout = stages::stage_timeout().expect("valid stage deadline");
    let directory = std::path::PathBuf::from(
        std::env::var_os("GENTS_EVAL_RUN_DIR")
            .expect("use make live-configurator-eval to allocate a run directory"),
    );
    std::fs::create_dir(directory.join("trials")).expect("fresh run directory");
    let mut run_report = reporting::RunReport::new(
        directory.clone(),
        suite_id(),
        suite_cases(),
        models.clone(),
        runs,
        provider.name(),
        concurrency,
        stage_timeout.as_secs(),
        if host_suite() {
            host_scenarios::provenance().expect("collect host provenance")
        } else if monitor_suite() {
            onboarding_scenarios::monitor_provenance().expect("collect monitor provenance")
        } else {
            reporting::RunProvenance::current(
                EVAL_COHORT,
                EVAL_GRADER,
                provider.endpoint(),
                EVAL_SAMPLING_ID,
                EVAL_TEMPERATURE,
                EVAL_TOP_P,
                EVAL_GRADER_SOURCES,
                EVAL_FIXTURES,
            )
            .expect("collect eval provenance")
        },
    )
    .expect("initialize eval report");
    run_report.save().expect("initialize run report");
    let trials = models
        .into_iter()
        .enumerate()
        .flat_map(|(index, model)| (1..=runs).map(move |trial| (index, model.clone(), trial)));
    let mut results = futures::stream::iter(trials)
        .map(|(index, model, trial)| {
            let artifacts = directory
                .join("trials")
                .join(format!("model-{:03}-trial-{trial:03}", index + 1));
            let span =
                tracing::info_span!(target: "gents::configurator_eval", "trial", %model, trial);
            run_retained_trial(provider, model, trial, artifacts).instrument(span)
        })
        .buffer_unordered(concurrency);
    while let Some(result) = results.next().await {
        run_report
            .record(result)
            .expect("record planned eval trial exactly once");
        run_report.save().expect("checkpoint run report");
    }
    assert!(
        run_report.failed() == 0,
        "{} of {} configurator eval trials failed; see {}",
        run_report.failed(),
        run_report.completed(),
        directory.join("report.json").display(),
    );
}
