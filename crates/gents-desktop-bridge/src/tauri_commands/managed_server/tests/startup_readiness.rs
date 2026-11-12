use super::*;

#[tokio::test]
async fn managed_readiness_reads_status_as_the_user_and_never_the_node_key() {
    use identity::Identity as _;
    use wiremock::matchers::{header_regex, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("node");
    let root = temp.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    let key_path = home.join("node.key");
    let identity = gents::store::identity::load_or_create_file_identity(&key_path).unwrap();
    let did = identity.did().unwrap().to_string();
    assert!(!gents::store::identity::can_mint_defradb_bearer(&did));
    let server = MockServer::start().await;
    let endpoint = reqwest::Url::parse(&server.uri()).unwrap();
    let config = gents_server::server_host::ServerConfig {
        home: home.clone(),
        http_addr: "127.0.0.1".parse().unwrap(),
        http_port: endpoint.port().unwrap(),
    };
    let graphql = format!("{}/api/v0/graphql", server.uri());
    std::fs::write(
        home.join("init.json"),
        serde_json::json!({
            "node_name":"Local", "node_did":did, "key_path":key_path,
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        home.join("runtime.json"),
        serde_json::json!({
            "node_name":"Local", "node_did":did, "graphql":graphql,
        })
        .to_string(),
    )
    .unwrap();
    let public = serde_json::json!({
        "service":"gents", "version":env!("CARGO_PKG_VERSION"),
        "lifecycle":"ready", "node_did":did, "graphql":graphql,
    });
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(public.clone()))
        .with_priority(2)
        .mount(&server)
        .await;
    let mut private = public;
    private["tool_ceiling"] = "readwrite".into();
    private["tool_root"] = root.to_string_lossy().into_owned().into();
    Mock::given(method("GET"))
        .and(path("/status"))
        .and(header_regex("authorization", "^Bearer .+"))
        .respond_with(ResponseTemplate::new(200).set_body_json(private))
        .with_priority(1)
        .mount(&server)
        .await;
    let authority = EffectiveManagedAuthority {
        tool_ceiling: ManagedServerToolCeiling::Readwrite,
        tool_root: Some(root),
    };
    let unsigned = observe_port_readiness_at(&config, None).await.unwrap();
    let PortReadiness::StatusUnreadable { public, cause } = unsigned else {
        panic!("without a User signer the private status is unread, not anonymous")
    };
    assert!(matches!(
        cause,
        gents_desktop_core::local_runtime::HostedRuntimeStatusError::NoUserSigner
    ));
    assert!(validate_ready_runtime(&public, &authority, &home).is_err());
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|request| !request.headers.contains_key("authorization")));
    let error = status_unreadable_error(&cause);
    assert_eq!(error.code, BridgeErrorCode::ClientNotRunning);

    let user = gents::KeyIdentity::load_or_create(temp.path().join("user.key"), None).unwrap();
    let user = user.client_signing_capability().unwrap();
    let signed = observe_port_readiness_at(&config, Some(&user))
        .await
        .unwrap();
    let PortReadiness::Ready(signed) = signed else {
        panic!("the User reads the runtime's private status")
    };
    validate_ready_runtime(&signed, &authority, &home).unwrap();
    let wrong_root = EffectiveManagedAuthority {
        tool_root: Some(temp.path().join("elsewhere")),
        ..authority.clone()
    };
    assert!(validate_ready_runtime(&signed, &wrong_root, &home).is_err());
    std::fs::remove_file(&key_path).unwrap();
    let PortReadiness::Ready(_) = observe_port_readiness_at(&config, Some(&user))
        .await
        .unwrap()
    else {
        panic!("the status read never depended on the node key")
    };
    assert!(!gents::store::identity::can_mint_defradb_bearer(&did));
    let count = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.headers.contains_key("authorization"))
        .count();
    std::fs::write(home.join("init.json"), r#"{"node_did":"did:key:foreign"}"#).unwrap();
    assert!(matches!(
        observe_port_readiness_at(&config, Some(&user))
            .await
            .unwrap(),
        PortReadiness::Foreign(_)
    ));
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.headers.contains_key("authorization"))
            .count(),
        count
    );
}

#[tokio::test]
async fn generated_fresh_startup_preserves_enrollment_authority() {
    use gents_desktop_core::client::{ClientCoreOptions, DesktopPaths};

    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let case = snapshot["enrollment_startup_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "fresh_startup")
        .unwrap();
    let (temp, state) = orchestration_state();
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(temp.path().join("client")),
            ClientCoreOptions::local_only(),
        )
        .await
        .unwrap(),
    );
    let access = gents::ConfigAccess::local_as(
        core.node_arc(),
        ::identity::Did::new(core.node_identity().did()).unwrap(),
    );
    let query = "{ NetworkEnrollmentRequest { _docID } NetworkEnrollmentDecision { _docID } NetworkAuthorizationRevision { _docID } }";
    let before = access.execute(query).await.unwrap();
    for name in [
        "NetworkEnrollmentRequest",
        "NetworkEnrollmentDecision",
        "NetworkAuthorizationRevision",
    ] {
        assert!(
            before["data"][name].as_array().unwrap().is_empty(),
            "{before}"
        );
    }
    start_running_managed_pairing(&state, Arc::clone(&core)).await;
    let after = access.execute(query).await.unwrap();
    assert_eq!(
        before["data"] == after["data"],
        case["expected_authority_unchanged"].as_bool().unwrap(),
    );
    drain_managed_runtime_pairing(&state).await;
    core.shutdown().await.unwrap();
}

/// Issue #3645: a relaunch after the desktop's own enrollment into its
/// co-hosted Node completed still reads the private status, because the
/// reader is the key provisioning made that home's native admin. The enrolled
/// desktop key holds the Node document's `administrator` relation and direct
/// node permissions, which a private read does not admit, so it is served the
/// public body, whose absent ceiling and root fail readiness.
#[tokio::test]
async fn managed_readiness_after_desktop_enrollment_reads_the_private_status() {
    use gents::store::identity::NodeIdentity as _;
    use gents_desktop_core::client::{DesktopPaths, PeerRecord, PrincipalIdentity};
    use identity::Identity as _;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let (temp, state) = orchestration_state();
    let home = temp.path().join("agent");
    let root = temp.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&root).unwrap();

    let desktop: DesktopPaths = state.policy.desktop_paths.clone();
    desktop.ensure_root_dirs().await.unwrap();
    let cli = desktop.cli_client();
    cli.ensure_root_dirs().await.unwrap();
    let admin = PrincipalIdentity::load_or_create(&cli).await.unwrap();
    let enrolled_key = PrincipalIdentity::load_or_create(&desktop).await.unwrap();
    assert_ne!(admin.did(), enrolled_key.did());

    let server = MockServer::start().await;
    let endpoint = reqwest::Url::parse(&server.uri()).unwrap();
    let config = gents_server::server_host::ServerConfig {
        home: home.clone(),
        http_addr: "127.0.0.1".parse().unwrap(),
        http_port: endpoint.port().unwrap(),
    };
    let did = "did:key:z6MkfLocalCoHostedNode".to_string();
    let graphql = format!("{}/api/v0/graphql", server.uri());
    std::fs::write(
        home.join("init.json"),
        serde_json::json!({ "node_name":"Local", "node_did":did }).to_string(),
    )
    .unwrap();

    let mut enrolled = PeerRecord::local_standard("Local", "", &did, &graphql);
    enrolled.source = Some("enrollment".into());
    enrolled.pairing_network_id = Some("network".into());
    enrolled.enrollment_request_digest = Some("digest".into());
    enrolled.enrollment_authorization_sequence = Some(1);
    assert!(enrolled.records_completed_enrollment_into(&did));
    std::fs::write(
        desktop.peer_directory_path(),
        serde_json::json!({ "peers": [enrolled] }).to_string(),
    )
    .unwrap();

    let public = serde_json::json!({
        "service":"gents", "version":env!("CARGO_PKG_VERSION"),
        "lifecycle":"ready", "node_did":did, "graphql":graphql,
    });
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(public.clone()))
        .with_priority(2)
        .mount(&server)
        .await;
    let mut private = public;
    private["tool_ceiling"] = "readwrite".into();
    private["tool_root"] = root.to_string_lossy().into_owned().into();
    let admin_did = admin.did().to_owned();
    Mock::given(method("GET"))
        .and(path("/status"))
        .and(move |request: &wiremock::Request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
                .and_then(|token| identity::from_token(token.as_bytes()).ok())
                .and_then(|caller| caller.did().ok())
                .is_some_and(|caller| caller.to_string() == admin_did)
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(private))
        .with_priority(1)
        .mount(&server)
        .await;

    let authority = EffectiveManagedAuthority {
        tool_ceiling: ManagedServerToolCeiling::Readwrite,
        tool_root: Some(root.clone()),
    };
    let user = user_status_signer(&state, &home)
        .await
        .expect("the provisioned admin signs the relaunch read");
    assert_eq!(user.did(), admin.did());
    let PortReadiness::Ready(status) = observe_port_readiness_at(&config, Some(&user))
        .await
        .unwrap()
    else {
        panic!("the home's admin reads the private status after enrollment")
    };
    assert_eq!(
        status.effective_tool_ceiling,
        Some(ManagedServerToolCeiling::Readwrite)
    );
    assert_eq!(
        status.effective_tool_root.as_deref(),
        Some(root.to_string_lossy().as_ref())
    );
    validate_ready_runtime(&status, &authority, &home).unwrap();

    let enrolled_capability = enrolled_key.client_signing_capability().unwrap();
    let PortReadiness::Ready(unauthorized) =
        observe_port_readiness_at(&config, Some(&enrolled_capability))
            .await
            .unwrap()
    else {
        panic!("a refused private read answers with the public body, not a refusal")
    };
    assert_eq!(unauthorized.effective_tool_ceiling, None);
    assert_eq!(unauthorized.effective_tool_root, None);
    assert!(validate_ready_runtime(&unauthorized, &authority, &home).is_err());
}
