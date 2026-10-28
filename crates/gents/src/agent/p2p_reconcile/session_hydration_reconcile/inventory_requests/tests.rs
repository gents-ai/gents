use super::*;
use crate::agent::p2p_reconcile::embedded_impl::tests::{
    authorize_enrollment_peer, runtime_http_test_node, wait_for_active_peer, wait_for_peer_info,
};
use crate::agent::p2p_reconcile::{
    enrollment_authority_channel, run_enrollment_reconciler, run_pairing_reconciler,
    EmbeddedRemoteP2pAdmin, GraphqlEnrollmentStore, PairingFilters, RemoteP2pAdmin,
};
use crate::file_inventory::{
    admit_demand, begin_client_progress, load_client_progress, receive_client_reply,
    verified_client_response, FileInventoryReceiver, VerifiedInventoryResponse,
};
use crate::identity::KeyIdentity;
use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::tool_call_lifecycle::admission_fixture::{
    ensure_fixture_session, publish_accepted_on_claimed_request,
};
use crate::tool_call_lifecycle::AwaitMode;
use gents_loop::live_output::LiveToolOutputRegistry;
use gents_protocol::file_inventory::FileInventoryAdmission;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::time::Duration;
use tracing::instrument::WithSubscriber;

async fn receive_page(
    node: &Arc<EmbeddedNode>,
    demand: &FileInventoryPageDemand,
    identity: &dyn NodeIdentity,
) -> Result<gents_protocol::file_inventory::FileInventoryPageReceipt> {
    let mut changes =
        crate::subscribe_collections(node, ["FileInventoryRequest", "FileInventoryPage"])?;
    admit_demand(node, demand).await?;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(verified) = verified_client_response(node, demand, identity).await? {
                if let FileInventoryResponse::Refused(receipt) = verified.response() {
                    anyhow::bail!(
                        "inventory request refused: {:?}; attempt={} cursor={:?}",
                        receipt.reason,
                        demand.attempt_id,
                        demand.cursor
                    );
                }
                if let Some(page) = verified.read_page(node.clone(), identity).await? {
                    return Ok(page);
                }
            }
            changes
                .recv()
                .await
                .context("inventory observation subscription closed")?;
        }
    })
    .await
    .context("enrolled client did not receive its inventory page")?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enrolled_inventory_pages_sixty_five_changes_without_hydrating_images() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(120), exercise_inventory_delivery(false)).await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enrolled_inventory_retries_keep_preparation_and_refusal_after_revocation() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(120), exercise_inventory_delivery(true)).await?
}

async fn exercise_inventory_delivery(recovery: bool) -> Result<()> {
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "gents::agent::p2p_reconcile::session_hydration_reconcile=debug".into()
            }),
        )
        .without_time()
        .with_test_writer()
        .finish();
    exercise_inventory_delivery_inner(recovery)
        .with_subscriber(subscriber)
        .await
}

async fn exercise_inventory_delivery_inner(recovery: bool) -> Result<()> {
    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>).await??;
    let model = contracts["file_inventory_receiver_cases"]
        .as_array()
        .context("receiver cases")?
        .iter()
        .find(|case| case["name"] == "complete_contiguous_inventory")
        .context("complete inventory case")?;
    let expected = &model["expected"];
    let expected_paths: BTreeSet<String> = expected["validated"]["entries"]
        .as_array()
        .context("modeled complete entries")?
        .iter()
        .map(|entry| entry["change"]["path"].as_str().unwrap().to_owned())
        .collect();
    let (directory, source, client) = tokio::task::spawn_blocking(|| -> Result<_> {
        let directory = tempfile::tempdir()?;
        let source: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(
            directory.path().join("source.key"),
            None,
        )?);
        let client: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(
            directory.path().join("client.key"),
            None,
        )?);
        Ok((directory, source, client))
    })
    .await??;
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
    let (owner, authority) = enrollment_authority_channel();
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let mut authority_changes = crate::subscribe_collections(
        &source_node,
        [
            "NetworkEnrollmentRequest",
            "NetworkEnrollmentDecision",
            "NetworkAuthorizationRevision",
            "PeerPairingDesired",
            "PeerPairingApplied",
            "AgentSession",
        ],
    )?;
    let enrollment_task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
        run_enrollment_reconciler(source_node.clone(), source.clone(), owner, cancel.clone())
            .with_current_subscriber(),
    ));
    let pairing_task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
        run_pairing_reconciler(
            source_node.clone(),
            source.clone(),
            authority.clone(),
            cancel.clone(),
        )
        .with_current_subscriber(),
    ));
    client_admin
        .add_replicator(
            &source_addresses,
            &["FileInventoryRequest".into()],
            &PairingFilters::default(),
        )
        .await?;
    let session = "inventory-live-session";
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    ensure_fixture_session(
        &source_node,
        session,
        source.did(),
        Some(client.did()),
        &now,
    )
    .await;
    let native_session = ConfigAccess::transact_local_readonly(
        &source_node,
        None,
        "test.inventory.live_session",
        |txn| {
            Box::pin(crate::session::load_agent_session_row_in_txn(
                txn,
                source.did(),
                session,
                Some(client.did()),
            ))
        },
    )
    .await?
    .context("native session")?;
    let mut demand = FileInventoryPageDemand {
        admission: FileInventoryAdmission {
            request: HydrationRequest {
                request_key: "inventory-live".into(),
                peer_id: client_peer,
                requester_did: client.did().into(),
                node_did: source.did().into(),
                session_id: session.into(),
            },
            session_doc_id: native_session.doc_id,
        },
        attempt_id: "inventory-live-attempt".into(),
        cursor: None,
    };
    let store = GraphqlHydrationStore {
        node: source_node.clone(),
        enrollment: authority.clone(),
        identity: source.clone(),
    };
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let admission = store.inventory_admission(&demand).await?;
            if matches!(
                decide_hydration(&demand.admission.request, &admission.catalog),
                HydrationVerdict::Admit(_)
            ) {
                tracing::debug!(
                    expected_enrollment = %enrollment,
                    observed_enrollment = ?admission.authorization.as_ref().map(|value| value.request_id.as_str()),
                    sequence = ?admission.authorization.as_ref().map(|value| value.authorization_sequence),
                    "file inventory route admitted"
                );
                return Ok::<_, anyhow::Error>(());
            }
            authority_changes
                .recv()
                .await
                .context("enrollment observation closed")?;
        }
    })
    .await
    .context("inventory route did not become authorized")??;
    let projection = GraphqlEnrollmentStore::new(source_node.clone(), source.clone())
        .load_projection()
        .await?;
    let active = projection
        .active
        .iter()
        .find(|entry| entry.request.request_id == enrollment)
        .context("active enrollment")?;
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "inventory-live-request",
        source.did(),
        client.did(),
        "general",
        session,
        "Update the recorded files",
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
        "test.inventory.live_request",
        &create.graphql_mutation().map_err(anyhow::Error::msg)?,
    )
    .await?;
    let request_doc = crate::graphql::created_doc_id(&response, "AgentRequest")?;
    let admitted =
        crate::request_admission::load_request_for_admission_test(&source_node, &request_doc)
            .await?;
    let admitted = crate::request_admission::AgentRequestAdmissionVerifier::new(
        source_node.clone(),
        source.clone(),
        authority.clone(),
    )
    .verify_fresh(&admitted, "general")
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut request = RequestLifecycle::new_with_node_did(
        source_node.clone(),
        "general",
        source.did(),
        admitted,
        120,
    );
    anyhow::ensure!(
        request.claim().await? == ClaimOutcome::Claimed,
        "native request did not claim"
    );
    let workspace = directory.path().join("workspace");
    tokio::fs::create_dir_all(workspace.join("src")).await?;
    let workspace = tokio::fs::canonicalize(workspace).await?;
    let output = LiveToolOutputRegistry::default();
    for (index, path) in expected_paths.iter().enumerate() {
        tokio::fs::write(workspace.join(path), format!("before {path}\n")).await?;
        let arguments = json!({"path":path, "content":format!("after {path}\n"), "overwrite":true, "raw_json":true});
        let mut tool = publish_accepted_on_claimed_request(
            source_node.clone(),
            &mut request,
            source.did(),
            index,
            "write_file",
            &format!("inventory-write-{index}"),
            arguments.clone(),
            AwaitMode::Foreground,
            true,
        )
        .await?;
        let physical = tool.doc_id().context("accepted physical tool")?.to_owned();
        let writer = output.writer_for(physical.clone()).await;
        let mutator = crate::toolset::write_file_for_test(&workspace);
        let result = gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
            None, CancellationToken::new(), None, Some(writer), None, None, Default::default(), false,
            mutator.call(arguments.to_string())).await?;
        tool.stage_deferred_writes(output.deferred_writes(&physical).await);
        tool.complete(&result).await?;
    }
    let native = FileInventorySnapshot::load(&source_node, &demand.admission).await?;
    anyhow::ensure!(
        native.entries().len() == expected_paths.len(),
        "native inventory omitted real mutations"
    );
    let source_cancel = cancel.child_token();
    let source_task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
        run_session_hydration_reconciler(
            source_node.clone(),
            authority.clone(),
            source.clone(),
            source_cancel.clone(),
        )
        .with_current_subscriber(),
    ));
    begin_client_progress(
        &client_node,
        &demand.admission,
        &demand.attempt_id,
        client.as_ref(),
    )
    .await?;
    for _ in 0..expected["state"]["pages"]
        .as_array()
        .context("modeled pages")?
        .len()
    {
        let page = receive_page(&client_node, &demand, client.as_ref())
            .await
            .context("initial inventory enumeration")?;
        let next = page.page.next.clone();
        let progress = receive_client_reply(
            &client_node,
            &demand,
            &gents_protocol::file_inventory::FileInventoryReply::Page(page),
            client.as_ref(),
        )
        .await?;
        anyhow::ensure!(
            load_client_progress(&client_node, &demand.admission, client.as_ref())
                .await?
                .as_ref()
                == Some(&progress),
            "client did not durably retain the signed inventory prefix"
        );
        demand.cursor = next;
    }
    let progress = load_client_progress(&client_node, &demand.admission, client.as_ref())
        .await?
        .context("durable client inventory")?;
    anyhow::ensure!(
        progress
            .active()
            .pending_demand(client.as_ref())
            .await?
            .is_none(),
        "complete client inventory has another demand"
    );
    let receiver = progress.active().receiver();
    let restored: FileInventoryReceiver = serde_json::from_slice(&serde_json::to_vec(receiver)?)?;
    let prefix = restored
        .validated(client.as_ref())
        .await?
        .context("received prefix")?;
    anyhow::ensure!(
        prefix.entries() == native.entries() && prefix.binding() == native.binding(),
        "transport changed the source's complete native snapshot"
    );
    anyhow::ensure!(
        prefix.complete() == expected["complete"].as_bool().unwrap()
            && prefix.known_inventory() == expected["known_inventory"].as_bool().unwrap(),
        "transport did not complete modeled coverage"
    );
    source_cancel.cancel();
    source_task.await??;
    if recovery {
        exercise_recovery(RecoveryContext {
            store: &store,
            client_node: &client_node,
            client: &client,
            request: &mut request,
            workspace: &workspace,
            output: &output,
            demand: &demand,
            snapshot: &native,
            expected_count: expected_paths.len(),
            enrollment: &enrollment,
            client_address: &client_addresses[0],
            contracts: &contracts,
        })
        .await?;
    }
    for collection in ["ToolFileChange", "ToolFileInventory"] {
        let response = ConfigAccess::local(client_node.clone())
            .execute(&format!("{{ {collection} {{ _docID }} }}"))
            .await?;
        anyhow::ensure!(
            response["data"][collection]
                .as_array()
                .is_some_and(Vec::is_empty),
            "metadata transport replicated {collection}"
        );
        let source_documents = ConfigAccess::transact_local_readonly(
            &source_node,
            None,
            "test.inventory.source_image_identities",
            |txn| Box::pin(async move { txn.cursor_rows(collection, "_docID", "").await }),
        )
        .await?;
        anyhow::ensure!(
            source_documents.len() == expected_paths.len() + usize::from(recovery),
            "source capture set is incomplete"
        );
        let as_creator = ConfigAccess::transact_local_readonly(
            &client_node,
            Some(source.did().parse()?),
            "test.inventory.client_image_absence",
            |txn| {
                Box::pin(async move {
                    let response = txn
                        .execute_local_response(&format!("{{ {collection} {{ _docID }} }}"))
                        .await?;
                    crate::graphql::required_rows::<Value>(&response, collection)
                })
            },
        )
        .await?;
        anyhow::ensure!(
            as_creator.is_empty(),
            "source-owned {collection} records reached the metadata client"
        );
        for document in source_documents {
            let id = document["_docID"]
                .as_str()
                .context("source capture identity")?;
            let owner = client_node
                .document_acp()
                .get_doc_owner(
                    gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID,
                    gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE,
                    id,
                )
                .await?;
            anyhow::ensure!(
                owner.is_none(),
                "metadata transport registered {collection} document {id}"
            );
        }
    }
    cancel.cancel();
    enrollment_task.await??;
    pairing_task.await??;
    client_node.shutdown().await;
    source_node.shutdown().await;
    Ok(())
}

struct RecoveryContext<'a> {
    store: &'a GraphqlHydrationStore,
    client_node: &'a Arc<EmbeddedNode>,
    client: &'a Arc<dyn NodeIdentity>,
    request: &'a mut RequestLifecycle,
    workspace: &'a std::path::Path,
    output: &'a LiveToolOutputRegistry,
    demand: &'a FileInventoryPageDemand,
    snapshot: &'a FileInventorySnapshot,
    expected_count: usize,
    enrollment: &'a str,
    client_address: &'a str,
    contracts: &'a Value,
}

async fn exercise_recovery(context: RecoveryContext<'_>) -> Result<()> {
    let RecoveryContext {
        store,
        client_node,
        client,
        request,
        workspace,
        output,
        demand,
        snapshot,
        expected_count,
        enrollment,
        client_address,
        contracts,
    } = context;
    let source_node = &store.node;
    let source = &store.identity;
    let mut retry = demand.clone();
    retry.cursor = None;
    retry.attempt_id.push_str("-lost-ack");
    admit_demand(client_node, &retry).await?;
    let row = wait_request(source_node, &retry).await?;
    let lost = LostAcknowledgment {
        inner: EmbeddedHydrationDelivery {
            node: source_node.clone(),
        },
        documents: std::sync::Mutex::new(Vec::new()),
    };
    serve_inventory(store, &lost, &row).await?;
    anyhow::ensure!(
        completion_case(contracts, true, true, false)?["expected"].is_null(),
        "model settled an unacknowledged page"
    );
    let waiting = wait_request(source_node, &retry).await?;
    anyhow::ensure!(
        waiting.response.is_none()
            && crate::file_inventory::load_recorded_reply(source_node, &retry, source.as_ref())
                .await?
                .is_none(),
        "lost acknowledgment settled a pending request"
    );
    let prepared = load_prepared_page(source_node, &retry, source.as_ref())
        .await?
        .context("prepared retry page")?;
    let calls = lost.documents.lock().unwrap().clone();
    anyhow::ensure!(
        calls.len() == HYDRATION_DELIVERY_MAX_ATTEMPTS
            && calls
                .iter()
                .all(|document| document == &prepared.document_id),
        "retry changed its prepared document"
    );
    let path = "src/late.rs";
    tokio::fs::write(workspace.join(path), "late before\n").await?;
    let arguments =
        json!({"path":path, "content":"late after\n", "overwrite":true, "raw_json":true});
    let mut late = publish_accepted_on_claimed_request(
        source_node.clone(),
        request,
        source.did(),
        expected_count,
        "write_file",
        "inventory-late-write",
        arguments.clone(),
        AwaitMode::Foreground,
        true,
    )
    .await?;
    let physical = late.doc_id().context("late physical tool")?.to_owned();
    let writer = output.writer_for(physical.clone()).await;
    let mutator = crate::toolset::write_file_for_test(workspace);
    let result = gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
        None, CancellationToken::new(), None, Some(writer), None, None, Default::default(), false,
        mutator.call(arguments.to_string())).await?;
    late.stage_deferred_writes(output.deferred_writes(&physical).await);
    late.complete(&result).await?;
    let changed = FileInventorySnapshot::load(source_node, &retry.admission).await?;
    anyhow::ensure!(
        changed.binding() != snapshot.binding(),
        "later native mutation did not change the snapshot"
    );
    let delivered = EmbeddedHydrationDelivery {
        node: source_node.clone(),
    };
    serve_inventory(store, &delivered, &waiting).await?;
    let response = wait_request(source_node, &retry)
        .await?
        .response
        .context("recovered response")?;
    VerifiedInventoryResponse::verify(response.clone(), &retry, client.as_ref()).await?;
    let recovered = receive_page(client_node, &retry, client.as_ref())
        .await
        .context("recovering the prepared inventory after lost acknowledgment")?;
    let preparation = contracts["file_inventory_preparation_cases"]
        .as_array()
        .context("preparation cases")?
        .iter()
        .find(|case| case["name"] == "membership_changed_after_preparation")
        .context("changed preparation case")?;
    anyhow::ensure!(
        preparation["expected"] == preparation["prior"],
        "model replaced an existing preparation"
    );
    anyhow::ensure!(
        recovered == prepared.receipt,
        "reply loss changed the original signed snapshot"
    );
    reset_request(source_node, &retry).await?;
    let no_delivery = NoDelivery(std::sync::atomic::AtomicUsize::new(0));
    serve_inventory(
        store,
        &no_delivery,
        &wait_request(source_node, &retry).await?,
    )
    .await?;
    anyhow::ensure!(
        no_delivery.0.load(std::sync::atomic::Ordering::SeqCst) == 0,
        "settled reply was redelivered"
    );
    let replay = wait_request(source_node, &retry)
        .await?
        .response
        .context("replayed response")?;
    anyhow::ensure!(
        replay == response,
        "reply replay changed its signed reference"
    );
    let mut continuation = retry.clone();
    continuation.cursor = prepared.receipt.page.next.clone();
    admit_demand(client_node, &continuation).await?;
    serve_inventory(
        store,
        &no_delivery,
        &wait_request(source_node, &continuation).await?,
    )
    .await?;
    let stale = wait_request(source_node, &continuation)
        .await?
        .response
        .context("stale response")?;
    let verified = VerifiedInventoryResponse::verify(stale, &continuation, client.as_ref()).await?;
    let FileInventoryResponse::Refused(receipt) = verified.response() else {
        anyhow::bail!("stale continuation became a page");
    };
    anyhow::ensure!(
        serde_json::to_value(receipt.reason)? == preparation["unprepared_continuation"]["kind"],
        "native stale continuation disagrees with model execution"
    );
    let mut revoked = retry.clone();
    revoked.attempt_id.push_str("-revoked");
    admit_demand(client_node, &revoked).await?;
    let fence = store
        .inventory_admission(&revoked)
        .await?
        .authorization
        .context("current fence")?;
    let revocation = RevokeAfterAcknowledgment {
        inner: EmbeddedHydrationDelivery {
            node: source_node.clone(),
        },
        enrollment: GraphqlEnrollmentStore::new(source_node.clone(), source.clone()),
        request: enrollment.into(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    };
    serve_inventory(
        store,
        &revocation,
        &wait_request(source_node, &revoked).await?,
    )
    .await?;
    anyhow::ensure!(
        revocation.calls.load(std::sync::atomic::Ordering::SeqCst) == 1
            && !store.inventory_current(&revoked, &fence).await?,
        "delivery did not observe real enrollment revocation"
    );
    let refusal = wait_request(source_node, &revoked)
        .await?
        .response
        .context("revoked response")?;
    let verified =
        VerifiedInventoryResponse::verify(refusal.clone(), &revoked, client.as_ref()).await?;
    let FileInventoryResponse::Refused(receipt) = verified.response() else {
        anyhow::bail!("revoked delivery became a page grant");
    };
    anyhow::ensure!(
        serde_json::to_value(receipt.reason)?
            == completion_case(contracts, true, false, true)?["expected"]["reason"],
        "real revocation disagrees with model execution"
    );
    let mut authority_changes = crate::subscribe_collections(
        source_node,
        [
            "NetworkEnrollmentRequest",
            "NetworkEnrollmentDecision",
            "NetworkAuthorizationRevision",
            "PeerPairingDesired",
            "PeerPairingApplied",
        ],
    )?;
    let new_enrollment = authorize_enrollment_peer(
        source_node.clone(),
        source.clone(),
        client.clone(),
        &revoked.admission.request.peer_id,
        client_address,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let admission = store.inventory_admission(&revoked).await?;
            if matches!(
                decide_hydration(&revoked.admission.request, &admission.catalog),
                HydrationVerdict::Admit(_)
            ) {
                tracing::debug!(
                    expected_enrollment = %new_enrollment,
                    observed_enrollment = ?admission.authorization.as_ref().map(|value| value.request_id.as_str()),
                    sequence = ?admission.authorization.as_ref().map(|value| value.authorization_sequence),
                    "file inventory route restored after reenrollment"
                );
                return Ok::<_, anyhow::Error>(());
            }
            authority_changes
                .recv()
                .await
                .context("restored enrollment observation closed")?;
        }
    })
    .await
    .context("reenrollment did not restore the inventory route")??;
    reset_request(source_node, &revoked).await?;
    serve_inventory(
        store,
        &no_delivery,
        &wait_request(source_node, &revoked).await?,
    )
    .await?;
    anyhow::ensure!(
        wait_request(source_node, &revoked).await?.response == Some(refusal),
        "reenrollment replaced a terminal refusal"
    );
    let mut fresh = revoked.clone();
    fresh.attempt_id.push_str("-fresh");
    admit_demand(client_node, &fresh).await?;
    serve_inventory(store, &delivered, &wait_request(source_node, &fresh).await?).await?;
    VerifiedInventoryResponse::verify(
        wait_request(source_node, &fresh)
            .await?
            .response
            .context("fresh response")?,
        &fresh,
        client.as_ref(),
    )
    .await?;
    let page = receive_page(client_node, &fresh, client.as_ref())
        .await
        .context("fresh inventory after reenrollment")?;
    anyhow::ensure!(
        &page.page.binding == changed.binding(),
        "explicit retry did not select the current snapshot"
    );
    Ok(())
}

fn completion_case(
    contracts: &Value,
    before: bool,
    after: bool,
    confirmed: bool,
) -> Result<&Value> {
    contracts["file_inventory_completion_cases"]
        .as_array()
        .context("completion cases")?
        .iter()
        .find(|case| {
            case["candidate"]["kind"] == "page"
                && case["before"] == before
                && case["after"] == after
                && case["confirmed"] == confirmed
                && case["committed"] == true
        })
        .context("modeled delivery observation")
}

async fn wait_request(
    node: &Arc<EmbeddedNode>,
    demand: &FileInventoryPageDemand,
) -> Result<InventoryRequestRow> {
    let mut changes = crate::subscribe_collections(node, ["FileInventoryRequest"])?;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(row) = ConfigAccess::transact_local_readonly(
                node,
                None,
                "test.inventory.wait_request",
                |txn| {
                    Box::pin(crate::file_inventory::requests::load_request_in_txn(
                        txn, demand,
                    ))
                },
            )
            .await?
            {
                return Ok(row);
            }
            changes
                .recv()
                .await
                .context("inventory request observation closed")?;
        }
    })
    .await
    .context("inventory request did not arrive")?
}

async fn reset_request(node: &Arc<EmbeddedNode>, demand: &FileInventoryPageDemand) -> Result<()> {
    let row = wait_request(node, demand).await?;
    ConfigAccess::write_local(node, "test.inventory.lose_response", &format!(
        r#"mutation {{ update_FileInventoryRequest(docID: "{}", input: {{ status: "pending", response: null }}) {{ _docID }} }}"#,
        escape_graphql_string(&row.document_id))).await?;
    Ok(())
}

struct LostAcknowledgment {
    inner: EmbeddedHydrationDelivery,
    documents: std::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl HydrationDelivery for LostAcknowledgment {
    async fn push_documents_to_peer(&self, peer: &str, target: DeliveryTarget<'_>) -> Result<()> {
        let DeliveryTarget::FileInventory(document) = target else {
            anyhow::bail!("unexpected delivery target");
        };
        self.documents.lock().unwrap().push(document.to_owned());
        self.inner.push_documents_to_peer(peer, target).await?;
        anyhow::bail!("injected lost acknowledgment after actual metadata delivery")
    }
}

struct NoDelivery(std::sync::atomic::AtomicUsize);

#[async_trait]
impl HydrationDelivery for NoDelivery {
    async fn push_documents_to_peer(&self, _: &str, _: DeliveryTarget<'_>) -> Result<()> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        anyhow::bail!("settled attempts must not perform another transfer")
    }
}

struct RevokeAfterAcknowledgment {
    inner: EmbeddedHydrationDelivery,
    enrollment: GraphqlEnrollmentStore,
    request: String,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl HydrationDelivery for RevokeAfterAcknowledgment {
    async fn push_documents_to_peer(&self, peer: &str, target: DeliveryTarget<'_>) -> Result<()> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.push_documents_to_peer(peer, target).await?;
        self.enrollment.revoke_request(&self.request).await?;
        Ok(())
    }
}
