mod live;
mod storage;

use super::*;
use crate::agent::p2p_reconcile::file_image_hydration::image_delivery_target;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex,
};

struct Store {
    demand: ImageDemandRow,
    admission: LoadedHydrationAdmission,
    selection: FileImageSelection,
    authorized: Vec<bool>,
    checks: AtomicUsize,
    fail_write: AtomicBool,
    committed: Mutex<Vec<FileImageSelection>>,
}

#[async_trait]
impl ImageRequestStore for Store {
    async fn prior(&self, _: &ImageDemandRow) -> Result<Option<FileImageHydrationReceipt>> {
        Ok(None)
    }
    async fn pending_images(&self) -> Result<Vec<ImageDemandRow>> {
        Ok(vec![self.demand.clone()])
    }
    async fn admission(&self, _: &FileImageDemand) -> Result<LoadedHydrationAdmission> {
        Ok(self.admission.clone())
    }
    async fn selection(
        &self,
        _: &FileImageDemand,
        _: &HydrationCatalog,
    ) -> Result<(FileImageSelection, Vec<String>)> {
        Ok((self.selection.clone(), Vec::new()))
    }
    async fn authorized(
        &self,
        _: &FileImageDemand,
        _: &EnrollmentAuthorizationFence,
    ) -> Result<bool> {
        Ok(self.authorized[self.checks.fetch_add(1, Ordering::SeqCst)])
    }
    async fn commit(
        &self,
        row: &ImageDemandRow,
        selection: &FileImageSelection,
        _: &str,
    ) -> Result<()> {
        assert_eq!(row, &self.demand);
        anyhow::ensure!(
            !self.fail_write.load(Ordering::SeqCst),
            "receipt write failed"
        );
        self.committed.lock().unwrap().push(selection.clone());
        Ok(())
    }
}

struct Delivery {
    attempts: AtomicUsize,
    fail: bool,
    pushed: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl HydrationDelivery for Delivery {
    async fn push_documents_to_peer(&self, peer: &str, target: DeliveryTarget<'_>) -> Result<()> {
        let DeliveryTarget::FileImage(document, _) = target else {
            panic!("unexpected transcript delivery")
        };
        self.attempts.fetch_add(1, Ordering::SeqCst);
        anyhow::ensure!(!self.fail, "transport failed");
        self.pushed
            .lock()
            .unwrap()
            .push((peer.to_owned(), document.to_owned()));
        Ok(())
    }
}

fn fixture(selection: FileImageSelection, authorized: Vec<bool>) -> (Store, Delivery) {
    let demand = FileImageDemand {
        admission: HydrationRequest {
            request_key: "key".into(),
            peer_id: "peer".into(),
            requester_did: "requester".into(),
            node_did: "node".into(),
            session_id: "session".into(),
        },
        request_doc_id: "request".into(),
        tool_call_doc_id: "tool".into(),
    };
    let catalog = HydrationCatalog {
        applied_pairing_routes: BTreeSet::from([AppliedPairingRoute {
            peer_id: "peer".into(),
            requester_did: "requester".into(),
            node_did: "node".into(),
        }]),
        selected_network_id: "network".into(),
        verified_active_memberships: BTreeSet::from([VerifiedActiveMembership {
            network_id: "network".into(),
            member_did: "requester".into(),
        }]),
        sessions: BTreeSet::from([SessionOwner {
            session_id: "session".into(),
            requester_did: "requester".into(),
            node_did: "node".into(),
        }]),
        ..Default::default()
    };
    let fence = EnrollmentAuthorizationFence {
        network_id: "network".into(),
        request_id: "enrollment".into(),
        admin_did: "admin".into(),
        member_did: "requester".into(),
        member_peer: "peer".into(),
        member_ticket: "ticket".into(),
        owner_node: "node".into(),
        profile: "client".into(),
        request_digest: "digest".into(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-01-01T00:00:00Z".into(),
    };
    (
        Store {
            demand: ImageDemandRow {
                document_id: "demand-doc".into(),
                demand,
            },
            admission: LoadedHydrationAdmission {
                catalog,
                authorization: Some(fence),
            },
            selection,
            authorized,
            checks: AtomicUsize::new(0),
            fail_write: AtomicBool::new(false),
            committed: Mutex::new(Vec::new()),
        },
        Delivery {
            attempts: AtomicUsize::new(0),
            fail: false,
            pushed: Mutex::new(Vec::new()),
        },
    )
}

#[tokio::test]
async fn file_image_delivery_and_receipts_follow_the_completion_contract() {
    for selection in [
        FileImageSelection::Selected {
            document: "exact-image".into(),
        },
        FileImageSelection::Missing,
        FileImageSelection::Rejected,
    ] {
        for before in [false, true] {
            for after in [false, true] {
                for fail_write in [false, true] {
                    let (store, delivery) = fixture(selection.clone(), vec![before, after]);
                    store.fail_write.store(fail_write, Ordering::SeqCst);
                    let result = reconcile_images(&store, &delivery).await;
                    assert_eq!(result.is_err(), fail_write);
                    let expected = completed_image_selection(
                        &selection,
                        before,
                        after,
                        HydrationDeliveryResult::Confirmed,
                        if fail_write {
                            HydrationTerminalWriteResult::Failed
                        } else {
                            HydrationTerminalWriteResult::Committed
                        },
                    );
                    assert_eq!(
                        *store.committed.lock().unwrap(),
                        expected.into_iter().collect::<Vec<_>>()
                    );
                    let targets = image_delivery_target(&selection, before)
                        .map(|doc| ("peer".into(), doc.into()));
                    assert_eq!(
                        *delivery.pushed.lock().unwrap(),
                        targets.into_iter().collect::<Vec<_>>()
                    );
                }
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn indeterminate_image_transport_leaves_the_receipt_pending() {
    let (store, mut delivery) = fixture(
        FileImageSelection::Selected {
            document: "exact-image".into(),
        },
        vec![true],
    );
    delivery.fail = true;
    reconcile_images(&store, &delivery).await.unwrap();
    assert_eq!(
        delivery.attempts.load(Ordering::SeqCst),
        HYDRATION_DELIVERY_MAX_ATTEMPTS
    );
    assert_eq!(store.checks.load(Ordering::SeqCst), 1);
    assert!(store.committed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_unpaired_image_demand_cannot_reach_delivery() {
    let (mut store, delivery) = fixture(
        FileImageSelection::Selected {
            document: "exact-image".into(),
        },
        vec![],
    );
    store.admission.catalog.applied_pairing_routes.clear();
    reconcile_images(&store, &delivery).await.unwrap();
    assert_eq!(store.checks.load(Ordering::SeqCst), 0);
    assert!(delivery.pushed.lock().unwrap().is_empty());
    assert_eq!(
        *store.committed.lock().unwrap(),
        vec![FileImageSelection::Rejected]
    );
}

#[tokio::test]
async fn signed_image_receipts_commit_only_to_the_exact_pending_demand() {
    use crate::identity::KeyIdentity;
    let root = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(root.path().join("node.key"), None).unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(root.path().join("db"))
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let access = ConfigAccess::local(node.clone());
    let (mut store, _) = fixture(FileImageSelection::Missing, vec![]);
    store.demand.demand.admission.node_did = identity.did().into();
    let demand = &store.demand.demand;
    let fields = [
        ("request_key", &demand.admission.request_key),
        ("peer_id", &demand.admission.peer_id),
        ("requester_did", &demand.admission.requester_did),
        ("node_did", &demand.admission.node_did),
        ("session_id", &demand.admission.session_id),
        ("request_doc_id", &demand.request_doc_id),
        ("tool_call_doc_id", &demand.tool_call_doc_id),
    ]
    .into_iter()
    .map(|(key, value)| format!("{key}: \"{}\"", escape_graphql_string(value)))
    .collect::<Vec<_>>()
    .join(", ");
    let response = access.write("test.file_image.demand", &format!(r#"mutation {{
        create_FileImageHydrationRequest(input: {{ {fields}, status: "pending", created_at: "2026-10-20T00:00:00Z" }}) {{ _docID }}
    }}"#)).await.unwrap();
    store.demand.document_id =
        crate::graphql::created_doc_id(&response, "FileImageHydrationRequest").unwrap();
    let mut receipt = FileImageHydrationReceipt {
        version: FILE_IMAGE_RECEIPT_VERSION,
        demand: store.demand.demand.clone(),
        selection: FileImageSelection::Selected {
            document: "image\"\\id".into(),
        },
        detail: String::new(),
        processed_at: "2026-10-20T00:00:01Z".into(),
        signer_did: identity.did().into(),
        signature: Vec::new(),
    };
    receipt.signature = identity
        .sign(&receipt.signing_payload().unwrap())
        .await
        .unwrap();
    receipt.validate_shape().unwrap();
    assert!(identity
        .verify(
            &receipt.signer_did,
            &receipt.signing_payload().unwrap(),
            &receipt.signature
        )
        .await
        .unwrap());
    let mut tampered = receipt.clone();
    tampered.demand.tool_call_doc_id.push_str("-other");
    assert!(!identity
        .verify(
            &tampered.signer_did,
            &tampered.signing_payload().unwrap(),
            &tampered.signature
        )
        .await
        .unwrap_or(false));
    let mut wrong_row = store.demand.clone();
    wrong_row.document_id.push_str("-other");
    let result = access
        .write(
            "test.file_image.wrong_cas",
            &terminal_image_mutation(&wrong_row, &receipt).unwrap(),
        )
        .await
        .unwrap();
    assert!(result["data"]["update_FileImageHydrationRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    let mutation = terminal_image_mutation(&store.demand, &receipt).unwrap();
    let result = access
        .write("test.file_image.commit", &mutation)
        .await
        .unwrap();
    assert_eq!(
        result["data"]["update_FileImageHydrationRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let result = access
        .write("test.file_image.replay", &mutation)
        .await
        .unwrap();
    assert!(result["data"]["update_FileImageHydrationRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    let saved = access
        .execute("{ FileImageHydrationRequest { status observation_doc_id outcome_signature } }")
        .await
        .unwrap();
    assert_eq!(
        saved["data"]["FileImageHydrationRequest"][0]["observation_doc_id"],
        "image\"\\id"
    );
    assert_eq!(
        saved["data"]["FileImageHydrationRequest"][0]["outcome_signature"],
        bs58::encode(&receipt.signature).into_string()
    );
    node.shutdown().await;
}
