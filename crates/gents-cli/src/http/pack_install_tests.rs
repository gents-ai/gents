use super::*;
use crate::commands::pack::test_support::fixture_dir;

const NODE: &str = "did:key:zRemotePackInstallNode";

fn archive(name: &str) -> Arc<PackArchive> {
    let (bytes, _) = gents::pack::archive::pack_dir(&fixture_dir(name)).unwrap();
    Arc::new(PackArchive::from_bytes(&bytes).unwrap())
}

fn body(root: &PackArchive, dependencies: &[Arc<PackArchive>], node_did: &str) -> Vec<u8> {
    encode_request(root, dependencies, |root, dependencies| InstallRequest {
        node_did: node_did.into(),
        root,
        dependencies,
        inference_slots: Vec::new(),
        overwrite: false,
        keep: false,
        preview: false,
        force_rebind: false,
        grant_authority: false,
        explicit: true,
        wire: Vec::new(),
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
        let declared = gents::pack::declared_paths(sent.manifest());
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
fn received_requests_bind_the_runtime_node_and_a_documents_root() {
    let root = archive("documents_fixture");
    assert!(refusal(&body(&root, &[], "did:key:zOtherNode")).contains("different node"));
    assert!(refusal(&body(&archive("assets_fixture"), &[], NODE)).contains("documents pack root"));

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
    assert_eq!(MAX_ARCHIVE_BYTES, gents::pack::archive::MAX_PACK_BYTES);
    assert!(
        MAX_BODY >= base64::encoded_len(MAX_ARCHIVE_BYTES, true).unwrap() + MAX_FIELD_BYTES,
        "the request body admits every archive closure within the shared bound"
    );
}
