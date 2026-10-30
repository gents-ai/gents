use super::super::*;
use crate::agent::p2p_reconcile::embedded_impl::push_documents_to_peer;
use crate::agent::p2p_reconcile::embedded_impl::tests::fixtures::{
    authorize_enrollment_peer, runtime_http_test_node, runtime_http_test_node_with_schema,
    wait_for_active_peer, wait_for_peer_info,
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
    ) -> Result<(FileImageSelection, Vec<String>)> {
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
            let DeliveryTarget::FileImage(image, _) = target else {
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

async fn admit_on_client(node: &Arc<EmbeddedNode>, demand: &FileImageDemand) -> Result<String> {
    let mut input: serde_json::Map<String, Value> = demand
        .reference_fields()
        .into_iter()
        .map(|(field, value)| (field.to_owned(), Value::String(value.to_owned())))
        .collect();
    if let Some(reference) = &demand.inventory_reference {
        input.insert(
            "inventory_reference".into(),
            serde_json::to_value(reference)?,
        );
    }
    input.insert("created_at".into(), Value::String(Utc::now().to_rfc3339()));
    input.insert("status".into(), Value::String("pending".into()));
    let response = ConfigAccess::transact_local_write(node, None, "test.file_review.admit", |txn| {
        let input = &input;
        Box::pin(async move {
            txn.execute_with_variables(
                &format!("mutation($input: {0}MutationInputArg!) {{ create_FileImageHydrationRequest: create_{0}(input: $input) {{ _docID }} }}", demand.collection_name()),
                &json!({"input": input}),
            ).await
        })
    }).await?;
    Ok(crate::graphql::created_doc_id(
        &response,
        "FileImageHydrationRequest",
    )?)
}

async fn receipt_row(
    node: &Arc<EmbeddedNode>,
    demand: &FileImageDemand,
    status: &str,
) -> Result<Value> {
    let query = format!(
        r#"{{ FileImageHydrationRequest: {}(filter: {{ request_key: {{ _eq: "{}" }} }}) {{
            _docID request_key peer_id requester_did node_did session_id request_doc_id tool_call_doc_id {}
            status status_detail observation_doc_id processed_at outcome_signer_did outcome_signature
        }} }}"#,
        demand.collection_name(),
        escape_graphql_string(&demand.admission.request_key),
        if demand.inventory_reference.is_some() {
            "inventory_reference"
        } else {
            ""
        },
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let result = ConfigAccess::local(node.clone()).execute(&query).await?;
            let rows = result["data"]["FileImageHydrationRequest"]
                .as_array()
                .context("file review demand rows")?;
            anyhow::ensure!(rows.len() <= 1, "file review demand became ambiguous");
            if let Some(row) = rows.first().filter(|row| row["status"] == status) {
                return Ok(row.clone());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("file review receipt did not reach its expected state")?
}

async fn verified(
    row: &Value,
    demand: &FileImageDemand,
    identity: &dyn NodeIdentity,
) -> Result<VerifiedFileImageReceipt> {
    for (name, expected) in demand.reference_fields() {
        anyhow::ensure!(
            row[name].as_str() == Some(expected),
            "receipt changed {name}"
        );
    }
    anyhow::ensure!(
        serde_json::from_value::<Option<FileImageInventoryReference>>(
            row["inventory_reference"].clone()
        )? == demand.inventory_reference,
        "receipt changed its signed inventory reference"
    );
    let selection = match row["status"].as_str() {
        Some("rejected") => {
            anyhow::ensure!(
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
            _ => anyhow::bail!("invalid image identity"),
        },
        _ => anyhow::bail!("receipt is not terminal"),
    };
    let text = |name: &str| -> Result<String> {
        Ok(row[name]
            .as_str()
            .with_context(|| format!("receipt omitted {name}"))?
            .into())
    };
    VerifiedFileImageReceipt::verify(
        FileImageHydrationReceipt {
            version: demand.receipt_version(),
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
    .await
}

async fn wait_admitted(store: &GraphqlHydrationStore, demand: &FileImageDemand) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let facts = store.admission(demand).await?;
            let verdict = match &demand.inventory_reference {
                Some(reference) => {
                    crate::agent::p2p_reconcile::session_hydration::decide_inventory_hydration(
                        &reference.page.admission,
                        &facts.catalog,
                    )
                }
                None => decide_hydration(&demand.admission, &facts.catalog),
            };
            if !matches!(verdict, HydrationVerdict::Reject(_)) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("native enrollment and pairing did not admit file review")?
}

async fn exercise_revocation(at: PauseAt, reset_refusal: bool, shared: bool) -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(
        directory.path().join("source.key"),
        None,
    )?);
    let client: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(
        directory.path().join("client.key"),
        None,
    )?);
    let retained_schema = gents_protocol::schemas::FILE_IMAGE_HYDRATION_REQUEST
        .lines()
        .filter(|line| !line.trim_start().starts_with("inventory_reference:"))
        .collect::<Vec<_>>()
        .join("\n");
    let (source_test, _) = runtime_http_test_node_with_schema(
        source.as_ref(),
        shared.then_some(retained_schema.as_str()),
    )
    .await;
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
        .context("client P2P")?
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
    let author: Arc<dyn NodeIdentity> = if shared {
        let key = directory.path().join("author.key");
        Arc::new(
            tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(key, None)).await??,
        )
    } else {
        client.clone()
    };
    let author_test = if shared {
        Some(runtime_http_test_node(author.as_ref()).await.0)
    } else {
        None
    };
    let author_enrollment = if let Some(author_test) = &author_test {
        let author_admin = EmbeddedRemoteP2pAdmin::new(author_test.node.clone());
        let addresses = wait_for_peer_info(&author_admin).await;
        source_admin.connect(&addresses).await?;
        wait_for_active_peer(&author_admin).await;
        let peer = author_test
            .node
            .p2p()
            .context("author P2P")?
            .local_peer_id()
            .await?;
        authorize_enrollment_peer(
            source_node.clone(),
            source.clone(),
            author.clone(),
            &peer,
            &addresses[0],
        )
        .await
    } else {
        enrollment.clone()
    };
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
            &[
                "FileImageHydrationRequest".into(),
                "FileInventoryImageRequest".into(),
            ],
            &PairingFilters::default(),
        )
        .await?;

    let enrollment_store = GraphqlEnrollmentStore::new(source_node.clone(), source.clone());
    let projection = enrollment_store.load_projection().await?;
    let active = projection
        .active
        .iter()
        .find(|entry| entry.request.request_id == author_enrollment)
        .context("active enrollment")?;
    let session = "recorded-image-fence";
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    ensure_fixture_session(
        &source_node,
        session,
        source.did(),
        Some(author.did()),
        &now,
    )
    .await;
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "recorded-image-fence-request",
        source.did(),
        author.did(),
        "general",
        session,
        "Update the recorded file",
        "interactive",
        now,
        AgentRequestAdmissionRecord::enrollment(
            author.did(),
            &active.request.request_id,
            &active.request.request_digest,
            &active.request.admin_did,
            active.revision.sequence,
            &active.revision.authorization_expires_at,
        ),
    );
    crate::sign_agent_request_create(author.as_ref(), &mut create).await?;
    let response = ConfigAccess::write_local(
        &source_node,
        "test.file_review.request",
        &create.graphql_mutation().map_err(anyhow::Error::msg)?,
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
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut request = RequestLifecycle::new_with_node_did(
        source_node.clone(),
        "general",
        source.did(),
        request,
        60,
    );
    anyhow::ensure!(
        request.claim().await? == ClaimOutcome::Claimed,
        "request did not claim"
    );
    let mut tool = publish_accepted_on_claimed_request(
        source_node.clone(), &mut request, source.did(), 0, "write_file", "recorded-image-fence-write",
        json!({"path":"recorded.txt","content":"recorded after\n","overwrite":true,"raw_json":true}),
        AwaitMode::Foreground, true,
    ).await?;
    let physical = tool.doc_id().context("accepted physical tool")?.to_owned();
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
        ..
    } = load_tool_file_change_observation(
        &access,
        &physical,
        &request_doc,
        source.did(),
        session,
        Some(author.did()),
    )
    .await?
    else {
        anyhow::bail!("native mutator did not retain its file images")
    };
    let mut demand = FileImageDemand {
        inventory_reference: None,
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
    let mut received_entry = None;
    if shared {
        let metadata = access.execute(&format!(
            r#"{{ ToolFileInventory(filter: {{tool_call_doc_id: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            escape_graphql_string(&physical),
        )).await?;
        let projections = metadata["data"]["ToolFileInventory"]
            .as_array()
            .context("metadata rows")?;
        anyhow::ensure!(projections.len() == 1, "ambiguous metadata");
        source_node
            .add_dac_actor_relationship(
                source.did(),
                "ToolFileInventory",
                projections[0]["_docID"].as_str().context("metadata ID")?,
                "reader",
                client.did(),
            )
            .await?;
        let root = ConfigAccess::transact_local_readonly(
            &source_node,
            None,
            "test.shared_image.root",
            |txn| {
                Box::pin(crate::session::load_agent_session_owner_in_txn(
                    txn,
                    source.did(),
                    session,
                ))
            },
        )
        .await?
        .context("shared root")?;
        push_documents_to_peer(
            &source_node,
            &client_peer,
            vec![defra_p2p_adapter::P2pDocumentRequest {
                collection: "AgentSession".into(),
                doc_id: root.doc_id.clone(),
            }],
        )
        .await?;
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let received = ConfigAccess::transact_local_readonly(
                    &client_node,
                    None,
                    "test.shared_image.received_session",
                    |txn| {
                        Box::pin(crate::session::load_agent_session_owner_in_txn(
                            txn,
                            source.did(),
                            session,
                        ))
                    },
                )
                .await?;
                if let Some(received) = received {
                    anyhow::ensure!(
                        received.doc_id == root.doc_id
                            && received.session.requester_did.as_deref() == Some(author.did()),
                        "received session changed its physical root or author"
                    );
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        let mut root_request = demand.admission.clone();
        root_request.request_key = "shared-metadata".into();
        let page_demand = gents_protocol::file_inventory::FileInventoryPageDemand {
            admission: gents_protocol::file_inventory::FileInventoryAdmission {
                request: root_request,
                session_doc_id: root.doc_id,
            },
            attempt_id: "shared-metadata-attempt".into(),
            cursor: None,
        };
        let source_store = GraphqlHydrationStore {
            node: source_node.clone(),
            enrollment: authority.clone(),
            identity: source.clone(),
        };
        let metadata_admission = source_store.inventory_admission(&page_demand).await?;
        let metadata_fence = metadata_admission
            .authorization
            .context("shared metadata enrollment")?;
        let snapshot = FileInventorySnapshot::load(&source_node, &page_demand.admission).await?;
        let page = crate::file_inventory::sign_current_page(
            source.as_ref(),
            &page_demand.derive_request_key()?,
            snapshot.page(None)?,
            || source_store.inventory_current(&page_demand, &metadata_fence),
        )
        .await?;
        let prepared = crate::file_inventory::delivery::prepare_page(
            &source_node,
            &page_demand,
            &page,
            source.as_ref(),
        )
        .await?;
        let grant = crate::file_inventory::delivery::sign_page_grant(
            source.as_ref(),
            &page_demand,
            &prepared,
        )
        .await?;
        let verified_page = crate::file_inventory::VerifiedInventoryResponse::verify(
            gents_protocol::file_inventory::FileInventoryResponse::Page(grant),
            &page_demand,
            client.as_ref(),
        )
        .await?;
        EmbeddedHydrationDelivery {
            node: source_node.clone(),
        }
        .push_documents_to_peer(
            &client_peer,
            DeliveryTarget::FileInventory(&prepared.document_id),
        )
        .await?;
        let received = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(page) = verified_page
                    .read_page(client_node.clone(), client.as_ref())
                    .await?
                {
                    return Ok::<_, anyhow::Error>(page);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        anyhow::ensure!(
            received == page,
            "received metadata changed its signed snapshot"
        );
        received_entry = Some(received.page.entries[0].clone());
        demand.inventory_reference = Some(FileImageInventoryReference {
            page: page_demand,
            snapshot_digest: page.page.binding.digest,
            entry_index: 0,
        });
        demand.admission.request_key = demand.derive_request_key();
        let facts = source_store.admission(&demand).await?;
        anyhow::ensure!(
            source_store.selection(&demand, &facts.catalog).await?.0
                == FileImageSelection::Rejected,
            "metadata access granted image bytes"
        );
        source_node
            .add_dac_actor_relationship(
                source.did(),
                "ToolFileChange",
                &image,
                "reader",
                client.did(),
            )
            .await?;
        source_node
            .delete_dac_actor_relationship(
                source.did(),
                "ToolFileChange",
                &image,
                "reader",
                client.did(),
            )
            .await?;
        anyhow::ensure!(
            source_store.selection(&demand, &facts.catalog).await?.0
                == FileImageSelection::Rejected,
            "revocation between metadata and image selection retained read authority"
        );

        let mut forged = demand.clone();
        forged.admission.requester_did = author.did().into();
        forged
            .inventory_reference
            .as_mut()
            .unwrap()
            .page
            .admission
            .request
            .requester_did = author.did().into();
        forged.admission.request_key = forged.derive_request_key();
        let forged_admission = source_store.admission(&forged).await?;
        anyhow::ensure!(
            forged_admission.authorization.is_none(),
            "another enrolled author was authenticated on the reader's peer"
        );
        anyhow::ensure!(
            source_store
                .selection(&forged, &forged_admission.catalog)
                .await?
                .0
                == FileImageSelection::Rejected,
            "a forged requester borrowed the reader's signed metadata"
        );

        source_node
            .add_dac_actor_relationship(
                source.did(),
                "ToolFileChange",
                &image,
                "reader",
                client.did(),
            )
            .await?;
    }
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
        .context("source P2P")?
        .local_peer_id()
        .await?;
    push_documents_to_peer(
        &client_node,
        &source_peer,
        vec![defra_p2p_adapter::P2pDocumentRequest {
            collection: demand.collection_name().into(),
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
        expected_changes: access
            .execute(&format!(
                r#"{{ ToolFileChange(docID: "{}") {{ file_changes }} }}"#,
                escape_graphql_string(&image),
            ))
            .await?["data"]["ToolFileChange"][0]["file_changes"]
            .clone(),
    };
    let sweep = reconcile_images(&store, &delivery);
    tokio::pin!(sweep);
    tokio::select! {
        result = &mut sweep => anyhow::bail!("image sweep returned before the fence: {result:?}"),
        _ = gate.reached.notified() => {},
        _ = tokio::time::sleep(Duration::from_secs(20)) => anyhow::bail!("image sweep did not reach its fence"),
    }
    if shared {
        source_node
            .delete_dac_actor_relationship(
                source.did(),
                "ToolFileChange",
                &image,
                "reader",
                client.did(),
            )
            .await?;
    } else {
        enrollment_store.revoke_request(&enrollment).await?;
    }
    gate.resume.notify_one();
    tokio::time::timeout(Duration::from_secs(20), sweep).await??;
    let checks = store.checks.lock().unwrap().clone();
    anyhow::ensure!(
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
    .context("model did not produce a terminal outcome")?;
    let refused = receipt_row(&source_node, &demand, "rejected").await?;
    let refusal = verified(&refused, &demand, client.as_ref()).await?;
    anyhow::ensure!(
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
            anyhow::ensure!(
                checks == [false, false],
                "revocation did not reach final fence"
            );
            anyhow::ensure!(
                delivery.calls.load(Ordering::SeqCst) == 0 && owner.is_none(),
                "revocation delivered a previously unseen image"
            );
        }
        PauseAt::Acknowledged => {
            anyhow::ensure!(
                checks == [true, false] && delivery.calls.load(Ordering::SeqCst) > 0,
                "post-acknowledgment revocation was not observed"
            );
            if let Some(owner) = owner {
                anyhow::ensure!(
                    owner.to_string() == source.did(),
                    "acknowledged image changed native creator ownership: {owner}"
                );
            }
        }
        PauseAt::Delivered => {
            anyhow::ensure!(
                checks == [true, false],
                "post-delivery revocation was not observed"
            );
            anyhow::ensure!(
                delivery.calls.load(Ordering::SeqCst) > 0
                    && owner.as_ref().map(ToString::to_string).as_deref() == Some(source.did()),
                "delivery did not preserve native creator ownership: {owner:?}"
            );
        }
    }
    anyhow::ensure!(
        !refusal.grant_local_read(client_node.clone()).await?,
        "refusal granted local read"
    );
    let visible = ConfigAccess::local(client_node.clone())
        .execute("{ ToolFileChange { _docID } }")
        .await?;
    anyhow::ensure!(
        visible["data"]["ToolFileChange"]
            .as_array()
            .context("image visibility")?
            .is_empty(),
        "client read an image without a served receipt"
    );

    if shared {
        source_node
            .add_dac_actor_relationship(
                source.did(),
                "ToolFileChange",
                &image,
                "reader",
                client.did(),
            )
            .await?;
    } else {
        let restored = authorize_enrollment_peer(
            source_node.clone(),
            source.clone(),
            client.clone(),
            &client_peer,
            &client_addresses[0],
        )
        .await;
        anyhow::ensure!(
            restored != enrollment,
            "reenrollment reused the revoked request"
        );
    }
    if reset_refusal {
        wait_admitted(&store.inner, &demand).await?;
        let client_refusal = receipt_row(&client_node, &demand, "rejected").await?;
        anyhow::ensure!(
            client_refusal == refused,
            "client did not receive the refusal"
        );
        ConfigAccess::write_local(
            &client_node,
            "test.file_review.reset_refusal",
            &format!(
                r#"mutation {{ update_FileImageHydrationRequest: update_{}(docID: "{}", input: {{
                    status: "pending", status_detail: null, observation_doc_id: null,
                    processed_at: null, outcome_signer_did: null, outcome_signature: null
                }}) {{ _docID }} }}"#,
                demand.collection_name(),
                escape_graphql_string(&demand_doc),
            ),
        )
        .await?;
        push_documents_to_peer(
            &client_node,
            &source_peer,
            vec![defra_p2p_adapter::P2pDocumentRequest {
                collection: demand.collection_name().into(),
                doc_id: demand_doc.clone(),
            }],
        )
        .await?;
        receipt_row(&source_node, &demand, "pending").await?;
        let deliveries_before_replay = delivery.calls.load(Ordering::SeqCst);
        let checks_before_replay = store.checks.lock().unwrap().len();
        reconcile_images(&store, &delivery).await?;
        anyhow::ensure!(
            delivery.calls.load(Ordering::SeqCst) == deliveries_before_replay
                && store.checks.lock().unwrap().len() == checks_before_replay,
            "replaying a recorded refusal reauthorized or redelivered its attempt"
        );
        let result = ConfigAccess::local(source_node.clone())
            .execute(&format!(
                r#"{{ FileImageHydrationRequest: {}(docID: "{}") {{
                    _docID request_key peer_id requester_did node_did session_id request_doc_id tool_call_doc_id {}
                    status status_detail observation_doc_id processed_at outcome_signer_did outcome_signature
                }} }}"#,
                demand.collection_name(), escape_graphql_string(&demand_doc),
                if demand.inventory_reference.is_some() {"inventory_reference"} else {""},
            ))
            .await?;
        anyhow::ensure!(
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
            collection: demand.collection_name().into(),
            doc_id: retry_doc,
        }],
    )
    .await?;
    receipt_row(&source_node, &retry, "pending").await?;
    reconcile_images(&store, &delivery).await?;
    let served = receipt_row(&client_node, &retry, "served").await?;
    let receipt = verified(&served, &retry, client.as_ref()).await?;
    anyhow::ensure!(
        receipt.selection()
            == &FileImageSelection::Selected {
                document: image.clone()
            },
        "retry selected another observation"
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if receipt.grant_local_read(client_node.clone()).await? {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("served retry image did not become readable")??;
    if let Some(entry) = &received_entry {
        let received = receipt
            .read_inventory_image(client_node.clone(), entry)
            .await?
            .context("shared image is not readable after a served receipt")?;
        anyhow::ensure!(
            received == changes,
            "shared reader changed canonical captured bytes"
        );
    }
    if shared && at == PauseAt::Delivered {
        source_node
            .delete_dac_actor_relationship(
                source.did(),
                "ToolFileChange",
                &image,
                "reader",
                client.did(),
            )
            .await?;
        let entry = received_entry.as_ref().context("shared inventory entry")?;
        let retained = receipt
            .read_inventory_image(client_node.clone(), entry)
            .await?
            .context("previously served image became unreadable")?;
        anyhow::ensure!(
            retained == changes,
            "retained image changed after revocation"
        );

        let mut fresh = demand.clone();
        fresh.admission.request_key =
            fresh.derive_retry_request_key(&uuid::Uuid::new_v4().to_string());
        let fresh_doc = admit_on_client(&client_node, &fresh).await?;
        push_documents_to_peer(
            &client_node,
            &source_peer,
            vec![defra_p2p_adapter::P2pDocumentRequest {
                collection: demand.collection_name().into(),
                doc_id: fresh_doc,
            }],
        )
        .await?;
        receipt_row(&source_node, &fresh, "pending").await?;
        reconcile_images(&store, &delivery).await?;
        let refused_fresh = receipt_row(&client_node, &fresh, "rejected").await?;
        let verified_fresh = verified(&refused_fresh, &fresh, client.as_ref()).await?;
        anyhow::ensure!(
            verified_fresh.selection() == &FileImageSelection::Rejected,
            "fresh request was not refused after source permission was revoked"
        );
    }
    let retained = ConfigAccess::local(client_node.clone()).execute(&format!(
        r#"{{ ToolFileChange(docID: "{}") {{ _docID tool_call_doc_id request_doc_id node_did session_id requester_did file_changes }} }}"#,
        escape_graphql_string(&image),
    )).await?;
    let rows = retained["data"]["ToolFileChange"]
        .as_array()
        .context("retained images")?;
    anyhow::ensure!(
        rows.len() == 1
            && rows[0]["_docID"] == image
            && rows[0]["tool_call_doc_id"] == physical
            && rows[0]["request_doc_id"] == retry.request_doc_id
            && rows[0]["node_did"] == source.did()
            && rows[0]["requester_did"] == author.did()
            && rows[0]["session_id"] == session,
        "retry changed retained file images or their exact scope"
    );
    if received_entry.is_none() {
        let ToolFileChangeRead::Retained {
            document_id: delivered,
            changes: delivered_changes,
            ..
        } = load_tool_file_change_observation(
            &ConfigAccess::local(client_node.clone()),
            &physical,
            &retry.request_doc_id,
            source.did(),
            session,
            Some(author.did()),
        )
        .await?
        else {
            anyhow::bail!("delivered file images are unreadable on the client");
        };
        anyhow::ensure!(
            delivered == image && delivered_changes == changes,
            "retry changed retained file images or their exact scope"
        );
    }
    anyhow::ensure!(
        receipt_row(&source_node, &demand, "rejected").await? == refused,
        "reenrollment replaced the signed refusal"
    );
    let client_refusal = receipt_row(&client_node, &demand, "rejected").await?;
    anyhow::ensure!(
        client_refusal == refused && client_refusal["_docID"] == demand_doc,
        "source refusal did not preserve its client-authored demand"
    );
    anyhow::ensure!(
        std::fs::read_to_string(workspace.join("recorded.txt"))? == "later edits\n",
        "review changed the working copy"
    );
    cancel.cancel();
    enrollment_task.await??;
    pairing_task.await??;
    client_node.shutdown().await;
    if let Some(author_test) = author_test {
        author_test.node.shutdown().await;
    }
    source_node.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revocation_after_selection_prevents_unseen_image_delivery() -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(120),
        exercise_revocation(PauseAt::Selection, false, false),
    )
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revocation_after_delivery_preserves_refusal_until_a_fresh_authorized_retry() -> Result<()>
{
    tokio::time::timeout(
        Duration::from_secs(120),
        exercise_revocation(PauseAt::Delivered, false, false),
    )
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resetting_a_client_demand_cannot_replace_a_signed_refusal() -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(120),
        exercise_revocation(PauseAt::Selection, true, false),
    )
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revocation_after_acknowledgment_preserves_refusal_until_a_fresh_retry() -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(120),
        exercise_revocation(PauseAt::Acknowledged, false, false),
    )
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_enrolled_reader_preserves_author_and_revoked_image_refusal_until_retry(
) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(120),
        exercise_revocation(PauseAt::Selection, true, true),
    )
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_reader_revoked_after_delivery_requires_fresh_authorized_retry() -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(120),
        exercise_revocation(PauseAt::Delivered, true, true),
    )
    .await?
}
