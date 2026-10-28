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
async fn edits_in_one_session_share_a_file_document_and_read_back_exactly() {
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
    assert!(documents.iter().all(|document| *document == documents[0]));
    let version = |image: &StoredFileImage| match image {
        StoredFileImage::File {
            version, encoding, ..
        } => (version.clone(), *encoding),
        other => panic!("expected a stored file image: {other:?}"),
    };
    assert_eq!(
        version(&stored[1].changes[0].before),
        version(&stored[0].changes[0].after),
        "an unchanged before image reuses the prior version"
    );
    assert_eq!(version(&stored[2].changes[0].after).1, FileEncoding::Blob);
    assert_eq!(version(&stored[2].changes[0].before).1, FileEncoding::Text);

    let access = ConfigAccess::local(admission.node.clone());
    for (stored, expected) in stored.into_iter().zip([&first, &second, &binary]) {
        let (resolved, contents) = resolve(&access, &scope, stored.clone()).await.unwrap();
        assert_eq!(&resolved, expected);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].scope, scope);
        let foreign = SessionScope {
            session_id: "session-b".into(),
            ..scope.clone()
        };
        assert!(resolve(&access, &foreign, stored).await.is_err());
    }
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[tokio::test]
async fn concurrent_first_edits_of_one_path_converge_on_one_file_document() {
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
    assert_eq!(
        stored_left.changes[0].content_doc_id,
        stored_right.changes[0].content_doc_id
    );
    let access = ConfigAccess::local(admission.node.clone());
    assert_eq!(resolve(&access, &scope, stored_left).await.unwrap().0, left);
    assert_eq!(
        resolve(&access, &scope, stored_right).await.unwrap().0,
        right
    );
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}
