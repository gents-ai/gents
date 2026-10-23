use super::*;
use crate::agent::p2p_reconcile::embedded_impl::tests::{
    authorize_enrollment_peer, runtime_http_test_node, wait_for_active_peer, wait_for_peer_info,
};
use crate::agent::p2p_reconcile::{
    enrollment_authority_channel, run_enrollment_reconciler, run_pairing_reconciler,
    EmbeddedRemoteP2pAdmin, GraphqlEnrollmentStore, PairingFilters, RemoteP2pAdmin,
};
use crate::file_inventory::{admit_demand, FileInventoryReceiver, VerifiedInventoryResponse};
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
            let row = ConfigAccess::transact_local_readonly(
                node,
                None,
                "test.inventory.read_request",
                |txn| {
                    Box::pin(crate::file_inventory::requests::load_request_in_txn(
                        txn, demand,
                    ))
                },
            )
            .await?;
            if let Some(response) = row.and_then(|row| row.response) {
                let verified =
                    VerifiedInventoryResponse::verify(response, demand, identity).await?;
                if let FileInventoryResponse::Refused(receipt) = verified.response() {
                    anyhow::bail!("inventory request refused: {:?}", receipt.reason);
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
    tokio::time::timeout(Duration::from_secs(120), exercise_inventory_delivery()).await?
}

async fn exercise_inventory_delivery() -> Result<()> {
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
            "PeerPairingApplied",
            "AgentSession",
        ],
    )?;
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
    let source_task =
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(run_session_hydration_reconciler(
            source_node.clone(),
            authority.clone(),
            source.clone(),
            cancel.clone(),
        )));
    let mut receiver = FileInventoryReceiver {
        admission: demand.admission.clone(),
        pages: vec![],
    };
    for _ in 0..expected["state"]["pages"]
        .as_array()
        .context("modeled pages")?
        .len()
    {
        let page = receive_page(&client_node, &demand, client.as_ref()).await?;
        let next = page.page.next.clone();
        receiver = receiver
            .receive_for_demand(&demand, page, client.as_ref())
            .await?;
        demand.cursor = next;
    }
    let restored: FileInventoryReceiver = serde_json::from_slice(&serde_json::to_vec(&receiver)?)?;
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
    }
    cancel.cancel();
    source_task.await??;
    enrollment_task.await??;
    pairing_task.await??;
    client_node.shutdown().await;
    source_node.shutdown().await;
    Ok(())
}
