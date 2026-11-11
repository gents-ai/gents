use super::*;
use gents_protocol::file_changes::{FilePermissions, ImageFailure};

pub(crate) fn permissions() -> FilePermissions {
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

pub fn edit(tool: &str, before: Option<&[u8]>, after: &[u8]) -> ToolFileChanges {
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
#[ignore = "manual equivalent-workload retained-storage measurement"]
async fn measure_per_observation_file_history_storage() {
    use gents_store::identity::NodeIdentity;

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
            gents_store::identity::KeyIdentity::load_or_create(root.join("identity.key"), None)
                .unwrap();
        let node_did = key.did().to_owned();
        let data = root.join("node");
        let node = defra_node::Node::builder()
            .data_path(&data)
            .with_node_identity_did(&node_did)
            .build()
            .await
            .unwrap();
        gents_store::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        node.shutdown().await.expect("shut down test node");
        drop(node);
        let baseline = size(data.clone()).await;
        let node = std::sync::Arc::new(
            defra_node::Node::builder()
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
                gents_store::config_client::IdempotentTransactionRetry::Standard,
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
        node.shutdown().await.expect("shut down test node");
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
