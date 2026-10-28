use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::process_control::{
    ProcessStop, ProcessStopOutcome, ProcessStopReceipt, PROCESS_STOP_VERSION,
};

use super::enrollment_store::GraphqlEnrollmentStore;
use super::session_command_authority::{current_admission, AuthorScope};
use super::session_commands::{CommandRow, FIELDS};
use crate::config_client::{ConfigAccess, IdempotentTransactionRetry};
use crate::graphql::escape_graphql_string;
use crate::hook::BackgroundExecutionRegistry;
use crate::identity::NodeIdentity;
use crate::tool_control::{
    cancel_session_background_process_by_doc_id, CancelBackgroundToolCallOutcome,
};

const COLLECTION: &str = "AgentSessionProcessStop";

pub(super) async fn verified_receipt(
    identity: &dyn NodeIdentity,
    intent: &str,
    raw: Option<&str>,
) -> Result<Option<ProcessStopReceipt>> {
    let Ok(command) = serde_json::from_str::<ProcessStop>(intent) else {
        return Ok(None);
    };
    let Some(receipt) = raw.and_then(|raw| serde_json::from_str::<ProcessStopReceipt>(raw).ok())
    else {
        return Ok(None);
    };
    if receipt.validate_for(&command).is_ok()
        && identity
            .verify(
                &receipt.node_did,
                &receipt.signing_payload(),
                &receipt.signature,
            )
            .await
            .unwrap_or(false)
    {
        Ok(Some(receipt))
    } else {
        Ok(None)
    }
}

async fn signed_receipt(
    identity: &dyn NodeIdentity,
    command: &ProcessStop,
    outcome: ProcessStopOutcome,
    reason: String,
) -> Result<ProcessStopReceipt> {
    let mut receipt = ProcessStopReceipt {
        version: PROCESS_STOP_VERSION,
        command_id: command.command_id.clone(),
        command_digest: command.digest(),
        node_did: command.node_did.clone(),
        outcome,
        reason,
        processed_at: Utc::now().to_rfc3339(),
        signature: Vec::new(),
    };
    receipt.signature = identity.sign(&receipt.signing_payload()).await?;
    receipt.validate_for(command)?;
    Ok(receipt)
}

pub(super) async fn process_command(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn NodeIdentity>,
    executions: &BackgroundExecutionRegistry,
    doc_id: &str,
) -> Result<Option<DateTime<Utc>>> {
    let store = GraphqlEnrollmentStore::new(node.clone(), identity.clone());
    let accepted = ConfigAccess::transact_local_idempotent(node, None, IdempotentTransactionRetry::Standard, "process_stop.authorize", |txn| {
        let store = &store;
        Box::pin(async move {
            let response = txn.execute(&format!(r#"{{ {COLLECTION}(docID: "{}") {{ {FIELDS} }} }}"#, escape_graphql_string(doc_id))).await?;
            let rows: Vec<CommandRow> = serde_json::from_value(response["data"][COLLECTION].clone())?;
            let [row] = rows.as_slice() else { anyhow::bail!("process stop command is missing or ambiguous") };
            if row.node_did != identity.did() { return Ok(None); }
            let command: ProcessStop = serde_json::from_str(&row.intent_json)?;
            anyhow::ensure!(row.command_id == command.command_id && row.requester_did == command.requester_did && row.requester_peer_id == command.requester_peer_id && row.node_did == command.node_did && row.session_id == command.session_id, "process stop scope disagrees with signed intent");
            crate::graphql::fence_arrivals(txn, COLLECTION).await?;
            let twins = txn.execute(&format!(r#"{{ {COLLECTION}(filter: {{ node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }} }}) {{ _docID command_id }} }}"#, escape_graphql_string(&row.node_did), escape_graphql_string(&row.requester_did))).await?;
            let twins = twins["data"][COLLECTION].as_array().context("missing process stop identity query")?;
            if twins.iter().any(|twin| twin["command_id"].as_str() == Some(command.command_id.as_str()) && twin["_docID"].as_str() != Some(row.doc_id.as_str())) { return Ok(None); }
            if let Some(receipt) = verified_receipt(identity.as_ref(), &row.intent_json, row.receipt_json.as_deref()).await? {
                return Ok(receipt.outcome.needs_execution().then_some((command, None)));
            }
            let rejection = if let Err(error) = command.validate_shape() { Some(error.to_string()) }
                else if command.node_did != identity.did() { Some("process stop targets another node".into()) }
                else if !identity.verify(&command.requester_did, &command.signing_payload(), &command.signature).await.unwrap_or(false) { Some("invalid process stop author signature".into()) }
                else { None };
            let now = Utc::now();
            let mut deferred = None;
            let rejection = if rejection.is_some() { rejection } else {
                let issued = DateTime::parse_from_rfc3339(&command.issued_at)?.with_timezone(&Utc);
                if now < issued { deferred = Some(issued); None }
                else if now >= DateTime::parse_from_rfc3339(&command.expires_at)? { Some("process stop has expired".into()) }
                else { current_admission(txn, store, AuthorScope { requester_did: &command.requester_did, requester_peer_id: command.requester_peer_id.as_deref(), node_did: &command.node_did }, COLLECTION).await?.err() }
            };
            if let Some(issued) = deferred { return Ok(Some((command, Some(issued)))); }
            let outcome = if rejection.is_some() { ProcessStopOutcome::Rejected } else { ProcessStopOutcome::Requested };
            let receipt = signed_receipt(identity.as_ref(), &command, outcome, rejection.unwrap_or_default()).await?;
            let written = txn.execute(&format!(r#"mutation {{ update_{COLLECTION}(docID: "{}", input: {{ receipt_json: "{}" }}) {{ _docID }} }}"#, escape_graphql_string(&row.doc_id), escape_graphql_string(&serde_json::to_string(&receipt)?))).await?;
            anyhow::ensure!(written["data"][format!("update_{COLLECTION}")].as_array().is_some_and(|rows| rows.len() == 1 && rows[0]["_docID"].as_str() == Some(row.doc_id.as_str())), "process stop receipt lost its command");
            Ok(outcome.needs_execution().then_some((command, None)))
        })
    }).await?;
    let Some((command, deferred)) = accepted else {
        return Ok(None);
    };
    if let Some(issued) = deferred {
        return Ok(Some(issued));
    }
    let result = cancel_session_background_process_by_doc_id(
        node.clone(),
        executions,
        &command.node_did,
        Some(&command.requester_did),
        &command.session_id,
        &command.tool_call_id,
        &command.tool_call_doc_id,
    )
    .await?;
    let (outcome, reason) = match result {
        CancelBackgroundToolCallOutcome::Cancelled { .. } => {
            (ProcessStopOutcome::Stopped, String::new())
        }
        CancelBackgroundToolCallOutcome::AlreadyTerminal { state } => {
            (ProcessStopOutcome::AlreadyTerminal, state)
        }
        CancelBackgroundToolCallOutcome::Lost => (
            ProcessStopOutcome::Lost,
            "process ownership was lost".into(),
        ),
        CancelBackgroundToolCallOutcome::Unverified => (
            ProcessStopOutcome::Unverified,
            "process ownership could not be verified".into(),
        ),
        CancelBackgroundToolCallOutcome::NotBackground
        | CancelBackgroundToolCallOutcome::NotFound => (
            ProcessStopOutcome::Rejected,
            "background process not found".into(),
        ),
    };
    let receipt = signed_receipt(identity.as_ref(), &command, outcome, reason).await?;
    ConfigAccess::transact_local_idempotent(node, None, IdempotentTransactionRetry::Standard, "process_stop.finish", |txn| {
        let command = &command;
        let receipt = &receipt;
        Box::pin(async move {
            let response = txn.execute(&format!(r#"{{ {COLLECTION}(docID: "{}") {{ {FIELDS} }} }}"#, escape_graphql_string(doc_id))).await?;
            let rows: Vec<CommandRow> = serde_json::from_value(response["data"][COLLECTION].clone())?;
            let [row] = rows.as_slice() else { anyhow::bail!("process stop command disappeared") };
            anyhow::ensure!(serde_json::from_str::<ProcessStop>(&row.intent_json)? == *command, "process stop intent changed after authorization");
            let prior = verified_receipt(identity.as_ref(), &row.intent_json, row.receipt_json.as_deref()).await?.context("process stop acceptance disappeared")?;
            if !prior.outcome.needs_execution() { return Ok(()); }
            let written = txn.execute(&format!(r#"mutation {{ update_{COLLECTION}(docID: "{}", input: {{ receipt_json: "{}" }}) {{ _docID }} }}"#, escape_graphql_string(doc_id), escape_graphql_string(&serde_json::to_string(receipt)?))).await?;
            anyhow::ensure!(written["data"][format!("update_{COLLECTION}")].as_array().is_some_and(|rows| rows.len() == 1 && rows[0]["_docID"].as_str() == Some(doc_id)), "process stop final receipt was not written");
            Ok(())
        })
    }).await?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::KeyIdentity;

    #[tokio::test]
    async fn accepted_receipt_is_verified_and_remains_recoverable() {
        let dir = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(dir.path().join("node.key"), None).unwrap();
        let command = ProcessStop {
            version: PROCESS_STOP_VERSION,
            command_id: "command".into(),
            requester_did: identity.did().into(),
            requester_peer_id: None,
            node_did: identity.did().into(),
            session_id: "session".into(),
            tool_call_id: "call".into(),
            tool_call_doc_id: "document".into(),
            issued_at: "2026-10-01T00:00:00Z".into(),
            expires_at: "2026-10-01T00:01:00Z".into(),
            signature: vec![1; 64],
        };
        let receipt = signed_receipt(
            &identity,
            &command,
            ProcessStopOutcome::Requested,
            String::new(),
        )
        .await
        .unwrap();
        let raw = serde_json::to_string(&receipt).unwrap();
        let intent = serde_json::to_string(&command).unwrap();
        assert!(verified_receipt(&identity, &intent, Some(&raw))
            .await
            .unwrap()
            .unwrap()
            .outcome
            .needs_execution());
        let mut forged = receipt;
        forged.outcome = ProcessStopOutcome::Stopped;
        assert!(verified_receipt(
            &identity,
            &intent,
            Some(&serde_json::to_string(&forged).unwrap())
        )
        .await
        .unwrap()
        .is_none());
    }

    #[tokio::test]
    async fn signed_stop_cancels_only_the_bound_background_process() {
        use crate::tool_call_lifecycle::admission_fixture::{
            published_admission, PublishedAdmissionOptions,
        };
        use crate::tool_call_lifecycle::AwaitMode;
        use tokio_util::sync::CancellationToken;

        let fixture = published_admission(PublishedAdmissionOptions {
            name: "signed-process-stop".into(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .unwrap();
        let identity: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_or_create(fixture.path.join("test-agent.key"), None).unwrap(),
        );
        let executions = BackgroundExecutionRegistry::default();
        let token = CancellationToken::new();
        let reservation = executions
            .reserve(
                fixture.tool.tool_call_id().to_owned(),
                fixture.tool.doc_id().unwrap().to_owned(),
                token.clone(),
            )
            .unwrap();
        let worker = tokio::spawn({
            let token = token.clone();
            async move {
                token.cancelled().await;
                drop(reservation);
            }
        });
        let mut command = ProcessStop {
            version: PROCESS_STOP_VERSION,
            command_id: "stop-command".into(),
            requester_did: identity.did().into(),
            requester_peer_id: None,
            node_did: identity.did().into(),
            session_id: fixture.tool.session_id().into(),
            tool_call_id: fixture.tool.tool_call_id().into(),
            tool_call_doc_id: fixture.tool.doc_id().unwrap().into(),
            issued_at: (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
            expires_at: (Utc::now() + chrono::Duration::minutes(1)).to_rfc3339(),
            signature: Vec::new(),
        };
        command = crate::tool_control::prepare_process_stop(identity.as_ref(), command)
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let reconciler = tokio::spawn(
            super::super::session_commands::run_session_command_reconciler_with_registry(
                fixture.node.clone(),
                identity.clone(),
                executions.clone(),
                shutdown.clone(),
            ),
        );
        let access = ConfigAccess::local(fixture.node.clone());
        let receipt =
            crate::tool_control::submit_prepared_process_stop(&access, identity.as_ref(), &command)
                .await
                .unwrap();
        assert_eq!(receipt.outcome, ProcessStopOutcome::Stopped);
        assert!(token.is_cancelled());
        worker.await.unwrap();
        let replay =
            crate::tool_control::submit_prepared_process_stop(&access, identity.as_ref(), &command)
                .await
                .unwrap();
        assert_eq!(replay, receipt);
        shutdown.cancel();
        reconciler.await.unwrap().unwrap();
        fixture.node.shutdown().await;
        std::fs::remove_dir_all(&fixture.path).unwrap();
    }

    #[tokio::test]
    async fn accepted_stop_is_recovered_after_its_issuance_window() {
        use crate::tool_call_lifecycle::admission_fixture::{
            published_admission, PublishedAdmissionOptions,
        };
        use crate::tool_call_lifecycle::AwaitMode;
        use tokio_util::sync::CancellationToken;

        let fixture = published_admission(PublishedAdmissionOptions {
            name: "recover-process-stop".into(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .unwrap();
        let identity: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_or_create(fixture.path.join("test-agent.key"), None).unwrap(),
        );
        let executions = BackgroundExecutionRegistry::default();
        let token = CancellationToken::new();
        let reservation = executions
            .reserve(
                fixture.tool.tool_call_id().into(),
                fixture.tool.doc_id().unwrap().into(),
                token.clone(),
            )
            .unwrap();
        let worker = tokio::spawn({
            let token = token.clone();
            async move {
                token.cancelled().await;
                drop(reservation);
            }
        });
        let command = crate::tool_control::prepare_process_stop(
            identity.as_ref(),
            ProcessStop {
                version: PROCESS_STOP_VERSION,
                command_id: "recover-stop".into(),
                requester_did: identity.did().into(),
                requester_peer_id: None,
                node_did: identity.did().into(),
                session_id: fixture.tool.session_id().into(),
                tool_call_id: fixture.tool.tool_call_id().into(),
                tool_call_doc_id: fixture.tool.doc_id().unwrap().into(),
                issued_at: (Utc::now() - chrono::Duration::minutes(2)).to_rfc3339(),
                expires_at: (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
                signature: Vec::new(),
            },
        )
        .await
        .unwrap();
        let receipt = signed_receipt(
            identity.as_ref(),
            &command,
            ProcessStopOutcome::Requested,
            String::new(),
        )
        .await
        .unwrap();
        let response = ConfigAccess::transact_local_write(&fixture.node, None, "test.process_stop.recover", |txn| {
            let command = &command; let receipt = &receipt;
            Box::pin(async move { txn.execute_with_variables(
                "mutation($input: AgentSessionProcessStopMutationInputArg!) { create_AgentSessionProcessStop(input: $input) { _docID } }",
                &serde_json::json!({"input":{"command_id":command.command_id,"requester_did":command.requester_did,
                    "requester_peer_id":command.requester_peer_id,"node_did":command.node_did,
                    "session_id":command.session_id,"intent_json":serde_json::to_string(command)?,"receipt_json":serde_json::to_string(receipt)?}}),
            ).await })
        }).await.unwrap();
        crate::graphql::created_doc_id(&response, COLLECTION).unwrap();
        let shutdown = CancellationToken::new();
        let reconciler = tokio::spawn(
            super::super::session_commands::run_session_command_reconciler_with_registry(
                fixture.node.clone(),
                identity.clone(),
                executions.clone(),
                shutdown.clone(),
            ),
        );
        let final_receipt = crate::tool_control::submit_prepared_process_stop(
            &ConfigAccess::local(fixture.node.clone()),
            identity.as_ref(),
            &command,
        )
        .await
        .unwrap();
        assert_eq!(final_receipt.outcome, ProcessStopOutcome::Stopped);
        assert!(token.is_cancelled());
        worker.await.unwrap();
        shutdown.cancel();
        reconciler.await.unwrap().unwrap();
        fixture.node.shutdown().await;
        std::fs::remove_dir_all(&fixture.path).unwrap();
    }

    #[tokio::test]
    async fn unavailable_enrollment_authority_never_stops_the_process() {
        use crate::tool_call_lifecycle::admission_fixture::{
            published_admission, PublishedAdmissionOptions,
        };
        use crate::tool_call_lifecycle::AwaitMode;
        use tokio_util::sync::CancellationToken;

        let fixture = published_admission(PublishedAdmissionOptions {
            name: "unpaired-process-stop".into(),
            real_identity: true,
            paired_client: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .unwrap();
        let runtime: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_or_create(fixture.path.join("test-agent.key"), None).unwrap(),
        );
        let client =
            KeyIdentity::load_or_create(fixture.path.join("paired-client.key"), None).unwrap();
        let executions = BackgroundExecutionRegistry::default();
        let token = CancellationToken::new();
        let reservation = executions
            .reserve(
                fixture.tool.tool_call_id().into(),
                fixture.tool.doc_id().unwrap().into(),
                token.clone(),
            )
            .unwrap();
        let now = Utc::now();
        let command = crate::tool_control::prepare_process_stop(
            &client,
            ProcessStop {
                version: PROCESS_STOP_VERSION,
                command_id: "unpaired-stop".into(),
                requester_did: client.did().into(),
                requester_peer_id: Some("unpaired-peer".into()),
                node_did: runtime.did().into(),
                session_id: fixture.tool.session_id().into(),
                tool_call_id: fixture.tool.tool_call_id().into(),
                tool_call_doc_id: fixture.tool.doc_id().unwrap().into(),
                issued_at: (now - chrono::Duration::seconds(1)).to_rfc3339(),
                expires_at: (now + chrono::Duration::minutes(1)).to_rfc3339(),
                signature: Vec::new(),
            },
        )
        .await
        .unwrap();
        let access = ConfigAccess::local(fixture.node.clone());
        let published = access.write("test.unpaired_stop", &format!(r#"mutation {{ create_AgentSessionProcessStop(input: {{ command_id: "{}", requester_did: "{}", requester_peer_id: "{}", node_did: "{}", session_id: "{}", intent_json: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&command.command_id), escape_graphql_string(&command.requester_did),
            escape_graphql_string(command.requester_peer_id.as_deref().unwrap()), escape_graphql_string(&command.node_did),
            escape_graphql_string(&command.session_id), escape_graphql_string(&serde_json::to_string(&command).unwrap()))).await.unwrap();
        let doc_id = crate::graphql::created_doc_id(&published, COLLECTION).unwrap();
        assert!(
            process_command(&fixture.node, &runtime, &executions, &doc_id)
                .await
                .is_err()
        );
        let response = access
            .execute(&format!(
                r#"{{ {COLLECTION}(docID: "{}") {{ intent_json receipt_json }} }}"#,
                escape_graphql_string(&doc_id)
            ))
            .await
            .unwrap();
        let row = &response["data"][COLLECTION][0];
        assert!(row["receipt_json"].is_null());
        assert!(!token.is_cancelled());
        drop(reservation);
        fixture.node.shutdown().await;
        std::fs::remove_dir_all(&fixture.path).unwrap();
    }

    #[tokio::test]
    async fn enrolled_client_stop_uses_current_route_and_revocation() -> Result<()> {
        use crate::agent::p2p_reconcile::embedded_impl::tests::{
            authorize_enrollment_peer, runtime_http_test_node, wait_for_active_peer,
            wait_for_peer_info,
        };
        use crate::agent::p2p_reconcile::{
            enrollment_authority_channel, run_enrollment_reconciler, run_pairing_reconciler,
            EmbeddedRemoteP2pAdmin, GraphqlEnrollmentStore, RemoteP2pAdmin,
        };
        use crate::tool_call_lifecycle::admission_fixture::{
            ensure_fixture_session, publish_accepted_on_claimed_request,
        };
        use crate::tool_call_lifecycle::AwaitMode;
        use gents_protocol::request_admission::{
            AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
        };
        use tokio_util::sync::CancellationToken;

        let directory = tempfile::tempdir()?;
        let runtime: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(
            directory.path().join("runtime.key"),
            None,
        )?);
        let client: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(
            directory.path().join("client.key"),
            None,
        )?);
        let (runtime_test, _) = runtime_http_test_node(runtime.as_ref()).await;
        let (client_test, _) = runtime_http_test_node(client.as_ref()).await;
        let node = runtime_test.node.clone();
        let client_node = client_test.node.clone();
        crate::test_support::install_test_agent(&node, runtime.did(), "general").await;
        let runtime_admin = EmbeddedRemoteP2pAdmin::new(node.clone());
        let client_admin = EmbeddedRemoteP2pAdmin::new(client_node.clone());
        let runtime_addresses = wait_for_peer_info(&runtime_admin).await;
        let client_addresses = wait_for_peer_info(&client_admin).await;
        runtime_admin.connect(&client_addresses).await?;
        wait_for_active_peer(&runtime_admin).await;
        wait_for_active_peer(&client_admin).await;
        let peer = client_node
            .p2p()
            .context("client P2P")?
            .local_peer_id()
            .await?;
        let enrollment = authorize_enrollment_peer(
            node.clone(),
            runtime.clone(),
            client.clone(),
            &peer,
            &client_addresses[0],
        )
        .await;
        let (owner, authority) = enrollment_authority_channel();
        let shutdown = CancellationToken::new();
        let enrollment_task = tokio::spawn(run_enrollment_reconciler(
            node.clone(),
            runtime.clone(),
            owner,
            shutdown.clone(),
        ));
        let pairing_task = tokio::spawn(run_pairing_reconciler(
            node.clone(),
            runtime.clone(),
            authority.clone(),
            shutdown.clone(),
        ));
        client_admin
            .add_replicator(
                &runtime_addresses,
                &[COLLECTION.into()],
                &crate::agent::p2p_reconcile::PairingFilters::default(),
            )
            .await?;
        let enrollment_store = GraphqlEnrollmentStore::new(node.clone(), runtime.clone());
        let projection = enrollment_store.load_projection().await?;
        let active = projection
            .active
            .iter()
            .find(|entry| entry.request.request_id == enrollment)
            .context("active enrollment")?;
        let session = "remote-stop-session";
        let now = Utc::now();
        ensure_fixture_session(
            &node,
            session,
            runtime.did(),
            Some(client.did()),
            &now.to_rfc3339(),
        )
        .await;
        let mut request = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "remote-stop-request",
            runtime.did(),
            client.did(),
            "general",
            session,
            "work",
            "interactive",
            now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            AgentRequestAdmissionRecord::enrollment(
                client.did(),
                &active.request.request_id,
                &active.request.request_digest,
                &active.request.admin_did,
                active.revision.sequence,
                &active.revision.authorization_expires_at,
            ),
        );
        crate::sign_agent_request_create(client.as_ref(), &mut request).await?;
        let response = ConfigAccess::write_local(
            &node,
            "test.remote_stop.request",
            &request.graphql_mutation().map_err(anyhow::Error::msg)?,
        )
        .await?;
        let request_doc = crate::graphql::created_doc_id(&response, "AgentRequest")?;
        let request =
            crate::request_admission::load_request_for_admission_test(&node, &request_doc).await?;
        let mut request = crate::lifecycle::RequestLifecycle::new_with_node_did(
            node.clone(),
            "general",
            runtime.did(),
            request,
            60,
        );
        assert_eq!(
            request.claim().await?,
            crate::lifecycle::ClaimOutcome::Claimed
        );
        let tool = publish_accepted_on_claimed_request(
            node.clone(),
            &mut request,
            runtime.did(),
            0,
            "bash_unrestricted",
            "remote-process",
            serde_json::json!({}),
            AwaitMode::Background,
            true,
        )
        .await?;
        let executions = BackgroundExecutionRegistry::default();
        let token = CancellationToken::new();
        let reservation = executions.reserve(
            tool.tool_call_id().into(),
            tool.doc_id().unwrap().into(),
            token.clone(),
        )?;
        let worker = tokio::spawn({
            let token = token.clone();
            async move {
                token.cancelled().await;
                drop(reservation);
            }
        });
        let command = crate::tool_control::prepare_process_stop(
            client.as_ref(),
            ProcessStop {
                version: PROCESS_STOP_VERSION,
                command_id: "remote-stop".into(),
                requester_did: client.did().into(),
                requester_peer_id: Some(peer.clone()),
                node_did: runtime.did().into(),
                session_id: session.into(),
                tool_call_id: tool.tool_call_id().into(),
                tool_call_doc_id: tool.doc_id().unwrap().into(),
                issued_at: (now - chrono::Duration::seconds(1)).to_rfc3339(),
                expires_at: (now + chrono::Duration::minutes(2)).to_rfc3339(),
                signature: Vec::new(),
            },
        )
        .await?;
        let access = ConfigAccess::local(node.clone());
        let publish = |command: &ProcessStop| {
            let access = &access;
            let command = command.clone();
            async move {
                let response = access.write("test.remote_stop.command", &format!(r#"mutation {{ create_AgentSessionProcessStop(input: {{ command_id: "{}", requester_did: "{}", requester_peer_id: "{}", node_did: "{}", session_id: "{}", intent_json: "{}" }}) {{ _docID }} }}"#,
                escape_graphql_string(&command.command_id), escape_graphql_string(&command.requester_did), escape_graphql_string(command.requester_peer_id.as_deref().unwrap()),
                escape_graphql_string(&command.node_did), escape_graphql_string(&command.session_id), escape_graphql_string(&serde_json::to_string(&command)?))).await?;
                crate::graphql::created_doc_id(&response, COLLECTION)
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let admitted = ConfigAccess::transact_local_readonly(
                    &node,
                    None,
                    "test.remote_stop.route",
                    |txn| {
                        Box::pin(async {
                            current_admission(
                                txn,
                                &enrollment_store,
                                AuthorScope {
                                    requester_did: client.did(),
                                    requester_peer_id: Some(&peer),
                                    node_did: runtime.did(),
                                },
                                COLLECTION,
                            )
                            .await
                        })
                    },
                )
                .await?;
                if admitted.is_ok() {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .context("client route did not become authorized")??;
        let command_task = tokio::spawn(
            super::super::session_commands::run_session_command_reconciler_with_registry(
                node.clone(),
                runtime.clone(),
                executions.clone(),
                shutdown.clone(),
            ),
        );
        let client_access = ConfigAccess::local(client_node.clone());
        let receipt = crate::tool_control::submit_prepared_process_stop(
            &client_access,
            client.as_ref(),
            &command,
        )
        .await?;
        assert_eq!(receipt.outcome, ProcessStopOutcome::Stopped);
        assert!(token.is_cancelled());
        worker.await?;
        enrollment_store.revoke_request(&enrollment).await?;
        let revoked = crate::tool_control::prepare_process_stop(
            client.as_ref(),
            ProcessStop {
                command_id: "revoked-stop".into(),
                signature: Vec::new(),
                ..command
            },
        )
        .await?;
        let revoked_doc = publish(&revoked).await?;
        process_command(&node, &runtime, &executions, &revoked_doc).await?;
        let response = access
            .execute(&format!(
                r#"{{ {COLLECTION}(docID: "{}") {{ intent_json receipt_json }} }}"#,
                escape_graphql_string(&revoked_doc)
            ))
            .await?;
        let row = &response["data"][COLLECTION][0];
        let receipt = verified_receipt(
            runtime.as_ref(),
            row["intent_json"].as_str().unwrap(),
            row["receipt_json"].as_str(),
        )
        .await?
        .context("revocation receipt")?;
        assert_eq!(receipt.outcome, ProcessStopOutcome::Rejected);
        shutdown.cancel();
        command_task.await??;
        enrollment_task.await??;
        pairing_task.await??;
        node.shutdown().await;
        client_node.shutdown().await;
        Ok(())
    }
}
