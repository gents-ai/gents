use super::*;
use clap::Parser;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct SeedCase {
    engineer: bool,
    requested_model: Option<String>,
    local_default: String,
    preset: Option<String>,
    preset_default: Option<String>,
    concurrent: Option<i64>,
    expected: Option<SeedPlan>,
}

#[derive(Deserialize, Debug)]
struct SeedPlan {
    model: Option<String>,
    enabled: bool,
    concurrent: Option<i64>,
    effective_concurrent: i64,
    context_override: Option<i64>,
    output_override: Option<i64>,
    records_probe: bool,
    unselected_model_reason: Option<String>,
    enabled_unselected_model_reason: Option<String>,
}

#[derive(Deserialize)]
struct Snapshot {
    initial_inference_cases: Vec<SeedCase>,
}

fn parse_case(case: &SeedCase) -> InitArgs {
    let mut argv = vec!["gents".to_string(), "init".to_string()];
    if case.engineer {
        argv.push("--engineer".into());
    }
    for (flag, value) in [
        ("--model-name", case.requested_model.clone()),
        ("--backend-preset", case.preset.clone()),
        (
            "--max-concurrent",
            case.concurrent.map(|value| value.to_string()),
        ),
    ] {
        if let Some(value) = value {
            argv.extend([flag.to_string(), value]);
        }
    }
    let Command::Init(args) = Cli::try_parse_from(argv).unwrap().command else {
        panic!("expected init");
    };
    assert_eq!(case.local_default, crate::DEFAULT_INIT_MODEL_NAME);
    assert_eq!(
        args.preset_default_model_name(),
        case.preset_default.as_deref()
    );
    args
}

#[tokio::test]
async fn generated_initial_inference_keeps_unselected_models_and_limits_unpinned() -> Result<()> {
    let snapshot: Snapshot = gents_lean_contract::load_contract_snapshot()?;
    assert!(!snapshot.initial_inference_cases.is_empty());
    for case in snapshot.initial_inference_cases {
        let args = parse_case(&case);
        let temp = tempfile::tempdir()?;
        let identity = Arc::new(KeyIdentity::load_or_create(
            temp.path().join("identity.key"),
            None,
        )?);
        let owner = identity.did();
        let node = Arc::new(
            gents::defra_node::EmbeddedNode::builder()
                .with_node_identity_did(owner.to_string())
                .build()
                .await?,
        );
        gents::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::local(node.clone());
        let initialized =
            initialize_runtime_home(&access, &args, owner, resolve_initial_tool_package(&args)?)
                .await;
        let Some(expected) = &case.expected else {
            assert!(initialized.is_err(), "{case:?}");
            assert!(
                gents::inference::backends::registry::list_all_backends(&node)
                    .await?
                    .is_empty()
            );
            assert!(load_node(&node, owner).await?.is_none());
            node.shutdown().await;
            continue;
        };
        let summary = initialized.with_context(|| format!("{case:?}"))?;
        let backends = gents::inference::backends::registry::list_all_backends(&node).await?;
        assert_eq!(backends.len(), 1, "{case:?}");
        let backend = &backends[0];
        assert_eq!(backend.enabled, expected.enabled, "{case:?}");
        assert_eq!(backend.max_concurrent, expected.concurrent, "{case:?}");
        assert_eq!(
            backend.effective_max_concurrent(),
            expected.effective_concurrent
        );
        let profile = gents::document_config::load_inference_profile(
            &node,
            owner,
            &summary.inference_profile_id,
        )
        .await?
        .expect("the agent's profile exists even before model selection");
        assert_eq!(
            (!profile.model_name.is_empty()).then_some(profile.model_name.as_str()),
            expected.model.as_deref(),
            "{case:?}",
        );
        assert_eq!(
            profile.context_window, expected.context_override,
            "{case:?}"
        );
        assert_eq!(
            profile.max_output_tokens, expected.output_override,
            "{case:?}"
        );
        let observation = gents::inference::backends::registry::lookup_backend_observation(
            &node,
            owner,
            &summary.backend_id,
        )
        .await?
        .expect("the backend exists");
        assert_eq!(
            observation.probe_status.is_some(),
            expected.records_probe,
            "{case:?}"
        );
        assert_eq!(
            observation.last_probe.is_some(),
            expected.records_probe,
            "{case:?}"
        );
        if expected.model.is_none() {
            let runtime = gents::Gents::from_default_agent_documents(
                node.clone(),
                identity.clone(),
                gents::DocumentRuntimeOptions::default(),
            )
            .await?;
            assert!(runtime.agents().is_empty());
            let unavailable = runtime
                .unavailable_agents()
                .get(&summary.default_agent_id)
                .expect("the unselected agent remains visible for setup");
            assert_eq!(
                serde_json::to_value(unavailable.public_reason)?.as_str(),
                expected.unselected_model_reason.as_deref(),
                "{case:?}",
            );
            drop(runtime);
            let mut enabled_backend = backend.clone();
            enabled_backend.enabled = true;
            publish_home_config(
                &access,
                owner,
                "Incomplete selection",
                &summary.default_agent_id,
                true,
                vec![replacement(Collection::InferenceBackend, &enabled_backend)?],
            )
            .await?;
            let runtime = gents::Gents::from_default_agent_documents(
                node.clone(),
                identity.clone(),
                gents::DocumentRuntimeOptions::default(),
            )
            .await?;
            assert!(runtime.agents().is_empty());
            let unavailable = runtime
                .unavailable_agents()
                .get(&summary.default_agent_id)
                .expect("an enabled backend still requires model selection");
            assert_eq!(
                serde_json::to_value(unavailable.public_reason)?.as_str(),
                expected.enabled_unselected_model_reason.as_deref(),
                "{case:?}",
            );
            drop(runtime);
            let steps = init_next_steps(&summary, false, false, false);
            assert!(
                !steps
                    .iter()
                    .any(|step| step.starts_with("llama-server -hf")
                        || step.starts_with("ollama pull")),
                "{steps:?}"
            );
        }
        node.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn provider_selection_after_unconfigured_init_uses_the_discovered_limits() -> Result<()> {
    let Command::Init(args) = Cli::try_parse_from(["gents", "init", "--engineer"])?.command else {
        panic!("expected init");
    };
    let temp = tempfile::tempdir()?;
    let identity = Arc::new(KeyIdentity::load_or_create(
        temp.path().join("identity.key"),
        None,
    )?);
    let owner = identity.did();
    let node = Arc::new(
        gents::defra_node::EmbeddedNode::builder()
            .with_node_identity_did(owner.to_string())
            .build()
            .await?,
    );
    gents::ensure_runtime_schemas(&node).await?;
    let access = ConfigAccess::local(node.clone());
    let summary =
        initialize_runtime_home(&access, &args, owner, resolve_initial_tool_package(&args)?)
            .await?;
    let mut backend = gents::inference::backends::registry::list_all_backends(&node)
        .await?
        .remove(0);
    let mut profile =
        gents::document_config::load_inference_profile(&node, owner, &summary.inference_profile_id)
            .await?
            .unwrap();
    backend.enabled = true;
    backend.endpoint = "http://127.0.0.1:8000/v1".into();
    profile.model_name = "selected-model".into();
    publish_home_config(
        &access,
        owner,
        "Selected provider",
        &summary.default_agent_id,
        true,
        vec![
            replacement(Collection::InferenceBackend, &backend)?,
            replacement(Collection::InferenceProfile, &profile)?,
        ],
    )
    .await?;
    let advertised = gents::document_config::AdvertisedModel {
        model_name: profile.model_name.clone(),
        display_name: None,
        context_window: Some(1_048_576),
        max_context_window: None,
        max_output_tokens: Some(65_536),
        reasoning_efforts: None,
    };
    gents::inference::backends::registry::record_discovered_catalog_on(
        &access,
        &backend,
        vec![advertised.clone()],
    )
    .await?;
    gents::inference::backends::registry::set_backend_probe_status_with_last_probe(
        &node,
        owner,
        &backend.backend_id,
        gents::HEALTHY_PROBE_STATUS,
        chrono::Utc::now(),
    )
    .await?;
    let runtime = gents::Gents::from_default_agent_documents(
        node.clone(),
        identity,
        gents::DocumentRuntimeOptions::default(),
    )
    .await?;
    let resolved = runtime
        .agents()
        .iter()
        .find(|agent| agent.agent_id == summary.default_agent_id)
        .expect("explicit selection makes the configured agent available");
    assert_eq!(resolved.model_name, advertised.model_name);
    assert_eq!(
        resolved.context_window as i64,
        advertised.context_window.unwrap()
    );
    assert_eq!(
        resolved.max_output_tokens as i64,
        advertised.max_output_tokens.unwrap()
    );
    drop(runtime);
    node.shutdown().await;
    Ok(())
}
