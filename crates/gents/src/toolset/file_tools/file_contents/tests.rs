use super::*;
use gents_protocol::file_changes::{FilePermissions, ImageFailure};

fn permissions() -> FilePermissions {
    FilePermissions {
        readonly: false,
        unix_mode: Some(0o644),
    }
}

#[test]
fn hex_round_trips_every_byte() {
    let bytes: Vec<u8> = (0..=255).collect();
    assert_eq!(decode_hex(&encode_hex(&bytes)).unwrap(), bytes);
    assert!(decode_hex("abc").is_err());
    assert!(decode_hex("zz").is_err());
}

#[test]
fn replayed_observation_matches_on_hashes_not_versions() {
    let capture = ToolFileChanges {
        version: gents_protocol::file_changes::FILE_CHANGES_VERSION,
        tool_call_doc_id: "tool".into(),
        changes: vec![FileChangeImages {
            root: "/root".into(),
            path: "a.txt".into(),
            before: FileImage::Missing,
            after: FileImage::File {
                content_hash: content_hash(b"after"),
                data: base64::engine::general_purpose::STANDARD.encode(b"after"),
                permissions: permissions(),
                links: Some(1),
            },
        }],
    };
    let stored = |hash: String, reason: Option<ImageFailure>| StoredFileChanges {
        version: STORED_FILE_CHANGES_VERSION,
        tool_call_doc_id: "tool".into(),
        changes: vec![StoredFileChange {
            root: "/root".into(),
            path: "a.txt".into(),
            content_doc_id: Some("content".into()),
            before: reason.map_or(StoredFileImage::Missing, |reason| {
                StoredFileImage::Unavailable { reason }
            }),
            after: StoredFileImage::File {
                content_hash: hash,
                version: "any-version".into(),
                encoding: FileEncoding::Text,
                permissions: permissions(),
                links: Some(1),
            },
        }],
    };
    assert!(same_observation(
        &stored(content_hash(b"after"), None),
        &capture
    ));
    assert!(!same_observation(
        &stored(content_hash(b"other"), None),
        &capture
    ));
    assert!(!same_observation(
        &stored(content_hash(b"after"), Some(ImageFailure::ReadFailed)),
        &capture
    ));
}

fn edit(tool: &str, before: Option<&[u8]>, after: &[u8]) -> ToolFileChanges {
    let image = |bytes: &[u8]| FileImage::File {
        content_hash: content_hash(bytes),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        permissions: permissions(),
        links: Some(1),
    };
    ToolFileChanges {
        version: gents_protocol::file_changes::FILE_CHANGES_VERSION,
        tool_call_doc_id: tool.into(),
        changes: vec![FileChangeImages {
            root: "/workspace".into(),
            path: "src/lib.rs".into(),
            before: before.map_or(FileImage::Missing, image),
            after: image(after),
        }],
    }
}

#[tokio::test]
async fn observations_in_one_session_have_separate_file_documents_and_read_back_exactly() {
    use crate::tool_call_lifecycle::admission_fixture::{
        published_admission, PublishedAdmissionOptions,
    };
    let admission = published_admission(PublishedAdmissionOptions {
        name: "file-contents-history".into(),
        real_identity: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let scope = SessionScope {
        node_did: admission.node_did.clone(),
        session_id: "session-a".into(),
        requester_did: Some("did:key:requester".into()),
    };
    let first = edit("tool-1", None, b"fn a() {}\n");
    let second = edit("tool-2", Some(b"fn a() {}\n"), b"fn a() {}\nfn b() {}\n");
    let binary = edit(
        "tool-3",
        Some(b"fn a() {}\nfn b() {}\n"),
        &[0, 159, 146, 150, 255],
    );
    let mut stored = Vec::new();
    for changes in [&first, &second, &binary] {
        stored.push(
            ConfigAccess::transact_local_idempotent(
                &admission.node,
                None,
                crate::config_client::IdempotentTransactionRetry::Standard,
                "test.file_contents.store",
                |txn| {
                    let (scope, changes) = (scope.clone(), changes.clone());
                    Box::pin(async move { store(txn, &scope, &changes).await })
                },
            )
            .await
            .unwrap(),
        );
    }
    let documents: Vec<_> = stored
        .iter()
        .map(|stored| stored.changes[0].content_doc_id.clone().unwrap())
        .collect();
    assert_ne!(documents[0], documents[1]);
    assert_ne!(documents[1], documents[2]);
    assert_ne!(documents[0], documents[2]);
    let reader = crate::store::identity::KeyIdentity::load_or_create(
        admission.path.join("review-reader.key"),
        None,
    )
    .unwrap();
    use crate::store::identity::NodeIdentity;
    admission
        .node
        .add_dac_actor_relationship(
            &admission.node_did,
            "ToolFileContent",
            &documents[0],
            "reader",
            reader.did(),
        )
        .await
        .unwrap();
    let actor = defra_node::AcpIdentity::Authenticated(reader.did().parse().unwrap());
    for (index, document) in documents.iter().enumerate() {
        assert_eq!(
            admission
                .node
                .document_acp()
                .check_doc_access(
                    &actor,
                    defra_node::DocumentPermission::Read,
                    gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID,
                    gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE,
                    document,
                )
                .await
                .unwrap(),
            index == 0,
            "a grant for one observation must not authorize another"
        );
    }
    let version = |image: &StoredFileImage| match image {
        StoredFileImage::File {
            version, encoding, ..
        } => (version.clone(), *encoding),
        other => panic!("expected a stored file image: {other:?}"),
    };
    assert_ne!(
        version(&stored[1].changes[0].before).0,
        version(&stored[0].changes[0].after).0,
        "independent observations do not share versions"
    );
    assert_eq!(version(&stored[2].changes[0].after).1, FileEncoding::Blob);
    assert_eq!(version(&stored[2].changes[0].before).1, FileEncoding::Text);

    let access = ConfigAccess::local(admission.node.clone());
    let mut aliased = stored[1].clone();
    let mut second_path = aliased.changes[0].clone();
    second_path.path = "another/path.rs".into();
    aliased.changes.push(second_path);
    assert!(
        resolve(ContentReader::Access(&access), &scope, aliased)
            .await
            .is_err(),
        "a content document cannot be reused under another path"
    );
    let mut crossed = stored[1].clone();
    crossed.changes[0].content_doc_id = Some(documents[0].clone());
    assert!(
        resolve(ContentReader::Access(&access), &scope, crossed)
            .await
            .is_err(),
        "one observation cannot resolve another tool's content"
    );
    for (stored, expected) in stored.into_iter().zip([&first, &second, &binary]) {
        let (resolved, contents) = resolve(ContentReader::Access(&access), &scope, stored.clone())
            .await
            .unwrap();
        assert_eq!(&resolved, expected);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].scope, scope);
        let foreign = SessionScope {
            session_id: "session-b".into(),
            ..scope.clone()
        };
        assert!(resolve(ContentReader::Access(&access), &foreign, stored)
            .await
            .is_err());
    }
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[tokio::test]
async fn concurrent_edits_of_one_path_keep_observation_documents_distinct() {
    use crate::tool_call_lifecycle::admission_fixture::{
        published_admission, PublishedAdmissionOptions,
    };
    let admission = published_admission(PublishedAdmissionOptions {
        name: "file-contents-concurrent".into(),
        real_identity: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let scope = SessionScope {
        node_did: admission.node_did.clone(),
        session_id: "session-a".into(),
        requester_did: None,
    };
    let write = |changes: ToolFileChanges| {
        let (node, scope) = (admission.node.clone(), scope.clone());
        async move {
            ConfigAccess::transact_local_idempotent(
                &node,
                None,
                crate::config_client::IdempotentTransactionRetry::Standard,
                "test.file_contents.concurrent",
                |txn| {
                    let (scope, changes) = (scope.clone(), changes.clone());
                    Box::pin(async move { store(txn, &scope, &changes).await })
                },
            )
            .await
        }
    };
    let left = edit("tool-left", None, b"left\n");
    let right = edit("tool-right", None, b"right\n");
    let (stored_left, stored_right) = tokio::join!(write(left.clone()), write(right.clone()));
    let (stored_left, stored_right) = (stored_left.unwrap(), stored_right.unwrap());
    assert_ne!(
        stored_left.changes[0].content_doc_id,
        stored_right.changes[0].content_doc_id
    );
    let access = ConfigAccess::local(admission.node.clone());
    assert_eq!(
        resolve(ContentReader::Access(&access), &scope, stored_left)
            .await
            .unwrap()
            .0,
        left
    );
    assert_eq!(
        resolve(ContentReader::Access(&access), &scope, stored_right)
            .await
            .unwrap()
            .0,
        right
    );
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[tokio::test]
#[ignore = "manual equivalent-workload retained-storage measurement"]
async fn measure_per_observation_file_history_storage() {
    use crate::store::identity::NodeIdentity;

    fn directory_bytes(path: &std::path::Path) -> std::io::Result<u64> {
        let mut bytes = 0;
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                bytes += directory_bytes(&entry.path())?;
            } else if metadata.is_file() {
                bytes += metadata.len();
            }
        }
        Ok(bytes)
    }

    async fn size(path: std::path::PathBuf) -> u64 {
        tokio::task::spawn_blocking(move || directory_bytes(&path).unwrap())
            .await
            .unwrap()
    }

    async fn measure(root: &std::path::Path, text_versions: bool, revisions: &[String]) -> u64 {
        let key =
            crate::store::identity::KeyIdentity::load_or_create(root.join("identity.key"), None)
                .unwrap();
        let node_did = key.did().to_owned();
        let data = root.join("node");
        let node = defra_node::EmbeddedNode::builder()
            .data_path(&data)
            .with_node_identity_did(&node_did)
            .build()
            .await
            .unwrap();
        crate::ensure_runtime_schemas(&node).await.unwrap();
        node.shutdown().await;
        drop(node);
        let baseline = size(data.clone()).await;
        let node = std::sync::Arc::new(
            defra_node::EmbeddedNode::builder()
                .data_path(&data)
                .with_node_identity_did(&node_did)
                .build()
                .await
                .unwrap(),
        );
        let scope = SessionScope {
            node_did,
            session_id: "measurement-session".into(),
            requester_did: None,
        };
        for (i, pair) in revisions.windows(2).enumerate() {
            let changes = edit(
                &format!("measurement-tool-{i}"),
                Some(pair[0].as_bytes()),
                pair[1].as_bytes(),
            );
            ConfigAccess::transact_local_idempotent(
                &node,
                None,
                crate::config_client::IdempotentTransactionRetry::Standard,
                "test.file_history.measure_storage",
                |txn| {
                    let scope = scope.clone();
                    let changes = changes.clone();
                    Box::pin(async move {
                        let file_changes = if text_versions {
                            serde_json::to_value(store(txn, &scope, &changes).await?)?
                        } else {
                            serde_json::to_value(&changes)?
                        };
                        txn.execute_with_variables(
                            "mutation($input: ToolFileChangeMutationInputArg!) { create_ToolFileChange(input: $input) { _docID } }",
                            &json!({"input": {
                                "tool_call_doc_id": changes.tool_call_doc_id,
                                "request_doc_id": format!("measurement-request-{i}"),
                                "node_did": scope.node_did,
                                "session_id": scope.session_id,
                                "requester_did": scope.requester_did,
                                "file_changes": file_changes,
                            }}),
                        ).await?;
                        Ok::<(), anyhow::Error>(())
                    })
                },
            ).await.unwrap();
        }
        node.shutdown().await;
        drop(node);
        size(data)
            .await
            .checked_sub(baseline)
            .expect("database shrank during measurement")
    }

    let output = std::env::var_os("GENTS_FILE_STORAGE_MEASUREMENT_OUTPUT")
        .expect("set GENTS_FILE_STORAGE_MEASUREMENT_OUTPUT to an output path");
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../gents-migration/src/registry.rs"
    ));
    let source = source.chars().take(32_000).collect::<String>();
    let revisions: Vec<String> = (0..=100)
        .map(|i| format!("{source}\n// measurement revision {i}\n"))
        .collect();
    let directory = tempfile::tempdir().unwrap();
    let inline = measure(&directory.path().join("inline"), false, &revisions).await;
    let per_observation_text = measure(&directory.path().join("text"), true, &revisions).await;
    let result = json!({
        "workload": "100 observations, same 101 UTF-8 revisions capped at 32000 characters",
        "base_source": "crates/gents-migration/src/registry.rs",
        "inline_directory_growth_bytes": inline,
        "per_observation_text_directory_growth_bytes": per_observation_text,
        "measurement_scope": "embedded database files after shutdown; same observation metadata, excludes lifecycle writes",
        "source_revision": std::env::var("GENTS_FILE_STORAGE_REVISION").unwrap_or_default(),
    });
    tokio::task::spawn_blocking(move || {
        std::fs::write(output, serde_json::to_vec_pretty(&result).unwrap())
    })
    .await
    .unwrap()
    .unwrap();
}
