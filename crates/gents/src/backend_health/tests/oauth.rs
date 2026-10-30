use super::*;

fn claude_backend() -> InferenceBackend {
    let mut claude = backend(
        "claude",
        crate::claude_subscription::DEFAULT_BACKEND_ENDPOINT.to_string(),
    );
    claude.provider_kind = crate::backend_provider::BackendProviderKind::ClaudeCliSubscription;
    claude.auth = crate::document_config::BackendAuth::NodeOAuth { account_ref: None };
    claude
}

fn oauth_backend(
    kind: crate::backend_provider::BackendProviderKind,
    id: &str,
    endpoint: &str,
) -> InferenceBackend {
    let mut backend = backend(id, endpoint.to_string());
    backend.provider_kind = kind;
    backend.auth = crate::document_config::BackendAuth::NodeOAuth { account_ref: None };
    backend
}

#[tokio::test]
async fn oauth_kinds_probe_the_credential_document_fresh_is_healthy_and_promotes() {
    let node = test_node().await;
    let did = "did:key:z6MkProbe";
    seed_credential(
        &node,
        did,
        crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
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
    seed_backend_observation(&node, &claude, "unknown").await;
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

#[tokio::test]
async fn oauth_account_reference_is_probed_with_its_own_account() {
    let node = test_node().await;
    let did = "did:key:z6MkProbeAccount";
    let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
    let account = crate::oauth_credential::OAuthCredential {
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
    crate::oauth_credential::upsert_oauth_credential(&node, &account)
        .await
        .expect("seed acct-b");
    let (options, client, health_map) = (
        probe_options(),
        reqwest::Client::new(),
        BackendHealthMap::new(),
    );
    let mut grok = oauth_backend(
        crate::backend_provider::BackendProviderKind::XaiGrokOAuth,
        "grok",
        "https://cli-chat-proxy.grok.com/v1",
    );
    grok.node_did = did.to_string();
    grok.auth = crate::document_config::BackendAuth::NodeOAuth {
        account_ref: Some("acct-b".into()),
    };
    let models = ModelsListener::start();
    grok.endpoint = models.endpoint();
    seed_backend_observation(&node, &grok, "unknown").await;
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
    let node = test_node().await;
    let did = "did:key:z6MkProbe";
    seed_credential(
        &node,
        did,
        crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
        Utc::now() + chrono::Duration::hours(8),
    )
    .await;
    let (options, client, health_map) = (
        probe_options(),
        reqwest::Client::new(),
        BackendHealthMap::new(),
    );
    let mut grok = oauth_backend(
        crate::backend_provider::BackendProviderKind::XaiGrokOAuth,
        "grok",
        "https://cli-chat-proxy.grok.com/v1",
    );
    grok.node_did = did.to_string();
    grok.auth = crate::document_config::BackendAuth::NodeOAuth {
        account_ref: Some("acct-b".into()),
    };
    let models = ModelsListener::start();
    grok.endpoint = models.endpoint();
    seed_backend_observation(&node, &grok, "unknown").await;
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
    let missing = crate::oauth_credential::classify_oauth_auth_error(
        &crate::oauth_credential::XAI_OAUTH_PRODUCT,
        did,
        crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
        &crate::oauth_credential::OAuthAuthProblem::Missing,
    );
    assert!(error.contains(&missing), "{error}");
}

#[tokio::test]
async fn oauth_kinds_fresh_grok_credential_stays_healthy_and_promotes() {
    let node = test_node().await;
    let did = "did:key:z6MkProbe";
    seed_credential(
        &node,
        did,
        crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
        Utc::now() + chrono::Duration::hours(8),
    )
    .await;
    let (options, client, health_map) = (
        probe_options(),
        reqwest::Client::new(),
        BackendHealthMap::new(),
    );
    let mut grok = oauth_backend(
        crate::backend_provider::BackendProviderKind::XaiGrokOAuth,
        "grok",
        "https://cli-chat-proxy.grok.com/v1",
    );
    grok.node_did = did.to_string();
    let models = ModelsListener::start();
    grok.endpoint = models.endpoint();
    seed_backend_observation(&node, &grok, "unknown").await;
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
    let node = test_node().await;
    let did = "did:key:z6MkProbe";
    seed_credential_with_refresh_token(
        &node,
        did,
        crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
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
        crate::backend_provider::BackendProviderKind::XaiGrokOAuth,
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
    let node = test_node().await;
    let did = "did:key:z6MkProbeExpired";
    let provider = crate::claude_oauth::CLAUDE_OAUTH_PROVIDER;
    seed_credential_with_refresh_token(
        &node,
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
    seed_backend_observation(&node, &claude, "unknown").await;

    // TOKEN_URL_ENV is process-global; serialize changes to its override.
    let _env = TOKEN_URL_ENV.lock().await;
    let _guard = TokenUrlOverrideGuard(crate::claude_oauth::CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV);
    let (url, server) = one_shot_token_server(
        200,
        r#"{"access_token":"access-REFRESHED","refresh_token":"refresh-ROTATED","expires_in":28800}"#,
    )
    .await;
    std::env::set_var(
        crate::claude_oauth::CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV,
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
    let stored = crate::oauth_credential::lookup_oauth_credential_by_id(
        &node,
        &crate::oauth_credential::oauth_credential_id(did, provider),
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
    let node = test_node().await;
    let did = "did:key:z6MkProbeHungRefresh";
    let provider = crate::claude_oauth::CLAUDE_OAUTH_PROVIDER;
    seed_credential_with_refresh_token(
        &node,
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
    seed_backend_observation(&node, &claude, "unknown").await;
    seed_backend_observation(&node, &next, "unknown").await;

    let _env = TOKEN_URL_ENV.lock().await;
    let _guard = TokenUrlOverrideGuard(crate::claude_oauth::CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV);
    let (received_tx, _received) = oneshot::channel();
    let (_never_release, release_rx) = oneshot::channel();
    let (url, _server) = crate::oauth_credential::test_support::gated_token_server(
        200,
        r#"{"access_token":"access-REFRESHED","expires_in":28800}"#,
        Some((received_tx, release_rx)),
    )
    .await;
    std::env::set_var(
        crate::claude_oauth::CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV,
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
        crate::backend_provider::BackendProviderKind::ChatGptCodex,
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
    let node = test_node().await;
    let models = ModelsListener::start();
    let _env = TOKEN_URL_ENV.lock().await;
    for (provider, override_env, kind, forbidden_event) in [
        (
            crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
            crate::claude_oauth::CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV,
            crate::backend_provider::BackendProviderKind::ClaudeCliSubscription,
            "subscriptionNotEntitled",
        ),
        (
            crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
            gents_protocol::chatgpt_oauth::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
            crate::backend_provider::BackendProviderKind::ChatGptCodex,
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
            let did = format!("did:key:z6MkRejectedRefresh{provider}{status}");
            seed_credential(
                &node,
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
        }
    }
    models.shutdown().await;
    node.shutdown().await;
}

#[tokio::test]
async fn subscription_probe_account_changes_retire_previous_measurement() {
    let node = test_node().await;
    let mut backend = claude_backend();
    backend.auth = crate::document_config::BackendAuth::NodeOAuth {
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

    backend.auth = crate::document_config::BackendAuth::NodeOAuth {
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
    node.shutdown().await;
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
