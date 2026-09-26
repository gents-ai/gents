//! Physical P2P replication of canonical output and durable peer identity.

use anyhow::{Context, Result};
use defra_p2p_adapter::P2pDocumentRequest;
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;

use crate::support::enrollment::wait_for_peer_identity;
use crate::support::p2p_waits::{wait_for_connected_peer, wait_for_listen_addr};
use crate::support::{test_p2p_db, TestDb};

#[tokio::test]
async fn p2p_crash_reopens_same_durable_peer_identity() {
    let mut db = test_p2p_db("p2p-crash-peer-identity").await;
    let (before_peer, _) = wait_for_peer_identity(db.node.as_ref()).await;
    db.simulate_process_crash()
        .await
        .expect("reopen P2P store after crash");
    let (after_peer, _) = wait_for_peer_identity(db.node.as_ref()).await;
    assert_eq!(
        before_peer, after_peer,
        "durable peer identity changed on restart"
    );
    assert_eq!(db.process_generation, 1);
}

#[tokio::test]
async fn replicated_output_ordinal_twin_fails_closed_after_physical_p2p_import() {
    assert_replicated_nonclosing_ordinal_twin_rejected()
        .await
        .expect("replicated ordinal twin must invalidate the original canonical projection");
}

/// Two connected P2P nodes. Only `AgentNetwork` has a replicator route: the
/// exact-document push is sent through the replicator retry guard and returns
/// Ok without sending when no route exists, while output collections must
/// arrive only by the explicit push.
async fn connected_pair() -> Result<(TestDb, TestDb)> {
    let a = test_p2p_db("p2p-replication-a").await;
    let b = test_p2p_db("p2p-replication-b").await;
    let b_address = wait_for_listen_addr(b.node.as_ref()).await;
    let a_address = wait_for_listen_addr(a.node.as_ref()).await;
    a.node
        .p2p()
        .context("A has no P2P transport")?
        .connect_peer(&b_address)
        .await
        .context("connect P2P peers")?;
    wait_for_connected_peer(a.node.as_ref()).await;
    wait_for_connected_peer(b.node.as_ref()).await;
    for (from, address) in [(&a, &b_address), (&b, &a_address)] {
        from.node
            .p2p()
            .context("peer has no P2P transport")?
            .add_replicator(
                vec!["AgentNetwork".to_string()],
                Some(address),
                Default::default(),
                Vec::new(),
                None,
            )
            .await
            .context("register exact-document push route")?;
    }
    Ok((a, b))
}

async fn segment_exists(db: &TestDb, doc_id: &str) -> Result<bool> {
    let query = format!(
        "{{ AgentOutputSegment(filter: {{ _docID: {{ _eq: \"{}\" }} }}, limit: 2) {{ _docID }} }}",
        escape_graphql_string(doc_id)
    );
    let response = db.node.execute(&query).await;
    anyhow::ensure!(
        !response.has_errors(),
        "query AgentOutputSegment/{doc_id} failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentOutputSegment"))
        .and_then(serde_json::Value::as_array)
        .context("exact-document query omitted collection")?;
    anyhow::ensure!(rows.len() <= 1, "duplicate physical segment {doc_id}");
    Ok(rows.len() == 1)
}

async fn push_segment(from: &TestDb, to: &TestDb, doc_id: &str) -> Result<()> {
    let (peer_id, _) = wait_for_peer_identity(to.node.as_ref()).await;
    from.node
        .p2p()
        .context("replication source has no P2P transport")?
        .push_documents_to_peer(
            &peer_id,
            vec![P2pDocumentRequest {
                collection: "AgentOutputSegment".to_string(),
                doc_id: doc_id.to_string(),
            }],
        )
        .await
        .with_context(|| format!("push AgentOutputSegment/{doc_id}"))?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if segment_exists(to, doc_id).await? {
            return Ok(());
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "P2P push did not materialize AgentOutputSegment/{doc_id}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Exercise imported-fact projection only; it does not establish that either
/// peer could authorize the other's write under a production ACP grant.
async fn assert_replicated_nonclosing_ordinal_twin_rejected() -> Result<()> {
    use gents::session::canonical_rows::{
        decode_output_segment_row, decode_transcript_message_row, output_segment_create_variables,
        AGENT_MESSAGE_FIELDS, AGENT_OUTPUT_SEGMENT_FIELDS, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::message::Message;
    use gents_protocol::output::ReconstructionError;

    const SESSION: &str = "p2p-replicated-ordinal-conflict";
    const ORIGINAL: &str = "original";
    const TWIN: &str = "intruder";
    let (a, b) = connected_pair().await?;
    let agent_did = crate::support::AGENT_DID;
    crate::support::create_agent_message_in_scope(
        a.node.as_ref(),
        agent_did,
        None,
        SESSION,
        1,
        "assistant",
        ORIGINAL,
        "2026-01-01T00:00:00Z",
    )
    .await;

    let a_access = ConfigAccess::Local(a.node.clone());
    let header_query = format!(
        "{{ AgentMessage(filter: {{ agent_did: {{ _eq: \"{}\" }}, session_id: {{ _eq: \"{}\" }}, sequence: {{ _eq: 1 }} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}",
        escape_graphql_string(agent_did),
        escape_graphql_string(SESSION),
    );
    let header_response = a_access.execute(&header_query).await?;
    let headers = header_response["data"]["AgentMessage"]
        .as_array()
        .context("original canonical header query omitted rows")?;
    anyhow::ensure!(headers.len() == 1, "expected one original canonical header");
    let header = decode_transcript_message_row(&headers[0])?;
    let request_doc_id = header
        .message
        .request_doc_id
        .as_deref()
        .context("original header omitted its request")?;
    let segment_query = format!(
        "{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: \"{}\" }}, agent_did: {{ _eq: \"{}\" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}",
        escape_graphql_string(request_doc_id),
        escape_graphql_string(agent_did),
    );
    let original_response = a_access.execute(&segment_query).await?;
    let original_rows = original_response["data"]["AgentOutputSegment"]
        .as_array()
        .context("original segment query omitted rows")?;
    anyhow::ensure!(original_rows.len() == 1, "expected one original segment");
    let original = decode_output_segment_row(&original_rows[0])?;
    anyhow::ensure!(
        original.segment.payload == ORIGINAL,
        "original payload changed"
    );
    anyhow::ensure!(
        original.segment.ordinal == Some(0) && original.segment.close.is_some(),
        "original fixture is not a closed ordinal-0 segment"
    );
    let native_id = header
        .message
        .native_id
        .as_deref()
        .context("original assistant header omitted native ID")?;
    anyhow::ensure!(native_id == "native-1", "fixture native ID changed");
    let (baseline_header, baseline_message) = gents::session::load_canonical_message_from_node(
        a.node.as_ref(),
        &header.doc_id,
        agent_did,
        None,
    )
    .await?;
    anyhow::ensure!(baseline_header == header.message, "baseline header changed");
    anyhow::ensure!(
        baseline_message == Message::assistant_with_id(native_id.to_owned(), ORIGINAL),
        "baseline projection did not contain the original text"
    );

    let mut twin = original.segment.clone();
    twin.payload = TWIN.into();
    twin.close = None;
    anyhow::ensure!(
        twin.payload.len() == original.segment.payload.len() && twin.close.is_none(),
        "twin must be nonclosing without changing the sealed byte extent"
    );
    let variables = output_segment_create_variables(&twin)?;
    ConfigAccess::transact_local(
        b.node.as_ref(),
        None,
        "test.p2p_replicated_output_conflict",
        |txn| {
            let variables = variables.clone();
            Box::pin(async move {
                txn.execute_with_variables(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &variables)
                    .await
                    .map(|_| ())
            })
        },
    )
    .await?;
    let b_access = ConfigAccess::Local(b.node.clone());
    let source_rows = b_access.execute(&segment_query).await?;
    let source_rows = source_rows["data"]["AgentOutputSegment"]
        .as_array()
        .context("source segment query omitted rows")?;
    anyhow::ensure!(source_rows.len() == 1, "source must hold only the twin");
    let source_twin = decode_output_segment_row(&source_rows[0])?;
    let twin_doc_id = source_twin.doc_id.clone();
    anyhow::ensure!(
        source_twin.segment == twin,
        "source twin differs from the created physical fact"
    );
    anyhow::ensure!(
        twin_doc_id != original.doc_id,
        "twin reused original identity"
    );
    anyhow::ensure!(
        !segment_exists(&a, &twin_doc_id).await?,
        "twin arrived on A before explicit P2P push"
    );
    anyhow::ensure!(
        segment_exists(&b, &twin_doc_id).await?,
        "twin is not physically present on its source"
    );
    push_segment(&b, &a, &twin_doc_id).await?;
    let received_response = a_access.execute(&segment_query).await?;
    let received_rows = received_response["data"]["AgentOutputSegment"]
        .as_array()
        .context("receiver segment query omitted rows")?;
    anyhow::ensure!(
        received_rows.len() == 2,
        "receiver did not retain both facts"
    );
    let received = received_rows
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(
        received
            .iter()
            .any(|row| row.doc_id == original.doc_id && row.segment == original.segment)
            && received
                .iter()
                .any(|row| row.doc_id == twin_doc_id && row.segment == twin),
        "receiver physical identities or bytes changed during P2P import"
    );
    let unchanged_header = a_access.execute(&header_query).await?;
    let unchanged_rows = unchanged_header["data"]["AgentMessage"]
        .as_array()
        .context("receiver header query omitted rows after import")?;
    anyhow::ensure!(unchanged_rows.len() == 1, "receiver header count changed");
    let unchanged = decode_transcript_message_row(&unchanged_rows[0])?;
    anyhow::ensure!(
        unchanged.doc_id == header.doc_id && unchanged.message == header.message,
        "P2P import changed the original canonical header"
    );
    let error = gents::session::load_canonical_message_from_node(
        a.node.as_ref(),
        &header.doc_id,
        agent_did,
        None,
    )
    .await
    .expect_err("replicated ordinal twin must reject canonical reconstruction");
    anyhow::ensure!(
        matches!(
            error.downcast_ref::<ReconstructionError>(),
            Some(ReconstructionError::ConflictingSegments { ordinal: 0, .. })
        ),
        "expected ordinal-0 segment conflict, got {error:#}"
    );
    Ok(())
}
