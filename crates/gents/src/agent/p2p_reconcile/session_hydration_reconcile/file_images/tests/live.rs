use super::super::*;
use crate::agent::p2p_reconcile::embedded_impl::push_documents_to_peer;
use crate::agent::p2p_reconcile::embedded_impl::tests::{
    authorize_enrollment_peer, runtime_http_test_node, wait_for_active_peer, wait_for_peer_info,
};
use crate::agent::p2p_reconcile::{
    enrollment_authority_channel, run_enrollment_reconciler, run_pairing_reconciler,
    EmbeddedRemoteP2pAdmin, GraphqlEnrollmentStore, PairingFilters, RemoteP2pAdmin,
};
use crate::identity::KeyIdentity;
use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::tool_call_lifecycle::admission_fixture::{
    ensure_fixture_session, publish_accepted_on_claimed_request,
};
use crate::tool_call_lifecycle::{AwaitMode, VerifiedFileImageReceipt};
use gents_loop::live_output::LiveToolOutputRegistry;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Notify;

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PauseAt {
    Selection,
    Acknowledged,
    Delivered,
}

struct Gate {
    at: PauseAt,
    armed: AtomicBool,
    reached: Notify,
    resume: Notify,
}

impl Gate {
    async fn pause(&self, at: PauseAt) {
        if self.at == at && self.armed.swap(false, Ordering::SeqCst) {
            self.reached.notify_one();
            self.resume.notified().await;
        }
    }
}

struct ObservedStore {
    inner: GraphqlHydrationStore,
    gate: Arc<Gate>,
    checks: Mutex<Vec<bool>>,
}

#[async_trait]
impl ImageRequestStore for ObservedStore {
    async fn prior(&self, row: &ImageDemandRow) -> Result<Option<FileImageHydrationReceipt>> {
        self.inner.prior(row).await
    }
    async fn pending_images(&self) -> Result<Vec<ImageDemandRow>> {
        self.inner.pending_images().await
    }

    async fn admission(&self, demand: &FileImageDemand) -> Result<LoadedHydrationAdmission> {
        self.inner.admission(demand).await
    }

    async fn selection(
        &self,
        demand: &FileImageDemand,
        catalog: &HydrationCatalog,
    ) -> Result<FileImageSelection> {
        let selection = self.inner.selection(demand, catalog).await?;
        self.gate.pause(PauseAt::Selection).await;
        Ok(selection)
    }

    async fn authorized(
        &self,
        demand: &FileImageDemand,
        fence: &EnrollmentAuthorizationFence,
    ) -> Result<bool> {
        let current = self.inner.authorized(demand, fence).await?;
        self.checks.lock().unwrap().push(current);
        Ok(current)
    }

    async fn commit(
        &self,
        row: &ImageDemandRow,
        selection: &FileImageSelection,
        detail: &str,
    ) -> Result<()> {
        self.inner.commit(row, selection, detail).await
    }
}

struct ObservedDelivery {
    inner: EmbeddedHydrationDelivery,
    gate: Arc<Gate>,
    calls: AtomicUsize,
    recipient: Arc<EmbeddedNode>,
    source_did: String,
    expected_changes: Value,
}

#[async_trait]
impl HydrationDelivery for ObservedDelivery {
    async fn push_documents_to_peer(&self, peer: &str, target: DeliveryTarget<'_>) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.push_documents_to_peer(peer, target).await?;
        self.gate.pause(PauseAt::Acknowledged).await;
        if self.gate.at == PauseAt::Delivered {
            let DeliveryTarget::FileImage(image) = target else {
                anyhow::bail!("file review delivered an unexpected collection")
            };
            self.wait_materialized(image).await?;
            self.gate.pause(PauseAt::Delivered).await;
        }
        Ok(())
    }
}

impl ObservedDelivery {
    /// Selective head acknowledgments also cover durable pending DAG imports.
    /// Observe the exact image as its creator before fencing materialized delivery.
    async fn wait_materialized(&self, image: &str) -> Result<()> {
        let query = format!(
            r#"{{ ToolFileChange(docID: "{}") {{ _docID file_changes }} }}"#,
            escape_graphql_string(image),
        );
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let owner = self
                    .recipient
                    .document_acp()
                    .get_doc_owner(
                        gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID,
                        gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE,
                        image,
                    )
                    .await?;
                if let Some(owner) = owner {
                    anyhow::ensure!(
                        owner.to_string() == self.source_did,
                        "imported image has an unexpected creator: {owner}"
                    );
                    let result = ConfigAccess::transact_local_readonly(
                        &self.recipient,
                        Some(self.source_did.parse()?),
                        "test.file_review.observe_import",
                        |txn| {
                            let query = query.clone();
                            Box::pin(async move { txn.execute(&query).await })
                        },
                    )
                    .await?;
                    let rows = result["data"]["ToolFileChange"]
                        .as_array()
                        .context("imported file image rows")?;
                    anyhow::ensure!(rows.len() <= 1, "imported image became ambiguous");
                    if let Some(row) = rows.first() {
                        anyhow::ensure!(
                            row["_docID"] == image && row["file_changes"] == self.expected_changes,
                            "import changed the retained image"
                        );
                        return Ok(());
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("acknowledged file image did not materialize")?
    }
}

async fn admit_on_client(node: &Arc<EmbeddedNode>, demand: &FileImageDemand) -> TestResult<String> {
    let fields = demand
        .reference_fields()
        .into_iter()
        .map(|(name, value)| format!("{name}: \"{}\"", escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    let now = escape_graphql_string(&Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
    let response = ConfigAccess::write_local(
        node,
        "test.file_review.admit",
        &format!(
            r#"mutation {{ create_FileImageHydrationRequest(input: {{
                {fields}, status: "pending", created_at: "{now}"
            }}) {{ _docID }} }}"#
        ),
    )
    .await?;
    Ok(crate::graphql::created_doc_id(
        &response,
        "FileImageHydrationRequest",
    )?)
}

async fn receipt_row(
    node: &Arc<EmbeddedNode>,
    demand: &FileImageDemand,
    status: &str,
) -> TestResult<Value> {
    let query = format!(
        r#"{{ FileImageHydrationRequest(filter: {{ request_key: {{ _eq: "{}" }} }}) {{
            _docID request_key peer_id requester_did node_did session_id request_doc_id tool_call_doc_id
            status status_detail observation_doc_id processed_at outcome_signer_did outcome_signature
        }} }}"#,
        escape_graphql_string(&demand.admission.request_key),
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let result = ConfigAccess::local(node.clone()).execute(&query).await?;
            let rows = result["data"]["FileImageHydrationRequest"]
                .as_array()
                .ok_or("file review demand rows")?;
            assert!(rows.len() <= 1, "file review demand became ambiguous");
            if let Some(row) = rows.first().filter(|row| row["status"] == status) {
                return Ok(row.clone());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|error| format!("file review receipt did not reach its expected state: {error}"))?
}

async fn verified(
    row: &Value,
    demand: &FileImageDemand,
    identity: &dyn NodeIdentity,
) -> TestResult<VerifiedFileImageReceipt> {
    for (name, expected) in demand.reference_fields() {
        assert!(
            row[name].as_str() == Some(expected),
            "receipt changed {name}"
        );
    }
    let selection = match row["status"].as_str() {
        Some("rejected") => {
            assert!(
                row["observation_doc_id"].is_null(),
                "refusal named an image"
            );
            FileImageSelection::Rejected
        }
        Some("served") => match &row["observation_doc_id"] {
            Value::Null => FileImageSelection::Missing,
            Value::String(document) => FileImageSelection::Selected {
                document: document.clone(),
            },
            _ => return Err("invalid image identity".into()),
        },
        _ => return Err("receipt is not terminal".into()),
    };
    let text = |name: &str| -> TestResult<String> {
        Ok(row[name]
            .as_str()
            .ok_or_else(|| format!("receipt omitted {name}"))?
            .into())
    };
    Ok(VerifiedFileImageReceipt::verify(
        FileImageHydrationReceipt {
            version: FILE_IMAGE_RECEIPT_VERSION,
            demand: demand.clone(),
            selection,
            detail: text("status_detail")?,
            processed_at: text("processed_at")?,
            signer_did: text("outcome_signer_did")?,
            signature: bs58::decode(text("outcome_signature")?).into_vec()?,
        },
        demand,
        identity,
    )
    .await?)
}

async fn wait_admitted(store: &GraphqlHydrationStore, demand: &FileImageDemand) -> TestResult<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let facts = store.admission(demand).await?;
            if !matches!(
                decide_hydration(&demand.admission, &facts.catalog),
                HydrationVerdict::Reject(_)
            ) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|error| format!("native enrollment and pairing did not admit file review: {error}"))?
}

async fn exercise_revocation(at: PauseAt, reset_refusal: bool) -> TestResult<()> {
    let directory = tempfile::tempdir()?;
    let source: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(
        directory.path().join("source.key"),
        None,
    )?);
    let client: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(
        directory.path().join("client.key"),
        None,
    )?);
    let (source_test, _) = runtime_http_test_node(source.as_ref()).await;
    let (client_test, _) = runtime_http_test_node(client.as_ref()).await;
    let source_node = source_test.node.clone();
    let client_node = client_test.node.clone();
    crate::test_support::install_test_agent(&source_node, source.did(), "general").await;
    let source_admin = EmbeddedRemoteP2pAdmin::new(source_node.clone());
    let client_admin = EmbeddedRemoteP2pAdmin::new(client_node.clone());
    let source_addresses = wait_for_peer_info(&source_admin).await;
    let client_addresses = wait_for_peer_info(&client_admin).await;
    source_admin.connect(&client_addresses).await?;
    wait_for_active_peer(&source_admin).await;
    wait_for_active_peer(&client_admin).await;
    let client_peer = client_node
        .p2p()
        .ok_or("client P2P")?
        .local_peer_id()
        .await?;
    let enrollment = authorize_enrollment_peer(
        source_node.clone(),
        source.clone(),
        client.clone(),
        &client_peer,
        &client_addresses[0],
    )
    .await;
    let (owner, authority) = enrollment_authority_channel();
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let enrollment_task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
        run_enrollment_reconciler(source_node.clone(), source.clone(), owner, cancel.clone()),
    ));
    let pairing_task =
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(run_pairing_reconciler(
            source_node.clone(),
            source.clone(),
            authority.clone(),
            cancel.clone(),
        )));
    client_admin
        .add_replicator(
            &source_addresses,
            &["FileImageHydrationRequest".into()],
            &PairingFilters::default(),
        )
        .await?;

    let enrollment_store = GraphqlEnrollmentStore::new(source_node.clone(), source.clone());
    let projection = enrollment_store.load_projection().await?;
    let active = projection
        .active
        .iter()
        .find(|entry| entry.request.request_id == enrollment)
        .ok_or("active enrollment")?;
    let session = "recorded-image-fence";
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    ensure_fixture_session(
        &source_node,
        session,
        source.did(),
        Some(client.did()),
        &now,
    )
    .await;
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "recorded-image-fence-request",
        source.did(),
        client.did(),
        "general",
        session,
        "Update the recorded file",
        "interactive",
        now,
        AgentRequestAdmissionRecord::enrollment(
            client.did(),
            &active.request.request_id,
            &active.request.request_digest,
            &active.request.admin_did,
            active.revision.sequence,
            &active.revision.authorization_expires_at,
        ),
    );
    crate::sign_agent_request_create(client.as_ref(), &mut create).await?;
    let response = ConfigAccess::write_local(
        &source_node,
        "test.file_review.request",
        &create.graphql_mutation()?,
    )
    .await?;
    let request_doc = crate::graphql::created_doc_id(&response, "AgentRequest")?;
    let request =
        crate::request_admission::load_request_for_admission_test(&source_node, &request_doc)
            .await?;
    let request = crate::request_admission::AgentRequestAdmissionVerifier::new(
        source_node.clone(),
        source.clone(),
        authority.clone(),
    )
    .verify_fresh(&request, "general")
    .await
    .map_err(|error| format!("{error}"))?;
    let mut request = RequestLifecycle::new_with_node_did(
        source_node.clone(),
        "general",
        source.did(),
        request,
        60,
    );
    assert!(
        request.claim().await? == ClaimOutcome::Claimed,
        "request did not claim"
    );
    let mut tool = publish_accepted_on_claimed_request(
        source_node.clone(), &mut request, source.did(), 0, "write_file", "recorded-image-fence-write",
        json!({"path":"recorded.txt","content":"recorded after\n","overwrite":true,"raw_json":true}),
        AwaitMode::Foreground, true,
    ).await?;
    let physical = tool.doc_id().ok_or("accepted physical tool")?.to_owned();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let workspace = std::fs::canonicalize(workspace)?;
    std::fs::write(workspace.join("recorded.txt"), "recorded before\n")?;
    let output = LiveToolOutputRegistry::default();
    let writer = output.writer_for(physical.clone()).await;
    let mutator = crate::toolset::write_file_for_test(&workspace);
    let result = gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
        None, CancellationToken::new(), None, Some(writer), None, None, Default::default(), false,
        mutator.call(json!({
            "path":"recorded.txt", "content":"recorded after\n", "raw_json":true,
            "overwrite":true,
        }).to_string()),
    ).await?;
    tool.stage_deferred_writes(output.deferred_writes(&physical).await);
    tool.complete(&result).await?;
    std::fs::write(workspace.join("recorded.txt"), "later edits\n")?;
    let access = ConfigAccess::local(source_node.clone());
    let ToolFileChangeRead::Retained {
        document_id: image,
        changes,
    } = load_tool_file_change_observation(
        &access,
        &physical,
        &request_doc,
        source.did(),
        session,
        Some(client.did()),
    )
    .await?
    else {
        return Err("native mutator did not retain its file images".into());
    };
    let mut demand = FileImageDemand {
        admission: HydrationRequest {
            request_key: String::new(),
            peer_id: client_peer.clone(),
            requester_did: client.did().into(),
            node_did: source.did().into(),
            session_id: session.into(),
        },
        request_doc_id: request_doc,
        tool_call_doc_id: physical.clone(),
    };
    demand.admission.request_key = demand.derive_request_key();
    let gate = Arc::new(Gate {
        at,
        armed: AtomicBool::new(true),
        reached: Notify::new(),
        resume: Notify::new(),
    });
    let store = ObservedStore {
        inner: GraphqlHydrationStore {
            node: source_node.clone(),
            enrollment: authority,
            identity: source.clone(),
        },
        gate: gate.clone(),
        checks: Mutex::new(Vec::new()),
    };
    wait_admitted(&store.inner, &demand).await?;
    let demand_doc = admit_on_client(&client_node, &demand).await?;
    let source_peer = source_node
        .p2p()
        .ok_or("source P2P")?
        .local_peer_id()
        .await?;
    push_documents_to_peer(
        &client_node,
        &source_peer,
        vec![defra_p2p_adapter::P2pDocumentRequest {
            collection: "FileImageHydrationRequest".into(),
            doc_id: demand_doc.clone(),
        }],
    )
    .await?;
    receipt_row(&source_node, &demand, "pending").await?;
    let delivery = ObservedDelivery {
        inner: EmbeddedHydrationDelivery {
            node: source_node.clone(),
        },
        gate: gate.clone(),
        calls: AtomicUsize::new(0),
        recipient: client_node.clone(),
        source_did: source.did().into(),
        expected_changes: serde_json::to_value(&changes)?,
    };
    let sweep = reconcile_images(&store, &delivery);
    tokio::pin!(sweep);
    tokio::select! {
        result = &mut sweep => return Err(format!("image sweep returned before the fence: {result:?}").into()),
        _ = gate.reached.notified() => {},
        _ = tokio::time::sleep(Duration::from_secs(20)) => return Err("image sweep did not reach its fence".into()),
    }
    enrollment_store.revoke_request(&enrollment).await?;
    gate.resume.notify_one();
    tokio::time::timeout(Duration::from_secs(20), sweep).await??;
    let checks = store.checks.lock().unwrap().clone();
    assert!(
        checks.len() == 2,
        "missing native authorization fences: {checks:?}"
    );
    let expected = completed_image_selection(
        &FileImageSelection::Selected {
            document: image.clone(),
        },
        checks[0],
        checks[1],
        HydrationDeliveryResult::Confirmed,
        HydrationTerminalWriteResult::Committed,
    )
    .ok_or("model did not produce a terminal outcome")?;
    let refused = receipt_row(&source_node, &demand, "rejected").await?;
    let refusal = verified(&refused, &demand, client.as_ref()).await?;
    assert!(
        refusal.selection() == &expected && expected == FileImageSelection::Rejected,
        "source receipt disagrees with completion owner"
    );
    let owner = client_node
        .document_acp()
        .get_doc_owner(
            gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID,
            gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE,
            &image,
        )
        .await?;
    match at {
        PauseAt::Selection => {
            assert!(
                checks == [false, false],
                "revocation did not reach final fence"
            );
            assert!(
                delivery.calls.load(Ordering::SeqCst) == 0 && owner.is_none(),
                "revocation delivered a previously unseen image"
            );
        }
        PauseAt::Acknowledged => {
            assert!(
                checks == [true, false] && delivery.calls.load(Ordering::SeqCst) > 0,
                "post-acknowledgment revocation was not observed"
            );
            if let Some(owner) = owner {
                assert!(
                    owner.to_string() == source.did(),
                    "acknowledged image changed native creator ownership: {owner}"
                );
            }
        }
        PauseAt::Delivered => {
            assert!(
                checks == [true, false],
                "post-delivery revocation was not observed"
            );
            assert!(
                delivery.calls.load(Ordering::SeqCst) > 0
                    && owner.as_ref().map(ToString::to_string).as_deref() == Some(source.did()),
                "delivery did not preserve native creator ownership: {owner:?}"
            );
        }
    }
    assert!(
        !refusal.grant_local_read(client_node.clone()).await?,
        "refusal granted local read"
    );
    let visible = ConfigAccess::local(client_node.clone())
        .execute("{ ToolFileChange { _docID } }")
        .await?;
    assert!(
        visible["data"]["ToolFileChange"]
            .as_array()
            .ok_or("image visibility")?
            .is_empty(),
        "client read an image without a served receipt"
    );

    let restored = authorize_enrollment_peer(
        source_node.clone(),
        source.clone(),
        client.clone(),
        &client_peer,
        &client_addresses[0],
    )
    .await;
    assert!(
        restored != enrollment,
        "reenrollment reused the revoked request"
    );
    if reset_refusal {
        wait_admitted(&store.inner, &demand).await?;
        let client_refusal = receipt_row(&client_node, &demand, "rejected").await?;
        assert!(
            client_refusal == refused,
            "client did not receive the refusal"
        );
        ConfigAccess::write_local(
            &client_node,
            "test.file_review.reset_refusal",
            &format!(
                r#"mutation {{ update_FileImageHydrationRequest(docID: "{}", input: {{
                    status: "pending", status_detail: null, observation_doc_id: null,
                    processed_at: null, outcome_signer_did: null, outcome_signature: null
                }}) {{ _docID }} }}"#,
                escape_graphql_string(&demand_doc),
            ),
        )
        .await?;
        push_documents_to_peer(
            &client_node,
            &source_peer,
            vec![defra_p2p_adapter::P2pDocumentRequest {
                collection: "FileImageHydrationRequest".into(),
                doc_id: demand_doc.clone(),
            }],
        )
        .await?;
        receipt_row(&source_node, &demand, "pending").await?;
        let deliveries_before_replay = delivery.calls.load(Ordering::SeqCst);
        let checks_before_replay = store.checks.lock().unwrap().len();
        reconcile_images(&store, &delivery).await?;
        assert!(
            delivery.calls.load(Ordering::SeqCst) == deliveries_before_replay
                && store.checks.lock().unwrap().len() == checks_before_replay,
            "replaying a recorded refusal reauthorized or redelivered its attempt"
        );
        let result = ConfigAccess::local(source_node.clone())
            .execute(&format!(
                r#"{{ FileImageHydrationRequest(docID: "{}") {{
                    _docID request_key peer_id requester_did node_did session_id request_doc_id tool_call_doc_id
                    status status_detail observation_doc_id processed_at outcome_signer_did outcome_signature
                }} }}"#,
                escape_graphql_string(&demand_doc),
            ))
            .await?;
        assert!(
            result["data"]["FileImageHydrationRequest"][0] == refused,
            "resetting a client-owned demand replaced its signed refusal: {}",
            result["data"]["FileImageHydrationRequest"][0],
        );
    }
    let mut retry = demand.clone();
    retry.admission.request_key = retry.derive_retry_request_key(&uuid::Uuid::new_v4().to_string());
    wait_admitted(&store.inner, &retry).await?;
    let retry_doc = admit_on_client(&client_node, &retry).await?;
    push_documents_to_peer(
        &client_node,
        &source_peer,
        vec![defra_p2p_adapter::P2pDocumentRequest {
            collection: "FileImageHydrationRequest".into(),
            doc_id: retry_doc,
        }],
    )
    .await?;
    receipt_row(&source_node, &retry, "pending").await?;
    reconcile_images(&store, &delivery).await?;
    let served = receipt_row(&client_node, &retry, "served").await?;
    let receipt = verified(&served, &retry, client.as_ref()).await?;
    assert!(
        receipt.selection()
            == &FileImageSelection::Selected {
                document: image.clone()
            },
        "retry selected another observation"
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if receipt.grant_local_read(client_node.clone()).await? {
                return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|error| format!("served retry image did not become readable: {error}"))??;
    let retained = ConfigAccess::local(client_node.clone()).execute(&format!(
        r#"{{ ToolFileChange(docID: "{}") {{ _docID tool_call_doc_id request_doc_id node_did session_id requester_did file_changes }} }}"#,
        escape_graphql_string(&image),
    )).await?;
    let rows = retained["data"]["ToolFileChange"]
        .as_array()
        .ok_or("retained images")?;
    assert!(
        rows.len() == 1
            && rows[0]["_docID"] == image
            && rows[0]["tool_call_doc_id"] == physical
            && rows[0]["request_doc_id"] == retry.request_doc_id
            && rows[0]["node_did"] == source.did()
            && rows[0]["requester_did"] == client.did()
            && rows[0]["session_id"] == session
            && rows[0]["file_changes"] == serde_json::to_value(&changes)?,
        "retry changed retained file images or their exact scope"
    );
    assert!(
        receipt_row(&source_node, &demand, "rejected").await? == refused,
        "reenrollment replaced the signed refusal"
    );
    let client_refusal = receipt_row(&client_node, &demand, "rejected").await?;
    assert!(
        client_refusal == refused && client_refusal["_docID"] == demand_doc,
        "source refusal did not preserve its client-authored demand"
    );
    assert!(
        std::fs::read_to_string(workspace.join("recorded.txt"))? == "later edits\n",
        "review changed the working copy"
    );
    cancel.cancel();
    enrollment_task.await??;
    pairing_task.await??;
    client_node.shutdown().await;
    source_node.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revocation_after_selection_prevents_unseen_image_delivery() -> TestResult<()> {
    tokio::time::timeout(
        Duration::from_secs(120),
        exercise_revocation(PauseAt::Selection, false),
    )
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revocation_after_delivery_preserves_refusal_until_a_fresh_authorized_retry(
) -> TestResult<()> {
    tokio::time::timeout(
        Duration::from_secs(120),
        exercise_revocation(PauseAt::Delivered, false),
    )
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resetting_a_client_demand_cannot_replace_a_signed_refusal() -> TestResult<()> {
    tokio::time::timeout(
        Duration::from_secs(120),
        exercise_revocation(PauseAt::Selection, true),
    )
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revocation_after_acknowledgment_preserves_refusal_until_a_fresh_retry() -> TestResult<()> {
    tokio::time::timeout(
        Duration::from_secs(120),
        exercise_revocation(PauseAt::Acknowledged, false),
    )
    .await?
}
