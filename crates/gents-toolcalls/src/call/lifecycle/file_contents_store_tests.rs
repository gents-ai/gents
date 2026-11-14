use gents_protocol::file_changes::{FileEncoding, StoredFileImage, ToolFileChanges};
use gents_protocol::session::SessionScope;
use gents_session::file_contents::tests::edit;
use gents_session::file_contents::{resolve, store, ContentReader};
use gents_store::config_client::ConfigAccess;

#[tokio::test]
async fn observations_in_one_session_have_separate_file_documents_and_read_back_exactly() {
    use crate::call::lifecycle::admission_fixture::{
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
                gents_store::config_client::IdempotentTransactionRetry::Standard,
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
    let reader = gents_store::identity::KeyIdentity::load_or_create(
        admission.path.join("review-reader.key"),
        None,
    )
    .unwrap();
    use gents_store::identity::NodeIdentity;
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
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[tokio::test]
async fn concurrent_edits_of_one_path_keep_observation_documents_distinct() {
    use crate::call::lifecycle::admission_fixture::{
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
                gents_store::config_client::IdempotentTransactionRetry::Standard,
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
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(admission.path).unwrap();
}
