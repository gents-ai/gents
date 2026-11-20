use super::*;
use gents_store::identity::NodeIdentity as _;

fn rootless_archive(
    version: &str,
    root: Option<&str>,
) -> (tempfile::TempDir, crate::pack::archive::PackArchive) {
    let (guard, dir) = crate::test_support::fixture_pack_copy("dependee_fixture", &json!({}));
    let path = dir.join("pack_config.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["tools"][0]["host"] = json!({"bash":{"mode":"ReadOnly"},"root":root});
    std::fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
    let path = dir.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["version"] = json!(version);
    std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (bytes, _) = crate::pack::archive::pack_dir(&dir).unwrap();
    (
        guard,
        crate::pack::archive::PackArchive::from_bytes(&bytes).unwrap(),
    )
}

async fn installer_root(
    access: &ConfigAccess,
    node: &Arc<defra_node::Node>,
    runtime: &gents_store::identity::KeyIdentity,
    root: &str,
) {
    let user = identity::Did::new(access.actor_did().unwrap()).unwrap();
    let binding = gents_store::identity::provision_agent_identity_authorized(node, runtime, &user)
        .await
        .unwrap();
    gents_inference::test_support::install_test_agent_binding_with_access(
        runtime.did(),
        "Installer",
        &binding,
        access,
    )
    .await;
    gents_documents::test_support::set_test_default_agent_as(
        access,
        runtime.did(),
        &binding.agent_did,
    )
    .await;
    let tools = json!({"node_did":runtime.did(), "tools_id":format!("{}:tools", binding.agent_did), "host":{"root":root,"bash":{"mode":"ReadOnly"}}});
    write_dependee_tools(access, &tools).await;
}

async fn apply_pack(
    access: &ConfigAccess,
    owner: &str,
    archive: &crate::pack::archive::PackArchive,
    prepared: &PreparedDocumentPackInstall,
    policy: DriftPolicy,
) -> Result<InstallReport> {
    super::super::super::install_prepared_document_graph(
        &crate::pack::InstallTarget {
            access,
            owner,
            plugin_home: None,
        },
        archive,
        prepared,
        crate::pack::DocumentGraphPolicy {
            drift: policy,
            profiles: &gents_inference::resolved::ProviderProfilePolicy,
        },
        false,
        true,
    )
    .await
    .map(|(report, _)| report)
    .map_err(super::super::PackChangeError::into_anyhow)
}

#[tokio::test]
async fn reinstall_and_upgrade_preserve_the_live_host_root() {
    let (directory, runtime, node, access) =
        super::super::super::agent_bindings::tests::fixture().await;
    let on = GraphNode {
        access: &access,
        node: &node,
        runtime: &runtime,
    };
    let broad = directory.path().to_str().unwrap();
    let narrow = directory.path().join("project");
    let later = narrow.join("private");
    std::fs::create_dir_all(&later).unwrap();
    installer_root(&access, &node, &runtime, broad).await;
    let (_guard, first) = rootless_archive("1.0.0", None);
    let prepared = prepare_graph(&on, &first, &[], &|_| None, &Default::default(), None)
        .await
        .unwrap();
    assert_eq!(
        prepared.config.tools[0]
            .host
            .as_ref()
            .unwrap()
            .root
            .as_deref(),
        Some(broad)
    );
    assert_eq!(prepared.installer_root_agents(first.manifest()).len(), 1);
    apply_pack(
        &access,
        runtime.did(),
        &first,
        &prepared,
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();

    let mut tools = dependee_tools(&access, runtime.did()).await;
    tools["host"]["root"] = json!(narrow);
    write_dependee_tools(&access, &tools).await;
    let (_guard, upgrade) = rootless_archive("2.0.0", None);
    for (archive, policy) in [
        (&first, DriftPolicy::Overwrite),
        (&first, DriftPolicy::Refuse),
        (&upgrade, DriftPolicy::Overwrite),
    ] {
        let prepared = prepare_graph(&on, archive, &[], &|_| None, &Default::default(), None)
            .await
            .unwrap();
        assert_eq!(
            prepared.config.tools[0]
                .host
                .as_ref()
                .unwrap()
                .root
                .as_deref(),
            narrow.to_str()
        );
        assert!(
            prepared
                .installer_root_agents(archive.manifest())
                .is_empty(),
            "a retained private root must not request the installer's broader grant"
        );
        apply_pack(&access, runtime.did(), archive, &prepared, policy)
            .await
            .unwrap();
        assert_eq!(
            dependee_tools(&access, runtime.did()).await["host"]["root"],
            json!(narrow)
        );
    }

    let (_guards, _, dependent) = documents_dependency_pair(false, false);
    let (_guard, dependency_upgrade) = rootless_archive("3.0.0", None);
    let overrides = BTreeMap::from([("fixture/dependee_fixture".to_owned(), BTreeMap::new())]);
    let prepared = prepare_graph(
        &on,
        &dependent,
        &[&dependency_upgrade],
        &|_| None,
        &overrides,
        None,
    )
    .await
    .unwrap();
    assert!(prepared
        .installer_root_agents(dependent.manifest())
        .is_empty());
    apply_pack(
        &access,
        runtime.did(),
        &dependent,
        &prepared,
        DriftPolicy::Overwrite,
    )
    .await
    .unwrap();
    assert_eq!(
        dependee_tools(&access, runtime.did()).await["host"]["root"],
        json!(narrow)
    );

    let prepared = prepare_graph(
        &on,
        &dependency_upgrade,
        &[],
        &|_| None,
        &Default::default(),
        None,
    )
    .await
    .unwrap();
    tools["host"]["root"] = json!(later);
    write_dependee_tools(&access, &tools).await;
    let error = apply_pack(
        &access,
        runtime.did(),
        &dependency_upgrade,
        &prepared,
        DriftPolicy::Overwrite,
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("changed during preparation"));
    assert_eq!(
        dependee_tools(&access, runtime.did()).await["host"]["root"],
        json!(later)
    );
    tools["host"]["root"] = Value::Null;
    write_dependee_tools(&access, &tools).await;
    let prepared = prepare_graph(
        &on,
        &dependency_upgrade,
        &[],
        &|_| None,
        &Default::default(),
        None,
    )
    .await
    .unwrap();
    assert!(prepared.config.tools[0]
        .host
        .as_ref()
        .unwrap()
        .root
        .is_none());
    assert!(prepared
        .installer_root_agents(dependency_upgrade.manifest())
        .is_empty());
    apply_pack(
        &access,
        runtime.did(),
        &dependency_upgrade,
        &prepared,
        DriftPolicy::Overwrite,
    )
    .await
    .unwrap();
    assert!(dependee_tools(&access, runtime.did()).await["host"]["root"].is_null());
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn unchanged_inherited_roots_keep_failed_grant_recovery_and_explicit_variables_win() {
    let (directory, runtime, node, access) =
        super::super::super::agent_bindings::tests::fixture().await;
    let on = GraphNode {
        access: &access,
        node: &node,
        runtime: &runtime,
    };
    let broad = directory.path().to_str().unwrap();
    installer_root(&access, &node, &runtime, broad).await;
    let (_guard, archive) = rootless_archive("1.0.0", None);
    for _ in 0..2 {
        let prepared = prepare_graph(&on, &archive, &[], &|_| None, &Default::default(), None)
            .await
            .unwrap();
        assert_eq!(prepared.installer_root_agents(archive.manifest()).len(), 1);
        apply_pack(
            &access,
            runtime.did(),
            &archive,
            &prepared,
            DriftPolicy::Refuse,
        )
        .await
        .unwrap();
    }
    let (_guard, authored) = rootless_archive("2.0.0", Some("${ROOT}"));
    let explicit = directory.path().join("chosen");
    let environment =
        |name: &str| (name == "ROOT").then(|| explicit.to_string_lossy().into_owned());
    let prepared = prepare_graph(&on, &authored, &[], &environment, &Default::default(), None)
        .await
        .unwrap();
    assert_eq!(
        prepared.config.tools[0]
            .host
            .as_ref()
            .unwrap()
            .root
            .as_deref(),
        explicit.to_str()
    );
    assert!(prepared
        .installer_root_agents(authored.manifest())
        .is_empty());
    apply_pack(
        &access,
        runtime.did(),
        &authored,
        &prepared,
        DriftPolicy::Overwrite,
    )
    .await
    .unwrap();
    assert_eq!(
        dependee_tools(&access, runtime.did()).await["host"]["root"],
        json!(explicit)
    );
    node.shutdown().await.unwrap();
}
