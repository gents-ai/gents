#[tokio::test]
async fn generated_hydration_routes_require_current_enrollment_materialization() -> Result<()> {
    use crate::agent::p2p_reconcile::enrollment_reconcile::test_enrollment_authority;
    use crate::config_client::ConfigAccess;
    use crate::identity::KeyIdentity;
    use crate::tool_call_lifecycle::admission_fixture::ensure_fixture_session;
    use serde_json::Value;

    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>).await??;
    let cases = contracts["enrollment_hydration_route_cases"]
        .as_array()
        .context("generated enrollment hydration route cases")?;
    assert_eq!(cases.len(), 7);
    let current = &cases[0]["authority"];
    let text = |field: &str| current[field].as_str().unwrap().to_owned();
    let fence = EnrollmentAuthorizationFence {
        network_id: text("network_id"),
        request_id: text("request_id"),
        admin_did: text("admin_did"),
        member_did: text("member_did"),
        member_peer: text("member_peer"),
        member_ticket: text("member_ticket"),
        owner_node: text("owner_node"),
        profile: text("profile"),
        request_digest: text("request_digest"),
        authorization_sequence: current["authorization_sequence"].as_u64().unwrap(),
        authorization_expires_at: text("authorization_expires_at"),
    };
    let (directory, identity, builder) = tokio::task::spawn_blocking(|| -> Result<_> {
        let directory = tempfile::tempdir()?;
        let identity: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(
            directory.path().join("identity.key"),
            None,
        )?);
        Ok((directory, identity, crate::test_support::node_builder()))
    })
    .await??;
    let node = Arc::new(builder.build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    ensure_fixture_session(
        &node,
        "session-1",
        &fence.owner_node,
        Some(&fence.member_did),
        "2026-10-28T00:00:00Z",
    )
    .await;
    let request = HydrationRequest {
        request_key: "model-generation".into(),
        peer_id: fence.member_peer.clone(),
        requester_did: fence.member_did.clone(),
        node_did: fence.owner_node.clone(),
        session_id: "session-1".into(),
    };
    let collections = [
        "SessionHydrationRequest",
        "FileImageHydrationRequest",
        "FileInventoryRequest",
    ];
    let filters = collections
        .map(|collection| {
            (
                collection.to_owned(),
                combine_filters(
                    equality_filter("requester_did", &fence.member_did),
                    equality_filter("node_did", &fence.owner_node),
                ),
            )
        })
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let filter = escape_graphql_string(&serde_json::to_string(&filters)?);
    let peer = escape_graphql_string(&fence.member_peer);
    ConfigAccess::write_local(&node, "test.hydration.applied_generation", &format!(
        r#"mutation {{ create_PeerPairingApplied(input: {{ peer_id: "{peer}", replicator_filter: "{filter}" }}) {{ _docID }} }}"#,
    )).await?;
    let (_control, authority) = test_enrollment_authority(Some(fence));
    let store = GraphqlHydrationStore {
        node: node.clone(),
        identity,
        enrollment: authority,
    };
    let literal = |value: &Value| match value {
        Value::String(value) => format!("\"{}\"", escape_graphql_string(value)),
        Value::Null => "null".into(),
        Value::Number(value) => value.as_i64().unwrap().to_string(),
        _ => panic!("unexpected modeled route field"),
    };
    for case in cases {
        let row = &case["desired"];
        let fields = [
            "peer_id",
            "node_did",
            "enrollment_request_digest",
            "enrollment_authorization_sequence",
            "enrollment_authorization_expires_at",
        ]
        .map(|field| format!("{field}: {}", literal(&row[field])))
        .join(", ");
        ConfigAccess::write_local(&node, "test.hydration.desired_generation", &format!(
            r#"mutation {{ upsert_PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer}" }} }}, add: {{ source: "enrollment", {fields} }}, update: {{ source: "enrollment", {fields} }}) {{ _docID }} }}"#,
        )).await?;
        let applied_fields = [
            "enrollment_request_digest",
            "enrollment_authorization_sequence",
            "enrollment_authorization_expires_at",
        ]
        .map(|field| format!("{field}: {}", literal(&case["applied"][field])))
        .join(", ");
        ConfigAccess::write_local(&node, "test.hydration.applied_generation", &format!(
            r#"mutation {{ update_PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer}" }} }}, input: {{ {applied_fields} }}) {{ _docID }} }}"#,
        )).await?;
        for collection in collections {
            let admission = store
                .load_admission_for_collection(&request, collection)
                .await?;
            assert_eq!(
                matches!(
                    decide_hydration(&request, &admission.catalog),
                    HydrationVerdict::Admit(_)
                ),
                case["expected_admitted"].as_bool().unwrap(),
                "{} {collection}",
                case["name"]
            );
        }
    }
    node.shutdown().await;
    drop(directory);
    Ok(())
}
