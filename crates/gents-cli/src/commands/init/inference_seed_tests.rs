use super::*;
use clap::Parser;
use gents_lean_vocab::families::initialization_initial_inference as family;

fn parse_case(case: &family::SeedInput) -> InitArgs {
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
    let Some(Command::Init(args)) = Cli::try_parse_from(argv).unwrap().command else {
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
    for case in family::cases() {
        let args = parse_case(&case.input);
        let home = gents::runtime::embedded::EmbeddedHome::create_temp("init-seed").await?;
        let identity = home.identity.clone();
        let owner = home.did();
        let node = home.node.clone();
        let graphql = home.graphql_endpoint().context("served init home")?;
        let user_home = tempfile::tempdir()?;
        let user = KeyIdentity::load_or_create(user_home.path().join("user.key"), None)?;
        let binding = provision_fresh_home(&node, identity.as_ref(), user.did()).await?;
        let access = gents_host::test_support::user_access(graphql, &user);
        let initialized = initialize_runtime_home(
            &access,
            &node,
            &args,
            identity.as_ref(),
            resolve_initial_tool_package(&args)?,
            Some(binding),
            None,
        )
        .await;
        let Ok(summary) = initialized else {
            assert!(
                gents_inference::backends::registry::list_all_backends(&node)
                    .await?
                    .is_empty()
            );
            assert!(load_node(&node, owner).await?.is_none());
            case.check(None);
            continue;
        };
        let backends = gents_inference::backends::registry::list_all_backends(&node).await?;
        assert_eq!(backends.len(), 1);
        let backend = &backends[0];
        let profile =
            gents_documents::load_inference_profile(&node, owner, &summary.inference_profile_id)
                .await?
                .expect("the agent's profile exists even before model selection");
        let model = (!profile.model_name.is_empty()).then(|| profile.model_name.clone());
        let observation = gents_inference::backends::registry::lookup_backend_observation(
            &node,
            owner,
            &summary.backend_id,
        )
        .await?
        .expect("the backend exists");
        assert_eq!(
            observation.probe_status.is_some(),
            observation.last_probe.is_some()
        );
        let mut unselected_model_reason = None;
        let mut enabled_unselected_model_reason = None;
        if model.is_none() {
            let runtime = gents::Gents::from_default_agent_documents(
                node.clone(),
                identity.clone(),
                gents::DocumentRuntimeOptions {
                    backend_admission: Some(home.backend_admission()),
                    ..Default::default()
                },
            )
            .await?;
            assert!(runtime.agents().is_empty());
            let unavailable = runtime
                .unavailable_agents()
                .get(&summary.default_agent_did)
                .expect("the unselected agent remains visible for setup");
            unselected_model_reason = serde_json::to_value(unavailable.public_reason)?
                .as_str()
                .map(str::to_owned);
            drop(runtime);
            let mut enabled_backend = backend.clone();
            enabled_backend.enabled = true;
            publish_home_config(
                &access,
                owner,
                "Incomplete selection",
                &summary.default_agent_did,
                true,
                vec![replacement(Collection::InferenceBackend, &enabled_backend)?],
            )
            .await?;
            let runtime = gents::Gents::from_default_agent_documents(
                node.clone(),
                identity.clone(),
                gents::DocumentRuntimeOptions {
                    backend_admission: Some(home.backend_admission()),
                    ..Default::default()
                },
            )
            .await?;
            assert!(runtime.agents().is_empty());
            let unavailable = runtime
                .unavailable_agents()
                .get(&summary.default_agent_did)
                .expect("an enabled backend still requires model selection");
            enabled_unselected_model_reason = serde_json::to_value(unavailable.public_reason)?
                .as_str()
                .map(str::to_owned);
            drop(runtime);
            let steps = init_next_steps(&summary, Path::new("/srv/home"), false, false, false);
            assert!(
                !steps
                    .iter()
                    .any(|step| step.starts_with("llama-server -hf")
                        || step.starts_with("ollama pull")),
                "{steps:?}"
            );
        }
        case.check(Some(family::SeedPlan {
            model,
            enabled: backend.enabled,
            concurrent: backend.max_concurrent,
            effective_concurrent: backend.effective_max_concurrent() as u64,
            context_override: profile.context_window,
            output_override: profile.max_output_tokens,
            records_probe: observation.probe_status.is_some(),
            unselected_model_reason,
            enabled_unselected_model_reason,
        }));
    }
    Ok(())
}

#[tokio::test]
async fn provider_selection_after_unconfigured_init_uses_the_discovered_limits() -> Result<()> {
    let Some(Command::Init(args)) = Cli::try_parse_from(["gents", "init", "--engineer"])?.command
    else {
        panic!("expected init");
    };
    let home = gents::runtime::embedded::EmbeddedHome::create_temp("init-seed").await?;
    let identity = home.identity.clone();
    let owner = home.did();
    let node = home.node.clone();
    let user_home = tempfile::tempdir()?;
    let user = KeyIdentity::load_or_create(user_home.path().join("user.key"), None)?;
    let binding = provision_fresh_home(&node, identity.as_ref(), user.did()).await?;
    let access = gents_host::test_support::user_access(
        home.graphql_endpoint().context("served init home")?,
        &user,
    );
    let summary = initialize_runtime_home(
        &access,
        &node,
        &args,
        identity.as_ref(),
        resolve_initial_tool_package(&args)?,
        Some(binding),
        None,
    )
    .await?;
    let mut backend = gents_inference::backends::registry::list_all_backends(&node)
        .await?
        .remove(0);
    let mut profile =
        gents_documents::load_inference_profile(&node, owner, &summary.inference_profile_id)
            .await?
            .unwrap();
    backend.enabled = true;
    backend.endpoint = "http://127.0.0.1:8000/v1".into();
    profile.model_name = "selected-model".into();
    publish_home_config(
        &access,
        owner,
        "Selected provider",
        &summary.default_agent_did,
        true,
        vec![
            replacement(Collection::InferenceBackend, &backend)?,
            replacement(Collection::InferenceProfile, &profile)?,
        ],
    )
    .await?;
    let advertised = gents_documents::AdvertisedModel {
        model_name: profile.model_name.clone(),
        display_name: None,
        context_window: Some(1_048_576),
        max_context_window: None,
        max_output_tokens: Some(65_536),
        reasoning_efforts: None,
    };
    gents_inference::backends::registry::record_discovered_catalog_on(
        &access,
        &backend,
        vec![advertised.clone()],
    )
    .await?;
    gents_inference::backends::registry::set_backend_probe_status_with_last_probe_as(
        &gents_store::config_client::ConfigAccess::local(node.clone()),
        owner,
        &backend.backend_id,
        gents_inference::backends::registry::HEALTHY_PROBE_STATUS,
        chrono::Utc::now(),
    )
    .await?;
    let runtime = gents::Gents::from_default_agent_documents(
        node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            backend_admission: Some(home.backend_admission()),
            ..Default::default()
        },
    )
    .await?;
    let resolved = runtime
        .agents()
        .iter()
        .find(|agent| agent.agent.agent_did == summary.default_agent_did)
        .expect("explicit selection makes the configured agent available");
    assert_eq!(resolved.agent.model_name, advertised.model_name);
    assert_eq!(
        resolved.agent.context_window as i64,
        advertised.context_window.unwrap()
    );
    assert_eq!(
        resolved.agent.max_output_tokens as i64,
        advertised.max_output_tokens.unwrap()
    );
    drop(runtime);
    node.shutdown().await.expect("node shutdown");
    Ok(())
}
