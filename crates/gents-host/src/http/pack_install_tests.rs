use super::*;
use crate::test_support::pack_fixture_dir;

const NODE: &str = "did:key:zRemotePackInstallNode";

fn archive(name: &str) -> Arc<PackArchive> {
    let (bytes, _) = gents_pack::pack::archive::pack_dir(&pack_fixture_dir(name)).unwrap();
    Arc::new(PackArchive::from_bytes(&bytes).unwrap())
}

fn body(root: &PackArchive, dependencies: &[Arc<PackArchive>], node_did: &str) -> Vec<u8> {
    encode_request(root, dependencies, |root, dependencies| InstallRequest {
        move_pin_tools: Vec::new(),
        root_source: gents_pack::pack::PackSource::Local { file_name: None },
        dependency_sources: vec![
            gents_pack::pack::PackSource::Local { file_name: None };
            dependencies.len()
        ],
        node_did: node_did.into(),
        root,
        dependencies,
        inference_slots: Vec::new(),
        dependency_variables: Vec::new(),
        bind_agents: Vec::new(),
        overwrite: false,
        keep: false,
        preview: false,
        force_rebind: false,
        grant_authority: false,
        explicit: true,
        wire: Vec::new(),
        client_build: gents_protocol::build_info::BuildInfo::current(Vec::new()),
    })
    .unwrap()
}

fn refusal(body: &[u8]) -> String {
    format!("{:#}", decode_request(body, NODE).err().expect("refused"))
}

#[test]
fn received_archives_carry_every_declared_path_at_the_resolved_digest() {
    for name in ["documents_fixture", "job_plugin_fixture"] {
        let sent = archive(name);
        let declared = gents_pack::pack::declared_paths(sent.manifest());
        assert!(
            declared.len() > sent.manifest().metadata.assets.len(),
            "{name} declares the manifest beside its assets"
        );
        let received = decode_request(&body(&sent, &[], NODE), NODE).unwrap();
        assert_eq!(received.root.digest(), sent.digest(), "{name}");
        for path in declared {
            assert_eq!(
                received.root.asset(&path).unwrap(),
                sent.asset(&path).unwrap(),
                "{name} {path}"
            );
        }
    }
}

#[test]
fn remote_sources_keep_file_names_but_reject_paths() {
    let root = archive("documents_fixture");
    let mut request: serde_json::Value = serde_json::from_slice(&body(&root, &[], NODE)).unwrap();
    request["root_source"] = serde_json::json!({"kind": "local", "file_name": "demo.pack"});
    let received = decode_request(&serde_json::to_vec(&request).unwrap(), NODE).unwrap();
    assert_eq!(
        received.root.provenance(),
        gents_pack::pack::PackSource::Local {
            file_name: Some("demo.pack".into())
        }
    );
    request["root_source"]["file_name"] = "/private/demo.pack".into();
    assert!(refusal(&serde_json::to_vec(&request).unwrap()).contains("invalid local pack filename"));
    request["root_source"] = serde_json::json!({
        "kind": "registry", "namespace": "other", "name": "other", "version": "1.0.0", "artifact_digest": "a".repeat(64)
    });
    assert!(
        refusal(&serde_json::to_vec(&request).unwrap()).contains("registry source does not match")
    );
}

#[test]
fn remote_install_requires_source_fields() {
    let root = archive("documents_fixture");
    let request: serde_json::Value = serde_json::from_slice(&body(&root, &[], NODE)).unwrap();
    for field in ["root_source", "dependency_sources"] {
        let mut missing = request.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(refusal(&serde_json::to_vec(&missing).unwrap()).contains(field));
    }
}

#[test]
fn remote_dependency_sources_follow_their_archives() {
    let root = archive("dependent_fixture");
    let dependency = archive("dependee_fixture");
    let mut request: serde_json::Value =
        serde_json::from_slice(&body(&root, std::slice::from_ref(&dependency), NODE)).unwrap();
    request["dependency_sources"] = serde_json::json!([{"kind": "store"}]);
    let received = decode_request(&serde_json::to_vec(&request).unwrap(), NODE).unwrap();
    assert_eq!(
        received.dependencies[0].provenance(),
        gents_pack::pack::PackSource::Store
    );
    request["dependency_sources"] = serde_json::json!([{"kind": "store"}, {"kind": "store"}]);
    assert!(refusal(&serde_json::to_vec(&request).unwrap()).contains("dependency source count"));
}

#[test]
fn received_dependencies_are_exactly_the_root_dependency_list() {
    let root = archive("dependent_fixture");
    let dependee = archive("dependee_fixture");
    let received =
        decode_request(&body(&root, std::slice::from_ref(&dependee), NODE), NODE).unwrap();
    assert_eq!(received.dependencies.len(), 1);
    assert_eq!(received.dependencies[0].digest(), dependee.digest());

    for (dependencies, case) in [
        (Vec::new(), "missing"),
        (vec![dependee.clone(), dependee.clone()], "duplicate"),
        (
            vec![dependee.clone(), archive("documents_fixture")],
            "extra",
        ),
        (vec![archive("documents_fixture")], "substituted"),
    ] {
        let error = refusal(&body(&root, &dependencies, NODE));
        assert!(
            error.contains("do not match the root manifest"),
            "{case}: {error}"
        );
    }
}

#[test]
fn received_request_preserves_dependency_variable_overrides() {
    let root = archive("dependent_fixture");
    let dependee = archive("dependee_fixture");
    let mut request: serde_json::Value =
        serde_json::from_slice(&body(&root, &[dependee], NODE)).unwrap();
    assert_eq!(request["dependency_variables"], serde_json::json!([]));
    let mut missing = request.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("dependency_variables");
    assert!(refusal(&serde_json::to_vec(&missing).unwrap()).contains("dependency_variables"));
    request["dependency_variables"] = serde_json::json!(["fixture/dependee_fixture:LABEL=value"]);
    let received = decode_request(&serde_json::to_vec(&request).unwrap(), NODE).unwrap();
    assert_eq!(
        received.request.dependency_variables,
        ["fixture/dependee_fixture:LABEL=value"]
    );
}

#[test]
fn received_requests_bind_the_runtime_node_and_reviewed_root() {
    let root = archive("documents_fixture");
    assert!(refusal(&body(&root, &[], "did:key:zOtherNode")).contains("different node"));
    let assets = archive("assets_fixture");
    assert_eq!(
        decode_request(&body(&assets, &[], NODE), NODE)
            .unwrap()
            .root
            .digest(),
        assets.digest()
    );

    let mut conflicting: serde_json::Value =
        serde_json::from_slice(&body(&root, &[], NODE)).unwrap();
    conflicting["keep"] = true.into();
    conflicting["overwrite"] = true.into();
    assert!(refusal(&serde_json::to_vec(&conflicting).unwrap()).contains("conflicting drift"));

    let mut redirected = conflicting;
    redirected["overwrite"] = false.into();
    redirected["home"] = "/elsewhere".into();
    assert!(refusal(&serde_json::to_vec(&redirected).unwrap()).contains("unknown field"));

    let mut tampered: serde_json::Value = serde_json::from_slice(&body(&root, &[], NODE)).unwrap();
    let mut bytes = STANDARD.decode(tampered["root"].as_str().unwrap()).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    tampered["root"] = STANDARD.encode(bytes).into();
    refusal(&serde_json::to_vec(&tampered).unwrap());
}

#[test]
fn archive_bytes_over_the_shared_pack_bound_are_refused_by_name() {
    ensure_archive_bound(MAX_ARCHIVE_BYTES).unwrap();
    let error = format!(
        "{:#}",
        ensure_archive_bound(MAX_ARCHIVE_BYTES + 1).unwrap_err()
    );
    assert!(
        error.contains(&format!("over the {MAX_ARCHIVE_BYTES} byte bound"))
            && error.contains("nothing was installed"),
        "{error}"
    );
    assert_eq!(MAX_ARCHIVE_BYTES, gents_pack::pack::archive::MAX_PACK_BYTES);
    assert!(
        MAX_BODY >= base64::encoded_len(MAX_ARCHIVE_BYTES, true).unwrap() + MAX_FIELD_BYTES,
        "the request body admits every archive closure within the shared bound"
    );
}

#[test]
fn hosted_pin_move_selection_is_explicit_and_required_on_the_wire() {
    let root = archive("documents_fixture");
    let mut request: serde_json::Value = serde_json::from_slice(&body(&root, &[], NODE)).unwrap();
    request["move_pin_tools"] = serde_json::json!(["named-A", "named-B"]);
    let received = decode_request(&serde_json::to_vec(&request).unwrap(), NODE).unwrap();
    assert_eq!(received.request.move_pin_tools, ["named-A", "named-B"]);
    request.as_object_mut().unwrap().remove("move_pin_tools");
    assert!(refusal(&serde_json::to_vec(&request).unwrap()).contains("move_pin_tools"));
}

#[tokio::test]
async fn hosted_fresh_install_preview_plans_wiring_without_publication() {
    use serde_json::json;
    let home = crate::test_support::ClientHome::new().await;
    let owner = home.home.identity.did().to_owned();
    let binding = home.agent("fixture").await;
    let config = serde_json::from_value(json!({
        "node": {"node_did": owner},
        "tools": [{"tools_id": "preview-tools", "node_did": owner}],
        "contexts": [{"context_id": "preview-context", "node_did": owner, "tools_id": "preview-tools"}],
        "agents": [{"agent_did": binding.agent_did, "creation_nonce": binding.creation_nonce,
            "node_did": owner, "context_id": "preview-context",
            "inference_profile_id": format!("{}:inference", binding.agent_did), "enabled": true}]
    })).unwrap();
    let plan = gents_documents::DesiredStateApplyPlan::from_pack_config(&config).unwrap();
    home.access
        .transact_write("test.hosted_preview.seed", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    let (_fixture, path) =
        gents_pack::test_support::fixture_pack_copy("job_plugin_fixture", &json!({}));
    // This test exercises hosted install/wiring, not schema authority. Keep
    // the fixture a job plugin without its unrelated application collection.
    let manifest_path = path.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["schemas"] = json!([]);
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .retain(|asset| asset.as_str() != Some("schemas/fixture_note.graphql"));
    manifest["plugins"][0]
        .as_object_mut()
        .unwrap()
        .remove("data");
    std::fs::remove_file(path.join("schemas/fixture_note.graphql")).unwrap();
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(path.join("pack_config.json"), serde_json::to_vec(&json!({
        "node": {},
        "agent_targets": [{"target_id": "preview-target", "target_node_did": "did:key:zPreviewForeign",
            "agent_did": "did:key:zPreviewWorker", "name": "preview-target"}],
        "tools": [{"tools_id": "preview-pack-tools", "integrations": {"plugins": [{
            "plugin": "fixture/job_fixture", "job": {"agent_targets": ["preview-target"], "policy": {"dynamic": "check", "dynamic_targets": ["preview-target"]}}
        }]}}]
    })).unwrap()).unwrap();
    let (bytes, _) = gents_pack::pack::archive::pack_dir(&path).unwrap();
    let archive = PackArchive::from_bytes(&bytes).unwrap();
    // A caller without a held signer must be rejected before config reads or
    // writes, not retried as an ordinary GraphQL mutation.
    let source = gents_pack::pack::resolved_pack::ResolvedPack::received(
        PackArchive::from_bytes(&bytes).unwrap(),
        gents_pack::pack::PackSource::Local { file_name: None },
    )
    .unwrap();
    let unsigned = gents_store::config_client::ConfigAccess::graphql_as(
        "http://127.0.0.1:1/graphql",
        "did:key:zUnheldCaller",
    );
    let error = gents_pack::pack::plugin_wire::enable_pack_dynamic(
        &unsigned,
        &owner,
        &binding.agent_did,
        &source,
        std::path::Path::new("."),
        gents_service::home_state::load_held_user_identity,
    )
    .await
    .unwrap_err();
    assert!(
        error.is::<gents_pack::pack::plugin_wire::HostedCallerMismatch>(),
        "{error:#}"
    );

    let mut request: serde_json::Value =
        serde_json::from_slice(&body(&archive, &[], &owner)).unwrap();
    request["preview"] = true.into();
    request["wire"] = json!([binding.agent_did]);
    let received = decode_request(&serde_json::to_vec(&request).unwrap(), &owner).unwrap();
    let plugin_home = tempfile::tempdir().unwrap();
    let query = "{ Tools { tools_id integrations _version {cid} } AgentContext {context_id skills _version {cid}} AgentTarget {target_id} Agent {agent_did _version {cid}} PackInstallation {_docID} }";
    let before = home.access.execute(query).await.unwrap();
    assert!(before.get("errors").is_none(), "{before}");
    let preview = install_owned(
        &owner,
        plugin_home.path().to_owned(),
        received,
        home.access.clone(),
    )
    .await
    .unwrap();
    assert_eq!(preview["wired"].as_array().unwrap().len(), 1, "{preview}");
    assert_eq!(preview["wired"][0]["preview"], true);
    assert_eq!(
        preview["wired"][0]["report"]["job"]["policy"]["dynamic"],
        "check"
    );
    assert_eq!(home.access.execute(query).await.unwrap(), before);
    // Read-only preview must leave a pristine plugin home pristine.
    fn relative_files(root: &std::path::Path, at: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(relative_files(root, &path));
            } else {
                files.push(path.strip_prefix(root).unwrap().to_owned());
            }
        }
        files.sort();
        files
    }
    assert_eq!(
        relative_files(plugin_home.path(), plugin_home.path()),
        Vec::<std::path::PathBuf>::new(),
        "preview must not provision identities or publish plugins"
    );
    // A fresh authored slot is an explicit skip, not failure of the install
    // preview. The original archive remains available for apply parity below.
    let config_path = path.join("pack_config.json");
    let mut authored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    authored["agents"] = json!([{"agent_slot": "fresh-worker", "context_id": "fresh-context", "inference_profile_id": "gents:inference-slot:worker"}]);
    std::fs::write(&config_path, serde_json::to_vec(&authored).unwrap()).unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["inference_slots"] = json!([{"name": "worker", "description": "Fresh preview worker", "agents": ["fresh-worker"]}]);
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (fresh_bytes, _) = gents_pack::pack::archive::pack_dir(&path).unwrap();
    let fresh_archive = PackArchive::from_bytes(&fresh_bytes).unwrap();
    let mut fresh_request: serde_json::Value =
        serde_json::from_slice(&body(&fresh_archive, &[], &owner)).unwrap();
    fresh_request["preview"] = true.into();
    fresh_request["wire"] = json!([binding.agent_did]);
    let received = decode_request(&serde_json::to_vec(&fresh_request).unwrap(), &owner).unwrap();
    let skipped = install_owned(
        &owner,
        plugin_home.path().to_owned(),
        received,
        home.access.clone(),
    )
    .await
    .unwrap();
    assert_eq!(skipped["wired"][0]["slot"], "fresh-worker", "{skipped}");
    assert_eq!(skipped["wired"][0]["skipped"], true);
    assert_eq!(
        skipped["wired"][0]["reason"],
        "needs runtime identity provisioning"
    );
    assert!(skipped["wired"][0]["next"]
        .as_str()
        .unwrap()
        .contains("Install"));
    assert_eq!(home.access.execute(query).await.unwrap(), before);
    assert_eq!(
        relative_files(plugin_home.path(), plugin_home.path()),
        Vec::<std::path::PathBuf>::new()
    );
    request["preview"] = false.into();
    let received = decode_request(&serde_json::to_vec(&request).unwrap(), &owner).unwrap();
    let applied = install_owned(
        &owner,
        plugin_home.path().to_owned(),
        received,
        home.access.clone(),
    )
    .await
    .unwrap();
    assert_eq!(preview["wired"][0]["report"], applied["wired"][0]["report"]);
}
