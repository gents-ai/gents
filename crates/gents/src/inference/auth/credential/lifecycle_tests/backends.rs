use super::*;

#[tokio::test]
async fn only_added_accounts_create_a_backend() {
    for product in PRODUCTS {
        let (db, _user, access) = access().await;
        let did = db.node_identity.did().to_owned();
        store(&access, product.sign_in(&did, "a", "refresh-a")).await;
        assert!(backends(&access, &did).await.is_empty(), "{product:?}");

        let b = store(&access, product.sign_in(&did, "b", "refresh-b")).await;
        let reference = b.credential.account_ref.clone().unwrap();
        let stored = backends(&access, &did).await;
        assert_eq!(stored.len(), 1, "{product:?}");
        let backend = &stored[0];
        let spec = preset(product);
        assert_eq!(
            backend.backend_id,
            format!("{}-{reference}", product.provider())
        );
        assert_eq!(Some(backend.name.clone()), b.credential.label);
        assert_eq!(
            backend.auth,
            crate::documents::BackendAuth::NodeOAuth {
                account_ref: Some(reference.clone())
            }
        );
        assert_eq!(backend.provider_kind, spec.provider_kind);
        assert_eq!(backend.openai_wire_api, spec.openai_wire_api);
        assert_eq!(backend.endpoint, spec.endpoint);
        assert!(backend.enabled);

        store(&access, product.sign_in(&did, "b", "refresh-b2")).await;
        let ids = |backends: Vec<crate::InferenceBackend>| {
            backends
                .into_iter()
                .map(|backend| (backend.backend_id, backend.name))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(backends(&access, &did).await),
            ids(stored),
            "{product:?}"
        );
        labeled(
            &access,
            product.sign_in(&did, "b", "refresh-b3"),
            "Personal",
        )
        .await;
        let renamed = backends(&access, &did).await;
        assert_eq!(renamed.len(), 1);
        assert_eq!(renamed[0].name, "Personal", "{product:?}");
    }
}

#[tokio::test]
async fn each_accounts_backend_keeps_its_own_models() {
    for product in PRODUCTS {
        let (db, _user, access) = access().await;
        let did = db.node_identity.did().to_owned();
        let spec = preset(product);
        let original = crate::InferenceBackend {
            physical_doc_id: None,
            node_did: did.clone(),
            backend_id: format!("{}-original", product.provider()),
            name: "Original".into(),
            provider_kind: spec.provider_kind,
            openai_wire_api: spec.openai_wire_api,
            endpoint: spec.endpoint.clone(),
            claude_platform: None,
            auth: crate::documents::BackendAuth::NodeOAuth { account_ref: None },
            connect_timeout_secs: None,
            discovery_timeout_secs: None,
            max_concurrent: None,
            max_queue_depth: None,
            enabled: true,
            tags: Vec::new(),
        };
        crate::documents::write_inference_backend_document(&access, &original)
            .await
            .unwrap();
        store(&access, product.sign_in(&did, "a", "refresh-a")).await;
        store(&access, product.sign_in(&did, "b", "refresh-b")).await;
        let added = backends(&access, &did)
            .await
            .into_iter()
            .find(|backend| backend.backend_id != original.backend_id)
            .expect("the added account's backend");
        for (backend, model) in [(&original, "model-a"), (&added, "model-b")] {
            let advertised: crate::documents::AdvertisedModel = serde_json::from_value(json!({
                "model_name": model,
                "display_name": null,
                "context_window": null,
                "max_context_window": null,
                "max_output_tokens": null,
                "reasoning_efforts": null,
            }))
            .unwrap();
            crate::inference::backends::registry::record_discovered_catalog_on(
                &access,
                backend,
                vec![advertised],
            )
            .await
            .unwrap();
        }
        for (backend, model) in [(&original, "model-a"), (&added, "model-b")] {
            let node_did = did.as_str();
            let backend_id = backend.backend_id.as_str();
            let observation = access
                .transact_readonly("test.backend_observation", |txn| {
                    Box::pin(async move {
                        crate::inference::backends::registry::lookup_backend_observation_in_txn(
                            txn, node_did, backend_id,
                        )
                        .await
                    })
                })
                .await
                .unwrap()
                .unwrap();
            let models: Vec<_> = observation
                .catalogs
                .iter()
                .flat_map(|catalog| catalog.models.iter().map(|m| m.model_name.as_str()))
                .collect();
            assert_eq!(models, [model], "{product:?}");
        }
    }
}

#[tokio::test]
async fn a_lost_backend_comes_back_at_the_next_sign_in() {
    for product in PRODUCTS {
        let (db, _user, access) = access().await;
        let did = db.node_identity.did().to_owned();
        store(&access, product.sign_in(&did, "a", "refresh-a")).await;
        let b = store(&access, product.sign_in(&did, "b", "refresh-b")).await;
        let reference = b.credential.account_ref.clone().unwrap();
        let serving = |backends: Vec<crate::InferenceBackend>| {
            backends
                .into_iter()
                .filter(|backend| backend.auth.oauth_account_ref() == Some(reference.as_str()))
                .count()
        };
        let backend_id = format!("{}-{reference}", product.provider());
        let plan = crate::store::config_client::DesiredStateApplyPlan::new(Vec::new())
            .unwrap()
            .with_removals(vec![(
                crate::Collection::InferenceBackend,
                did.clone(),
                backend_id,
            )])
            .unwrap();
        access
            .transact_write("test.backend_rm", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    crate::documents::apply_desired_state_plan(txn, plan).await
                })
            })
            .await
            .unwrap();
        assert_eq!(serving(backends(&access, &did).await), 0);

        let again = store(&access, product.sign_in(&did, "b", "refresh-b2")).await;
        assert_eq!(again.result, SignInResult::Refreshed);
        assert_eq!(serving(backends(&access, &did).await), 1, "{product:?}");
        store(&access, product.sign_in(&did, "b", "refresh-b3")).await;
        assert_eq!(serving(backends(&access, &did).await), 1, "{product:?}");
    }
}

#[tokio::test]
async fn an_unrelated_invalid_profile_does_not_block_an_account_backend() {
    let (db, _user, access) = access().await;
    let did = db.node_identity.did();
    store(&access, Product::Claude.sign_in(did, "a", "refresh-a")).await;
    access.transact_write("test.seed_invalid_profile", |txn| {
        Box::pin(async move {
            let input = json!({"node_did": did, "profile_id": "profile-x", "backend_id": "missing-backend", "model_name": "model-x"});
            let response = txn.execute_client_signed_create(
                "mutation($input: InferenceProfileMutationInputArg!) { create_InferenceProfile(input: $input) { _docID } }",
                &json!({"input": input}),
                gents_protocol::schemas::INFERENCE_PROFILE,
            ).await?;
            let doc = gents_protocol::graphql::extract_mutation_doc_id(&response, "InferenceProfile")?;
            crate::store::config_client::publish_node_child_in_txn(txn, "InferenceProfile", &doc, did).await?;
            Ok::<(), anyhow::Error>(())
        })
    }).await.unwrap();

    let profiles = || async {
        access
            .transact_readonly("test.invalid_profile", |txn| {
                Box::pin(async move {
                    crate::documents::list_inference_profiles_in_txn(txn, did).await
                })
            })
            .await
            .unwrap()
    };
    let before = profiles().await;
    let profile = &before[0];
    let signed = store_sign_in(
        &access,
        Product::Claude.sign_in(did, "b", "refresh-b"),
        None,
    )
    .await;
    let stored = backends(&access, did).await;
    let backend = stored
        .first()
        .expect("account backend to compare with the model");
    let model_input = json!({
        "documents": [
            {"key": format!("Node:{did}"), "required": [], "localValid": true},
            {"key": format!("InferenceProfile:{}", profile.profile_id),
             "required": [format!("InferenceBackend:{}", profile.backend_id)], "localValid": true},
            {"key": format!("InferenceBackend:{}", backend.backend_id), "required": [], "localValid": true},
        ],
        "changed": [format!("InferenceBackend:{}", backend.backend_id)],
    });
    let expected = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let proofs = gents_lean_contract::proofs_dir().unwrap();
        gents_lean_contract::run_lake_build(&proofs).unwrap();
        let mut lean = Command::new("lake")
            .args([
                "env",
                "lean",
                "--run",
                "Proofs/Conformance/ConfigurationAffected.lean",
            ])
            .current_dir(proofs)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(lean.stdin.take().unwrap(), "{model_input}").unwrap();
        let output = lean.wait_with_output().unwrap();
        assert!(output.status.success(), "Lean affected validation failed");
        serde_json::from_slice::<bool>(&output.stdout).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        signed
            .as_ref()
            .is_ok_and(|value| value.result == SignInResult::Added),
        expected
    );
    assert_eq!(stored.len(), usize::from(expected));
    assert_eq!(rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.len(), 2);
    assert_eq!(
        serde_json::to_value(profiles().await).unwrap(),
        serde_json::to_value(before).unwrap()
    );
}
