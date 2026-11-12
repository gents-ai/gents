use gents_documents::*;

#[tokio::test]
async fn tools_document_round_trips_defra_query_fields() {
    let (db, _user, access, _binding) = crate::test_support::root::signed_test_node(
        "tools_document_round_trips_defra_query_fields",
    )
    .await;
    let node_did = db.node_identity.did();

    let doc = Tools {
        tools_id: "amy-general-tools".to_string(),
        node_did: node_did.to_string(),
        built_ins: Some(BuiltInTools {
            enable_session_history_tool: Some(true),
            ..Default::default()
        }),
        datastore: Some(DatastoreTools {
            enable_defra_query: Some(false),
            defra_query_collections: Some(vec![
                "AgentRequest".to_string(),
                "AgentResponse".to_string(),
            ]),
            ..Default::default()
        }),
        ..Default::default()
    };
    write_tools_document(
        &access,
        &doc,
        &gents_documents::test_support::AdmitAllProfiles,
    )
    .await
    .expect("write should persist the defra_query fields");

    let loaded: Tools = access
        .transact_write("test.tools.read", |txn| {
            let doc = &doc;
            Box::pin(async move {
                gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Tools,
                    &doc.node_did,
                    &doc.tools_id,
                )
                .await?
                .map(|(_, value)| {
                    serde_json::from_value::<Tools>(value).map_err(anyhow::Error::from)
                })
                .transpose()
            })
        })
        .await
        .expect("read should succeed")
        .expect("tools should exist");
    assert_eq!(
        loaded
            .built_ins
            .as_ref()
            .unwrap()
            .enable_session_history_tool,
        Some(true)
    );
    assert_eq!(
        loaded.datastore.as_ref().unwrap().enable_defra_query,
        Some(false)
    );
    assert_eq!(
        loaded.datastore.as_ref().unwrap().defra_query_collections,
        Some(vec![
            "AgentRequest".to_string(),
            "AgentResponse".to_string()
        ])
    );
}

#[tokio::test]
async fn tools_update_can_clear_lsp_config() {
    let (db, _user, access, _binding) =
        crate::test_support::root::signed_test_node("tools_update_can_clear_lsp_config").await;
    let node_did = db.node_identity.did();

    let lsp = |config: Option<String>| Tools {
        tools_id: "lsp-config-clear".to_string(),
        node_did: node_did.to_string(),
        integrations: Some(IntegrationTools {
            lsp: config.map(|config| LspTools {
                config: Some(config),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    let doc = lsp(Some(r#"{"idle_timeout_ms":1000}"#.to_string()));
    write_tools_document(
        &access,
        &doc,
        &gents_documents::test_support::AdmitAllProfiles,
    )
    .await
    .unwrap();
    async fn read(
        access: &gents_store::config_client::ConfigAccess,
        node_did: &str,
    ) -> anyhow::Result<Option<Tools>> {
        access
            .transact_write("test.tools.read", |txn| {
                Box::pin(async move {
                    gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Tools,
                        node_did,
                        "lsp-config-clear",
                    )
                    .await?
                    .map(|(_, value)| {
                        serde_json::from_value::<Tools>(value).map_err(anyhow::Error::from)
                    })
                    .transpose()
                })
            })
            .await
    }
    let loaded: Tools = read(&access, node_did).await.unwrap().unwrap();
    assert_eq!(
        loaded
            .integrations
            .as_ref()
            .unwrap()
            .lsp
            .as_ref()
            .unwrap()
            .config,
        Some(r#"{"idle_timeout_ms":1000}"#.to_string())
    );

    write_tools_document(
        &access,
        &lsp(None),
        &gents_documents::test_support::AdmitAllProfiles,
    )
    .await
    .unwrap();
    let cleared: Tools = read(&access, node_did).await.unwrap().unwrap();
    let cleared_config = cleared
        .integrations
        .as_ref()
        .unwrap()
        .lsp
        .as_ref()
        .and_then(|lsp| lsp.config.as_deref());
    assert!(
        cleared_config.is_none_or(str::is_empty),
        "removing lsp config from desired state must clear the stored override: {:?}",
        cleared_config
    );
}

#[tokio::test]
async fn tools_document_round_trips_read_only_commands() {
    let (db, _user, access, _binding) = crate::test_support::root::signed_test_node(
        "tools_document_round_trips_read_only_commands",
    )
    .await;
    let node_did = db.node_identity.did();

    let doc = Tools {
        tools_id: "steward-readonly-allowlist".to_string(),
        node_did: node_did.to_string(),
        host: Some(HostTools {
            bash: Some(BashTools {
                read_only_commands: Some(vec!["jq".to_string(), "echo".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    write_tools_document(
        &access,
        &doc,
        &gents_documents::test_support::AdmitAllProfiles,
    )
    .await
    .expect("write should persist the read_only_commands field");

    let loaded: Tools = access
        .transact_write("test.tools.read", |txn| {
            let doc = &doc;
            Box::pin(async move {
                gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Tools,
                    &doc.node_did,
                    &doc.tools_id,
                )
                .await?
                .map(|(_, value)| {
                    serde_json::from_value::<Tools>(value).map_err(anyhow::Error::from)
                })
                .transpose()
            })
        })
        .await
        .expect("read should succeed")
        .expect("tools should exist");
    assert_eq!(
        loaded
            .host
            .as_ref()
            .unwrap()
            .bash
            .as_ref()
            .unwrap()
            .read_only_commands,
        Some(vec!["jq".to_string(), "echo".to_string()]),
    );
}

#[tokio::test]
async fn tools_document_round_trips_write_tools() {
    let (db, _user, access, _binding) =
        crate::test_support::root::signed_test_node("tools_document_round_trips_write_tools").await;
    let node_did = db.node_identity.did();

    let decls = vec![WriteToolDecl {
        notification: None,
        tool_name: "request_action".to_string(),
        collection: "ActionRequest".to_string(),
        description: "Emit one ActionRequest describing a remediable drift.".to_string(),
        fields: vec![
            WriteToolField {
                name: "drift_sig".to_string(),
                required: true,
                fill: None,
            },
            WriteToolField {
                name: "summary".to_string(),
                required: true,
                fill: None,
            },
            WriteToolField {
                name: "target_paths".to_string(),
                required: false,
                fill: None,
            },
        ],
        output_obligation: None,
    }];
    let tools = Tools {
        tools_id: "steward-write-tools".to_string(),
        node_did: node_did.to_string(),
        datastore: Some(DatastoreTools {
            datastore_tool_surface_ids: Some(vec!["surface".to_string()]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let surface: DatastoreToolSurfaceDocument = serde_json::from_value(serde_json::json!({
        "surface_id": "surface",
        "node_did": node_did,
        "entries": decls,
    }))
    .unwrap();
    let plan = gents_documents::DesiredStateApplyPlan::new(vec![
        gents_documents::DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::DatastoreToolSurface,
            add: serde_json::to_value(&surface).unwrap(),
            update: serde_json::to_value(&surface).unwrap(),
        },
        gents_documents::DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::Tools,
            add: serde_json::to_value(&tools).unwrap(),
            update: serde_json::to_value(&tools).unwrap(),
        },
    ])
    .unwrap();
    access
        .transact_write("test.tools.write", |txn| {
            let plan = &plan;
            let tools = &tools;
            let surface = &surface;
            let decls = &decls;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(txn, plan, &gents_documents::test_support::AdmitAllProfiles).await?;
                let merged = gents_documents::merge_datastore_tool_surfaces(tools, [surface])?;
                assert_eq!(
                    merged.write_tools, *decls,
                    "write_tools must expand from the referenced surface through the desired-state owner"
                );
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn tools_document_round_trips_session_message_enablement() {
    let (db, _user, access, _binding) = crate::test_support::root::signed_test_node(
        "tools_document_round_trips_session_message_enablement",
    )
    .await;
    let node_did = db.node_identity.did();

    let doc = Tools {
        tools_id: "amy-background-tools".to_string(),
        node_did: node_did.to_string(),
        agents: Some(AgentTools {
            enabled: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    write_tools_document(
        &access,
        &doc,
        &gents_documents::test_support::AdmitAllProfiles,
    )
    .await
    .expect("write should persist the session-message enablement");

    let loaded: Tools = access
        .transact_write("test.tools.read", |txn| {
            let doc = &doc;
            Box::pin(async move {
                gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Tools,
                    &doc.node_did,
                    &doc.tools_id,
                )
                .await?
                .map(|(_, value)| {
                    serde_json::from_value::<Tools>(value).map_err(anyhow::Error::from)
                })
                .transpose()
            })
        })
        .await
        .expect("read should succeed")
        .expect("tools should exist");
    assert_eq!(loaded.agents.as_ref().unwrap().enabled, Some(true));
}

#[tokio::test]
async fn agent_description_round_trip_with_explicit_owner_fixture() {
    let (db, _user, access, binding) =
        crate::test_support::root::signed_test_node("agent_description_round_trip").await;
    let node_did = db.node_identity.did();
    let agent_did = &binding.agent_did;

    let authored = serde_json::json!({
        "agent_did": agent_did,
        "creation_nonce": binding.creation_nonce,
        "node_did": node_did,
        "display_name": "Amy General",
        "description": "A general-purpose assistant for research and writing.",
        "context_id": null,
        "inference_profile_id": format!("{agent_did}:inference"),
        "tags": null,
    });
    let sparse: Agent = serde_json::from_value(authored.clone()).unwrap();
    assert_eq!(
        sparse.context_id, None,
        "null context_id must decode to None"
    );
    assert!(
        sparse.tags.is_empty(),
        "null tags must decode to the empty default"
    );
    assert!(sparse.enabled, "omitted enabled must default to true");
    assert_eq!(
        serde_json::to_value(&sparse).unwrap(),
        serde_json::json!({
            "agent_did": agent_did,
        "creation_nonce": binding.creation_nonce,
            "node_did": node_did,
            "display_name": "Amy General",
            "description": "A general-purpose assistant for research and writing.",
            "inference_profile_id": format!("{agent_did}:inference"),
        })
    );

    let doc = Agent {
        agent_did: agent_did.clone(),
        creation_nonce: binding.creation_nonce.clone(),
        node_did: node_did.to_string(),
        display_name: Some("Amy General".to_string()),
        description: Some("A general-purpose assistant for research and writing.".to_string()),
        context_id: Some(format!("{agent_did}:context")),
        inference_profile_id: format!("{agent_did}:inference"),
        enabled: true,
        tags: Vec::new(),
        created_at: None,
    };
    let value = serde_json::to_value(&doc).unwrap();
    let plan = gents_documents::DesiredStateApplyPlan::new(vec![
        gents_documents::DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::Agent,
            add: value.clone(),
            update: value,
        },
    ])
    .unwrap();
    access
        .transact_write("test.agent.description", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_documents::test_support::AdmitAllProfiles,
                )
                .await
            })
        })
        .await
        .expect("upsert should persist description fields");

    let loaded = load_agent(&db.node, node_did, agent_did)
        .await
        .expect("load should succeed")
        .expect("agent should exist after upsert");
    assert_eq!(
        loaded.description,
        Some("A general-purpose assistant for research and writing.".to_string()),
    );
    assert_eq!(loaded.context_id, doc.context_id);
    assert_eq!(loaded.inference_profile_id, doc.inference_profile_id);
    assert!(loaded.enabled);
}

#[tokio::test]
async fn inference_retry_policy_fields_round_trip() {
    let (db, _user, access, _binding) =
        crate::test_support::root::signed_test_node("inference_retry_policy_fields_round_trip")
            .await;
    let node_did = db.node_identity.did();

    let retry = InferenceRetryPolicy {
        node_did: node_did.to_string(),
        retry_policy_id: "retry-policy".to_string(),
        display_name: Some("Retry Policy".to_string()),
        max_transport_retries: Some(4),
        backoff_ms: Some(vec![1_000, 5_000, 30_000]),
        max_resample_retries: Some(2),
        allow_repair: Some(false),
        interactive_max_retries: Some(2),
        ..Default::default()
    };
    let backend: gents_documents::InferenceBackend = serde_json::from_value(serde_json::json!({
        "node_did": node_did,
        "backend_id": "backend",
        "name": "Local",
        "provider_kind": "OpenAiCompatible",
        "endpoint": "http://127.0.0.1:1/v1",
        "auth": {"kind": "unauthenticated"}
    }))
    .unwrap();
    let profile = InferenceProfile {
        node_did: node_did.to_string(),
        profile_id: "retry-profile".to_string(),
        display_name: Some("Retry Profile".to_string()),
        backend_id: "backend".to_string(),
        model_name: "test-model".to_string(),
        reasoning_effort: Some(gents_documents::inference_defaults::ReasoningEffort::Max),
        execution_id: Some("retry-execution".to_string()),
        ..Default::default()
    };
    let execution = InferenceExecution {
        node_did: node_did.to_string(),
        execution_id: "retry-execution".to_string(),
        retry_policy_id: Some("retry-policy".to_string()),
        ..Default::default()
    };

    let plan = gents_documents::DesiredStateApplyPlan::new(vec![
        gents_documents::DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::InferenceBackend,
            add: serde_json::to_value(&backend).unwrap(),
            update: serde_json::to_value(&backend).unwrap(),
        },
        gents_documents::DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::InferenceRetryPolicy,
            add: serde_json::to_value(&retry).unwrap(),
            update: serde_json::to_value(&retry).unwrap(),
        },
        gents_documents::DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::InferenceExecution,
            add: serde_json::to_value(&execution).unwrap(),
            update: serde_json::to_value(&execution).unwrap(),
        },
        gents_documents::DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::InferenceProfile,
            add: serde_json::to_value(&profile).unwrap(),
            update: serde_json::to_value(&profile).unwrap(),
        },
    ])
    .expect("retry chain plan must be valid");
    access
        .transact_write("test.retry_chain.write", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_documents::test_support::AdmitAllProfiles,
                )
                .await
            })
        })
        .await
        .expect("apply should persist the retry chain");

    let loaded = load_inference_profile(&db.node, node_did, "retry-profile")
        .await
        .expect("load should succeed")
        .expect("profile should exist");
    assert_eq!(
        loaded.reasoning_effort,
        Some(gents_documents::inference_defaults::ReasoningEffort::Max)
    );
    assert_eq!(loaded.execution_id.as_deref(), Some("retry-execution"));
}

#[tokio::test]
async fn oauth_account_scope_matches_lean() {
    use gents_inference::auth::credential::{
        resolve_oauth_credential, resolve_oauth_pool, upsert_oauth_credential_on, AccountPick,
    };
    use gents_inference::backends::provider::BackendProviderOauthExt;
    use gents_loop::backend_provider::BackendProviderKind;
    use gents_lean_vocab::families::configuration_scope_oauth_account as family;
    let kind_of = |value: &str| -> BackendProviderKind {
        serde_json::from_value(serde_json::Value::String(value.to_owned())).unwrap()
    };
    let backend_auth = |auth: &family::OauthAuth| -> BackendAuth {
        let mut value = serde_json::to_value(auth).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .retain(|_, field| !field.is_null());
        let backend: BackendAuth = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&backend).unwrap(), value);
        backend
    };
    let cases = family::cases();
    let rows_of = |rows: &[family::OauthRow]| -> Vec<(String, String, Option<String>, bool)> {
        rows.iter()
            .map(|row| {
                (
                    row.owner.clone(),
                    row.provider_kind.clone(),
                    row.account.clone(),
                    row.enabled,
                )
            })
            .collect()
    };
    let rows = rows_of(&cases[0].input.rows);

    let (local_db, _local_user, access, _local_agent) =
        crate::test_support::root::signed_test_node("oauth-model-local").await;
    let (foreign_db, _foreign_user, foreign_access, _foreign_agent) =
        crate::test_support::root::signed_test_node("oauth-model-foreign").await;
    let local_node = local_db.node_identity.did();
    let foreign_node = foreign_db.node_identity.did();
    assert_ne!(local_node, foreign_node);
    let actual_owner = |model: &str| match model {
        "alice" => local_node,
        "bob" => foreign_node,
        _ => panic!("unknown model Node {model}"),
    };
    let model_owner = |actual: &str| {
        if actual == local_node {
            "alice"
        } else if actual == foreign_node {
            "bob"
        } else {
            panic!("unknown actual Node {actual}")
        }
    };
    for (owner, provider_kind, account, enabled) in &rows {
        let provider = kind_of(provider_kind).oauth_provider().unwrap();
        let owner = owner.as_str();
        let account = account.as_deref();
        let credential = gents_inference::auth::credential::OAuthCredential {
            doc_id: None,
            credential_id: format!("{provider}:{owner}:{}", account.unwrap_or("original")),
            node_did: actual_owner(owner).to_string(),
            provider: provider.to_string(),
            access_token: "access-TEST".to_string(),
            refresh_token: "refresh-TEST".to_string(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled: *enabled,
            account_ref: account.map(str::to_string),
            connected_at: None,
            provider_account_key: None,
            label: None,
        };
        let writer = if owner == "alice" {
            &access
        } else {
            &foreign_access
        };
        upsert_oauth_credential_on(writer, &credential)
            .await
            .unwrap();
    }

    let resolved_row = |owner: &str, account: Option<&str>| family::OauthResolved {
        owner: owner.to_owned(),
        account: account.map(str::to_owned),
    };
    for case in cases {
        let input = &case.input;
        assert_eq!(rows_of(&input.rows), rows);
        let auth = backend_auth(&input.auth);
        let text = serde_json::to_string(&auth).unwrap();
        assert!(
            !text.contains(&input.scope) && !text.contains(&input.stored_owner),
            "{text}"
        );
        let actual_scope = actual_owner(&input.scope);
        let resolved = match &auth {
            BackendAuth::NodeOAuthPool { .. } => {
                let provider = kind_of(&input.provider_kind).oauth_provider().unwrap();
                let pool_rows = resolve_oauth_pool(&access, actual_scope, provider)
                    .await
                    .unwrap();
                let mut pool: Vec<_> = pool_rows
                    .iter()
                    .map(|row| {
                        resolved_row(model_owner(&row.node_did), row.account_ref.as_deref())
                    })
                    .collect();
                pool.sort_by_key(|row| (row.owner.clone(), row.account.clone()));
                pool
            }
            BackendAuth::NodeOAuth { .. } => {
                let provider = kind_of(&input.provider_kind).oauth_provider().unwrap();
                resolve_oauth_credential(
                    &access,
                    actual_scope,
                    provider,
                    AccountPick::Reference(auth.oauth_account_ref()),
                )
                .await
                .unwrap()
                .map(|row| resolved_row(model_owner(&row.node_did), row.account_ref.as_deref()))
                .into_iter()
                .collect()
            }
            _ => Vec::new(),
        };
        case.check(family::OauthOutput {
            uses_node_oauth: auth.uses_node_oauth(),
            resolved,
        });
    }
}

#[tokio::test]
async fn reference_loader_isolates_foreign_malformed_backends_and_rejects_owned_corruption() {
    use gents_store::identity::NodeIdentity;

    let (db, _user, access, _binding) =
        crate::test_support::root::signed_test_node("reference-owner-isolation").await;
    let owner = db.node_identity.did();
    let foreign = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("reference-foreign-node")
    })
    .await
    .unwrap();
    assert_ne!(owner, foreign.did());
    gents_documents::test_support::bootstrap_signed_test_node(&access, foreign.did()).await;
    for (node_did, id, auth) in [
        (owner, "good", "unauthenticated"),
        (foreign.did(), "bad", "invalid-auth"),
    ] {
        let input = serde_json::json!({
            "node_did": node_did, "backend_id": id, "name": "fixture",
            "provider_kind": "OpenAiCompatible", "endpoint": "http://localhost:8000/v1",
            "auth_kind": auth, "enabled": true,
            "creation_nonce": uuid::Uuid::new_v4().to_string(),
        });
        access.transact_write("test.reference_fixture", |txn| {
            let input = &input;
            Box::pin(async move {
                let response = txn.execute_client_signed_create(
                    "mutation($input: InferenceBackendMetadataMutationInputArg!) { create_InferenceBackendMetadata(input: $input) { _docID } }",
                    &serde_json::json!({"input": input}),
                    gents_protocol::schemas::INFERENCE_BACKEND_METADATA,
                ).await?;
                let doc = gents_store::graphql::created_doc_id(&response, "InferenceBackendMetadata")?;
                gents_documents::publish_node_child_in_txn(
                    txn, "InferenceBackendMetadata", &doc, node_did,
                ).await?;
                Ok::<_, anyhow::Error>(())
            })
        }).await.unwrap();
    }
    access
        .transact_write("test.reference_owner_isolation", |txn| {
            Box::pin(async move {
                let refs = ConfigReferences::load_in_txn(txn, owner).await?;
                refs.validate()?;
                assert!(refs.documents().any(|((collection, id), _)| *collection
                    == gents_store::collection::Collection::InferenceBackend
                    && id == "good"));
                assert!(!refs.documents().any(|((_, id), _)| id == "bad"));
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let response = access.execute(&format!(
        r#"{{ InferenceBackendMetadata(filter: {{node_did: {{_eq: "{}"}}, backend_id: {{_eq: "good"}}}}, limit: 2) {{ _docID }} }}"#,
        gents_store::graphql::escape_graphql_string(owner),
    )).await.unwrap();
    let [row] = response["data"]["InferenceBackendMetadata"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("owned backend must be unique");
    };
    let doc = gents_store::graphql::escape_graphql_string(row["_docID"].as_str().unwrap());
    let mutation = format!(
        r#"mutation($input: InferenceBackendMetadataMutationInputArg!) {{ update_InferenceBackendMetadata(docID: "{doc}", input: $input) {{ _docID }} }}"#,
    );
    access
        .transact_write("test.corrupt_owned_backend", |txn| {
            let mutation = &mutation;
            Box::pin(async move {
                txn.execute_client_signed_update(
                    mutation,
                    &serde_json::json!({"input": {"auth_kind": "invalid-auth"}}),
                    gents_protocol::schemas::INFERENCE_BACKEND_METADATA,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let result = access
        .transact_write("test.reference_owned_corruption", |txn| {
            Box::pin(async move { ConfigReferences::load_in_txn(txn, owner).await?.validate() })
        })
        .await;
    assert!(
        result.is_err(),
        "owned malformed configuration must fail closed"
    );
}
