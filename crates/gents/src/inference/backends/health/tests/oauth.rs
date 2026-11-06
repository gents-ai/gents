use super::*;

async fn oauth_backend_configuration(access: &crate::ConfigAccess, backend: &InferenceBackend) {
    crate::store::config_client::write_inference_backend_document(access, backend)
        .await
        .unwrap();
}

async fn oauth_backend_observation(
    access: &crate::ConfigAccess,
    runtime_access: &crate::ConfigAccess,
    backend: &InferenceBackend,
    status: &str,
) {
    oauth_backend_configuration(access, backend).await;
    crate::inference::backends::registry::set_backend_probe_status_as(
        runtime_access,
        &backend.node_did,
        &backend.backend_id,
        status,
    )
    .await
    .unwrap();
}

fn claude_backend() -> InferenceBackend {
    let mut claude = backend(
        "claude",
        crate::inference::clients::claude_subscription::DEFAULT_BACKEND_ENDPOINT.to_string(),
    );
    claude.provider_kind =
        crate::inference::backends::provider::BackendProviderKind::ClaudeCliSubscription;
    claude.auth = crate::documents::BackendAuth::NodeOAuth { account_ref: None };
    claude
}

fn oauth_backend(
    kind: crate::inference::backends::provider::BackendProviderKind,
    id: &str,
    endpoint: &str,
) -> InferenceBackend {
    let mut backend = backend(id, endpoint.to_string());
    backend.provider_kind = kind;
    backend.auth = crate::documents::BackendAuth::NodeOAuth { account_ref: None };
    backend
}

#[tokio::test]
async fn oauth_kinds_probe_the_credential_document_fresh_is_healthy_and_promotes() {
    let (db, _user, user_access, _binding) =
        crate::test_support::signed_test_node("health-oauth").await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let runtime_access =
        crate::inference::auth::credential::runtime_credential_access(&node, did).unwrap();
    seed_credential(
        &runtime_access,
        did,
        crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
        Utc::now() + chrono::Duration::hours(8),
    )
    .await;
    let (options, client, health_map) = (
        probe_options(),
        reqwest::Client::new(),
        BackendHealthMap::new(),
    );
    let mut claude = claude_backend();
    claude.node_did = did.to_string();
    let models = ModelsListener::start();
    claude.endpoint = models.endpoint();
    oauth_backend_observation(&user_access, &runtime_access, &claude, "unknown").await;
    let outcome = probe_backends_cycle(
        &node,
        &client,
        std::slice::from_ref(&claude),
        Utc::now(),
        &health_map,
        &options,
        Some(OAuthProbeContext {
            node: node.clone(),
            node_did: did,
        }),
    )
    .await;
    assert_eq!(outcome.promotable, vec!["claude".to_string()]);
    let snap = health_map.get("claude").await.expect("entry");
    assert_eq!(snap.state, BackendHealthState::Healthy);
    assert!(snap.last_error.is_none());
}

async fn toggled_models_server(
    available: Arc<std::sync::atomic::AtomicBool>,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/v1/models",
        get(move || {
            let available = available.clone();
            async move {
                if available.load(Ordering::SeqCst) {
                    Ok(Json(serde_json::json!({"data": [{"id": "test-model"}]})))
                } else {
                    Err(axum::http::StatusCode::NOT_FOUND)
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (endpoint, server)
}

#[tokio::test]
async fn scheduled_oauth_probe_failure_records_a_bounded_reason_and_recovers() {
    let (db, _user, user_access, _binding) =
        crate::test_support::signed_test_node("health-oauth").await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let runtime_access =
        crate::inference::auth::credential::runtime_credential_access(&node, did).unwrap();
    let mut claude = claude_backend();
    claude.node_did = did.to_owned();
    seed_credential(
        &runtime_access,
        &claude.node_did,
        crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
        Utc::now() + chrono::Duration::hours(8),
    )
    .await;
    let available = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (endpoint, server) = toggled_models_server(available.clone()).await;
    claude.endpoint = endpoint.clone();
    oauth_backend_configuration(&user_access, &claude).await;
    let health = BackendHealthMap::new();
    let mut schedule = ProbeSchedule::default();

    let before = Utc::now();
    let outcome = run_scheduled_backend_probe_cycle(
        node.clone(),
        &reqwest::Client::new(),
        &health,
        &probe_options(),
        did,
        Some((&mut schedule, true)),
        None,
    )
    .await;
    assert!(outcome.promotable.is_empty());
    let failed = health
        .get("claude")
        .await
        .expect("failed probe is observed");
    assert_modeled_probe(None, &failed, "probeFail");
    assert!(failed.last_probe_at >= before);
    let report = failed.probe_report();
    assert_eq!(report["state"], "degraded");
    let reason = report["last_error"].as_str().expect("failure reason");
    assert!(reason.contains("404"), "{reason}");
    assert!(reason.contains(&format!("{endpoint}/models")), "{reason}");
    assert!(!reason.contains("access-TEST"), "{reason}");
    assert!(reason.chars().count() <= MAX_PROBE_ERROR_CHARS + 3);

    available.store(true, Ordering::SeqCst);
    let outcome = run_scheduled_backend_probe_cycle(
        node.clone(),
        &reqwest::Client::new(),
        &health,
        &probe_options(),
        did,
        Some((&mut schedule, true)),
        None,
    )
    .await;
    assert_eq!(outcome.promotable, vec!["claude".to_string()]);
    let recovered = health.get("claude").await.unwrap();
    assert_modeled_probe(Some(&failed), &recovered, "probeSuccess");
    assert!(recovered.last_error.is_none());
    let observation = user_access
        .transact_readonly("test.health.observation", |txn| {
            let backend_id = &claude.backend_id;
            Box::pin(async move {
                crate::inference::backends::registry::lookup_backend_observation_in_txn(
                    txn, did, backend_id,
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observation.probe_status.as_deref(), Some("healthy"));
    assert!(observation.last_probe.is_some());
    server.abort();
    node.shutdown().await.expect("node shutdown");
}

#[test]
fn probe_errors_are_bounded_on_a_char_boundary() {
    let long = "é".repeat(MAX_PROBE_ERROR_CHARS * 2);
    let bounded = bounded_probe_error(long);
    assert_eq!(bounded.chars().count(), MAX_PROBE_ERROR_CHARS + 3);
    assert!(bounded.ends_with("..."));
    assert_eq!(bounded_probe_error("short".into()), "short");
}

#[tokio::test]
async fn oauth_account_reference_is_probed_with_its_own_account() {
    let (db, _user, user_access, _binding) =
        crate::test_support::signed_test_node("health-oauth-account").await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let runtime_access =
        crate::inference::auth::credential::runtime_credential_access(&node, did).unwrap();
    let provider = crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER;
    let account = crate::inference::auth::credential::OAuthCredential {
        doc_id: None,
        credential_id: format!("{provider}:{did}:acct-b"),
        node_did: did.to_string(),
        provider: provider.to_string(),
        access_token: "access-TEST".into(),
        refresh_token: "refresh-TEST".into(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: Utc::now() + chrono::Duration::hours(8),
        last_refresh: Some(Utc::now()),
        enabled: true,
        account_ref: Some("acct-b".into()),
        connected_at: Some(Utc::now()),
        provider_account_key: None,
        label: None,
    };
    crate::inference::auth::credential::upsert_oauth_credential(&node, &account)
        .await
        .expect("seed acct-b");
    let (options, client, health_map) = (
        probe_options(),
        reqwest::Client::new(),
        BackendHealthMap::new(),
    );
    let mut grok = oauth_backend(
        crate::inference::backends::provider::BackendProviderKind::XaiGrokOAuth,
        "grok",
        "https://cli-chat-proxy.grok.com/v1",
    );
    grok.node_did = did.to_string();
    grok.auth = crate::documents::BackendAuth::NodeOAuth {
        account_ref: Some("acct-b".into()),
    };
    let models = ModelsListener::start();
    grok.endpoint = models.endpoint();
    oauth_backend_observation(&user_access, &runtime_access, &grok, "unknown").await;
    let outcome = probe_backends_cycle(
        &node,
        &client,
        std::slice::from_ref(&grok),
        Utc::now(),
        &health_map,
        &options,
        Some(OAuthProbeContext {
            node: node.clone(),
            node_did: did,
        }),
    )
    .await;
    let snap = health_map.get("grok").await.expect("entry");
    assert_eq!(
        outcome.promotable,
        vec!["grok".to_string()],
        "{:?}",
        snap.last_error
    );
    assert_eq!(snap.state, BackendHealthState::Healthy);
}

#[tokio::test]
async fn oauth_account_reference_without_its_row_is_not_probed() {
    let (db, _user, user_access, _binding) =
        crate::test_support::signed_test_node("health-oauth").await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let runtime_access =
        crate::inference::auth::credential::runtime_credential_access(&node, did).unwrap();
    seed_credential(
        &runtime_access,
        did,
        crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER,
        Utc::now() + chrono::Duration::hours(8),
    )
    .await;
    let (options, client, health_map) = (
        probe_options(),
        reqwest::Client::new(),
        BackendHealthMap::new(),
    );
    let mut grok = oauth_backend(
        crate::inference::backends::provider::BackendProviderKind::XaiGrokOAuth,
        "grok",
        "https://cli-chat-proxy.grok.com/v1",
    );
    grok.node_did = did.to_string();
    grok.auth = crate::documents::BackendAuth::NodeOAuth {
        account_ref: Some("acct-b".into()),
    };
    let models = ModelsListener::start();
    grok.endpoint = models.endpoint();
    oauth_backend_observation(&user_access, &runtime_access, &grok, "unknown").await;
    let outcome = probe_backends_cycle(
        &node,
        &client,
        std::slice::from_ref(&grok),
        Utc::now(),
        &health_map,
        &options,
        Some(OAuthProbeContext {
            node: node.clone(),
            node_did: did,
        }),
    )
    .await;
    assert!(outcome.promotable.is_empty());
    let error = health_map
        .get("grok")
        .await
        .and_then(|snap| snap.last_error)
        .unwrap_or_default();
    let missing = crate::inference::auth::credential::classify_oauth_auth_error(
        &crate::inference::auth::credential::XAI_OAUTH_PRODUCT,
        did,
        crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER,
        &crate::inference::auth::credential::OAuthAuthProblem::Missing,
    );
    assert!(error.contains(&missing), "{error}");
}

#[tokio::test]
async fn oauth_kinds_fresh_grok_credential_stays_healthy_and_promotes() {
    let (db, _user, user_access, _binding) =
        crate::test_support::signed_test_node("health-oauth").await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let runtime_access =
        crate::inference::auth::credential::runtime_credential_access(&node, did).unwrap();
    seed_credential(
        &runtime_access,
        did,
        crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER,
        Utc::now() + chrono::Duration::hours(8),
    )
    .await;
    let (options, client, health_map) = (
        probe_options(),
        reqwest::Client::new(),
        BackendHealthMap::new(),
    );
    let mut grok = oauth_backend(
        crate::inference::backends::provider::BackendProviderKind::XaiGrokOAuth,
        "grok",
        "https://cli-chat-proxy.grok.com/v1",
    );
    grok.node_did = did.to_string();
    let models = ModelsListener::start();
    grok.endpoint = models.endpoint();
    oauth_backend_observation(&user_access, &runtime_access, &grok, "unknown").await;
    for _ in 0..3 {
        let outcome = probe_backends_cycle(
            &node,
            &client,
            std::slice::from_ref(&grok),
            Utc::now(),
            &health_map,
            &options,
            Some(OAuthProbeContext {
                node: node.clone(),
                node_did: did,
            }),
        )
        .await;
        assert!(outcome.flipped.is_empty());
        assert_eq!(outcome.promotable, vec!["grok".to_string()]);
        let snap = health_map.get("grok").await.expect("entry");
        assert_eq!(snap.state, BackendHealthState::Healthy);
        assert_eq!(snap.failure_count, 0);
        assert!(snap.last_error.is_none(), "{:?}", snap.last_error);
    }
    assert!(health_map.vetoed_backend_ids().await.is_empty());
}

#[tokio::test]
async fn oauth_kinds_stale_credential_without_refresh_token_still_demotes_after_k() {
    let (db, _user, _user_access, _binding) =
        crate::test_support::signed_test_node("health-oauth").await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let runtime_access =
        crate::inference::auth::credential::runtime_credential_access(&node, did).unwrap();
    seed_credential_with_refresh_token(
        &runtime_access,
        did,
        crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER,
        Utc::now() - chrono::Duration::minutes(1),
        "",
    )
    .await;
    let (options, client, health_map) = (
        probe_options(),
        reqwest::Client::new(),
        BackendHealthMap::new(),
    );
    let grok = oauth_backend(
        crate::inference::backends::provider::BackendProviderKind::XaiGrokOAuth,
        "grok",
        "https://cli-chat-proxy.grok.com/v1",
    );
    for cycle in 1..=3u32 {
        let outcome = probe_backends_cycle(
            &node,
            &client,
            std::slice::from_ref(&grok),
            Utc::now(),
            &health_map,
            &options,
            Some(OAuthProbeContext {
                node: node.clone(),
                node_did: did,
            }),
        )
        .await;
        assert!(outcome.promotable.is_empty());
        let snap = health_map.get("grok").await.expect("entry");
        assert_eq!(snap.failure_count, cycle);
        let err = snap.last_error.clone().unwrap_or_default();
        assert!(err.contains("missing refresh_token"), "{err}");
        if cycle == 3 {
            assert_eq!(snap.state, BackendHealthState::Unhealthy);
        }
    }
}

struct TokenUrlOverrideGuard(&'static str);

impl Drop for TokenUrlOverrideGuard {
    fn drop(&mut self) {
        std::env::remove_var(self.0);
    }
}

#[tokio::test]
async fn oauth_kinds_refresh_an_expired_credential_on_probe_and_recover() {
    let (db, _user, user_access, _binding) =
        crate::test_support::signed_test_node("health-oauth").await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let runtime_access =
        crate::inference::auth::credential::runtime_credential_access(&node, did).unwrap();
    let provider = crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER;
    seed_credential_with_refresh_token(
        &runtime_access,
        did,
        provider,
        Utc::now() - chrono::Duration::minutes(1),
        "refresh-TEST",
    )
    .await;
    let (options, client, health_map) = (
        probe_options(),
        reqwest::Client::new(),
        BackendHealthMap::new(),
    );
    let mut claude = claude_backend();
    claude.node_did = did.to_string();
    let models = ModelsListener::start();
    claude.endpoint = models.endpoint();
    oauth_backend_observation(&user_access, &runtime_access, &claude, "unknown").await;

    // TOKEN_URL_ENV is process-global; serialize changes to its override.
    let _env = TOKEN_URL_ENV.lock().await;
    let _guard =
        TokenUrlOverrideGuard(crate::inference::auth::claude::CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV);
    let (url, server) = one_shot_token_server(
        200,
        r#"{"access_token":"access-REFRESHED","refresh_token":"refresh-ROTATED","expires_in":28800}"#,
    )
    .await;
    std::env::set_var(
        crate::inference::auth::claude::CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV,
        &url,
    );
    let outcome = probe_backends_cycle(
        &node,
        &client,
        std::slice::from_ref(&claude),
        Utc::now(),
        &health_map,
        &options,
        Some(OAuthProbeContext {
            node: node.clone(),
            node_did: did,
        }),
    )
    .await;
    assert_eq!(
        outcome.promotable,
        vec!["claude".to_string()],
        "a probe whose credential owner refreshed the token must report the backend healthy"
    );
    let snap = health_map.get("claude").await.expect("entry");
    assert_eq!(snap.state, BackendHealthState::Healthy);
    assert_eq!(snap.failure_count, 0);
    assert!(snap.last_error.is_none(), "{:?}", snap.last_error);
    assert_eq!(
        models.last_authorization().as_deref(),
        Some("Bearer access-REFRESHED"),
        "discovery must send the token the credential owner resolved, not the stale row"
    );
    let refresh_request = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the probe refreshes through the token endpoint")
        .expect("token server task");
    assert!(
        refresh_request.contains("refresh-TEST"),
        "the refresh must present the stored refresh token: {refresh_request}"
    );
    let stored = crate::inference::auth::credential::lookup_oauth_credential_by_id(
        &node,
        &crate::inference::auth::credential::oauth_credential_id(did, provider),
    )
    .await
    .expect("lookup stored credential")
    .expect("stored credential row");
    assert_eq!(
        stored.access_token, "access-REFRESHED",
        "the credential owner must persist the refreshed access token"
    );
    assert_eq!(
        stored.refresh_token, "refresh-ROTATED",
        "the credential owner must persist the rotated refresh token"
    );
    models.shutdown().await;
}

#[tokio::test]
async fn a_hung_token_endpoint_does_not_stall_the_next_backend_probe() {
    let (db, _user, user_access, _binding) =
        crate::test_support::signed_test_node("health-oauth").await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let runtime_access =
        crate::inference::auth::credential::runtime_credential_access(&node, did).unwrap();
    let provider = crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER;
    seed_credential_with_refresh_token(
        &runtime_access,
        did,
        provider,
        Utc::now() - chrono::Duration::minutes(1),
        "refresh-TEST",
    )
    .await;
    let (options, client, health_map) = (
        probe_options(),
        reqwest::Client::new(),
        BackendHealthMap::new(),
    );
    let models = ModelsListener::start();
    let mut claude = claude_backend();
    claude.node_did = did.to_string();
    claude.endpoint = models.endpoint();
    let mut next = backend("next", models.endpoint());
    next.node_did = did.to_string();
    oauth_backend_observation(&user_access, &runtime_access, &claude, "unknown").await;
    oauth_backend_observation(&user_access, &runtime_access, &next, "unknown").await;

    let _env = TOKEN_URL_ENV.lock().await;
    let _guard =
        TokenUrlOverrideGuard(crate::inference::auth::claude::CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV);
    let (received_tx, _received) = oneshot::channel();
    let (_never_release, release_rx) = oneshot::channel();
    let (url, _server) = crate::inference::auth::credential::test_support::gated_token_server(
        200,
        r#"{"access_token":"access-REFRESHED","expires_in":28800}"#,
        Some((received_tx, release_rx)),
    )
    .await;
    std::env::set_var(
        crate::inference::auth::claude::CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV,
        &url,
    );
    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        probe_backends_cycle(
            &node,
            &client,
            &[claude.clone(), next.clone()],
            Utc::now(),
            &health_map,
            &options,
            Some(OAuthProbeContext {
                node: node.clone(),
                node_did: did,
            }),
        ),
    )
    .await
    .expect("a token endpoint that never answers must not stall the probe cycle");
    assert_eq!(outcome.promotable, vec!["next".to_string()]);
    let claude_health = health_map.get("claude").await.expect("claude entry");
    assert_eq!(claude_health.failure_count, 1);
    assert!(
        claude_health
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("timed out")),
        "{:?}",
        claude_health.last_error
    );
    assert_eq!(
        health_map.get("next").await.expect("next entry").state,
        BackendHealthState::Healthy
    );
    models.shutdown().await;
}

#[tokio::test]
async fn oauth_kinds_missing_credential_fails_with_login_hint() {
    let node = test_node().await;
    let (options, client, health_map) = (
        probe_options(),
        reqwest::Client::new(),
        BackendHealthMap::new(),
    );
    let codex = oauth_backend(
        crate::inference::backends::provider::BackendProviderKind::ChatGptCodex,
        "codex",
        "https://chatgpt.com/backend-api/codex",
    );
    probe_backends_cycle(
        &node,
        &client,
        std::slice::from_ref(&codex),
        Utc::now(),
        &health_map,
        &options,
        Some(OAuthProbeContext {
            node: node.clone(),
            node_did: "did:key:z6MkNobody",
        }),
    )
    .await;
    let snap = health_map.get("codex").await.expect("entry");
    assert_eq!(snap.state, BackendHealthState::CredentialsRequired);
    assert!(snap
        .last_error
        .clone()
        .unwrap_or_default()
        .contains("gents codex-login --node-did did:key:z6MkNobody"));
}

#[tokio::test]
async fn subscription_probe_preserves_typed_refresh_rejections() {
    let models = ModelsListener::start();
    let _env = TOKEN_URL_ENV.lock().await;
    for (provider, override_env, kind, forbidden_event) in [
        (
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            crate::inference::auth::claude::CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV,
            crate::inference::backends::provider::BackendProviderKind::ClaudeCliSubscription,
            "subscriptionNotEntitled",
        ),
        (
            crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
            gents_protocol::chatgpt_oauth::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
            crate::inference::backends::provider::BackendProviderKind::ChatGptCodex,
            "probeFail",
        ),
    ] {
        let _guard = TokenUrlOverrideGuard(override_env);
        for (status, body, event) in [
            (401, r#"{"error":"invalid_grant"}"#, "credentialsRejected"),
            (400, r#"{"error":"invalid_grant"}"#, "credentialsRejected"),
            (403, r#"{"error":"forbidden"}"#, forbidden_event),
            (500, r#"{"error":"unavailable"}"#, "probeFail"),
        ] {
            let (db, _user, _user_access, _binding) =
                crate::test_support::signed_test_node("health-rejection").await;
            let node = db.node.clone();
            let did = db.node_identity.did().to_owned();
            let runtime_access =
                crate::inference::auth::credential::runtime_credential_access(&node, &did).unwrap();
            seed_credential(
                &runtime_access,
                &did,
                provider,
                Utc::now() - chrono::Duration::minutes(1),
            )
            .await;
            let mut backend = oauth_backend(kind, provider, &models.endpoint());
            backend.node_did = did.clone();
            let (url, server) = one_shot_token_server(status, body).await;
            std::env::set_var(override_env, url);
            let health = BackendHealthMap::new();
            let outcome = probe_backends_cycle(
                &node,
                &reqwest::Client::new(),
                &[backend],
                Utc::now(),
                &health,
                &probe_options(),
                Some(OAuthProbeContext {
                    node: node.clone(),
                    node_did: &did,
                }),
            )
            .await;
            server.await.unwrap();
            let snapshot = health.get(provider).await.unwrap();
            assert_modeled_probe(None, &snapshot, event);
            assert_eq!(!outcome.flipped.is_empty(), snapshot.state.blocks_routing());
            assert_eq!(
                models.requests(),
                0,
                "a failed credential resolution must not call discovery"
            );
            let error = snapshot.last_error.as_deref().unwrap();
            assert!(!error.contains("refresh-TEST") && !error.contains("access-TEST"));
            node.shutdown().await.expect("node shutdown");
        }
    }
    models.shutdown().await;
}

#[tokio::test]
async fn subscription_probe_account_changes_retire_previous_measurement() {
    let node = test_node().await;
    let mut backend = claude_backend();
    backend.auth = crate::documents::BackendAuth::NodeOAuth {
        account_ref: Some("account-one".into()),
    };
    let health = BackendHealthMap::new();
    let options = probe_options();
    let mut outcome = ProbeCycleOutcome::default();
    record_probe_event(
        &node,
        &backend,
        ProbeEvent::CredentialsRejected,
        None,
        ProbeEnv {
            now: Utc::now(),
            health_map: &health,
            options: &options,
            cancel: None,
        },
        &mut outcome,
    )
    .await;
    let rejected = health.get("claude").await.unwrap();
    assert_modeled_probe(None, &rejected, "credentialsRejected");
    assert!(rejected.state_for(&backend).unwrap().blocks_routing());

    backend.auth = crate::documents::BackendAuth::NodeOAuth {
        account_ref: Some("account-two".into()),
    };
    assert_eq!(
        rejected.state_for(&backend),
        None,
        "another account must not inherit the rejected account's measurement"
    );
    record_probe_event(
        &node,
        &backend,
        ProbeEvent::ProbeFail,
        Some("network unavailable".into()),
        ProbeEnv {
            now: Utc::now(),
            health_map: &health,
            options: &options,
            cancel: None,
        },
        &mut outcome,
    )
    .await;
    let changed = health.get("claude").await.unwrap();
    assert_modeled_probe(None, &changed, "probeFail");
    assert_eq!(
        changed.state_for(&backend),
        Some(BackendHealthState::Degraded)
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn oauth_kinds_are_skipped_without_a_probe_context() {
    let (options, client, health_map) = (
        probe_options(),
        reqwest::Client::new(),
        BackendHealthMap::new(),
    );
    let node = test_node().await;
    let outcome = probe_backends_cycle(
        &node,
        &client,
        std::slice::from_ref(&claude_backend()),
        Utc::now(),
        &health_map,
        &options,
        None,
    )
    .await;
    assert!(outcome.promotable.is_empty());
    assert!(
        health_map.get("claude").await.is_none(),
        "no measurement without a node"
    );
}

/// A Claude gateway signed in with `gents claude-gateway-login` carries node
/// OAuth credentials, so the probe must select its stored account and map it to
/// the gateway refresh kind and product rather than the subscription's.
#[tokio::test]
async fn a_gateway_node_oauth_backend_probes_its_stored_gateway_account() {
    let (db, _user, user_access, _binding) =
        crate::test_support::signed_test_node("health-oauth").await;
    let node = db.node.clone();
    let did = db.node_identity.did();
    let runtime_access =
        crate::inference::auth::credential::runtime_credential_access(&node, did).unwrap();
    seed_credential(
        &runtime_access,
        did,
        crate::claude_gateway_login::CLAUDE_GATEWAY_PROVIDER,
        Utc::now() + chrono::Duration::hours(8),
    )
    .await;
    let (options, client, health_map) = (
        probe_options(),
        reqwest::Client::new(),
        BackendHealthMap::new(),
    );
    let mut gateway = oauth_backend(
        crate::inference::backends::provider::BackendProviderKind::ClaudeGateway,
        "gateway",
        "https://gw.example",
    );
    gateway.node_did = did.to_string();
    let models = ModelsListener::start();
    gateway.endpoint = models.endpoint();
    oauth_backend_observation(&user_access, &runtime_access, &gateway, "unknown").await;
    let outcome = probe_backends_cycle(
        &node,
        &client,
        std::slice::from_ref(&gateway),
        Utc::now(),
        &health_map,
        &options,
        Some(OAuthProbeContext {
            node: node.clone(),
            node_did: did,
        }),
    )
    .await;
    let snap = health_map.get("gateway").await.expect("entry");
    assert_eq!(
        outcome.promotable,
        vec!["gateway".to_string()],
        "{:?}",
        snap.last_error
    );
    assert_eq!(snap.state, BackendHealthState::Healthy);
    assert!(snap.last_error.is_none(), "{:?}", snap.last_error);
}

/// The gateway kind maps to the gateway refresh kind and product, so a probe
/// never falls through the helper's `unreachable!` arm.
#[test]
fn a_gateway_backend_maps_to_the_gateway_refresh_kind_and_product() {
    use crate::inference::backends::health::oauth::{oauth_product, oauth_refresh_kind};
    use crate::inference::backends::provider::BackendProviderKind;
    assert_eq!(
        oauth_refresh_kind(BackendProviderKind::ClaudeGateway),
        crate::inference::auth::credential::OAuthRefreshKind::Gateway
    );
    assert_eq!(
        oauth_product(BackendProviderKind::ClaudeGateway),
        crate::claude_gateway_login::CLAUDE_GATEWAY_PRODUCT
    );
}
