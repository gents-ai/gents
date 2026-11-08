//! The conflict premises of `tla/BackgroundAdmissionContention`'s
//! request-row writers: a lease renewal and a background admission write
//! different fields of one `AgentRequest` document, and DefraDB aborts either
//! one when the other commits first.

use super::*;

async fn renewal_fixture() -> (
    Arc<EmbeddedNode>,
    crate::support::TestDb,
    Arc<dyn crate::store::identity::NodeIdentity>,
    gents_protocol::agent_identity::AgentIdentityBinding,
    crate::ConfigAccess,
    RequestLifecycle,
    AgentRequestRow,
    DateTime<Utc>,
    DateTime<Utc>,
) {
    let case = crate::lean_vocab_test::lean_request_execution_lease_observed_cases()
        .iter()
        .find(|case| case.name == "recovery_invalidates_admitted_renewal")
        .unwrap();
    let (node, dir, identity, agent) = test_node().await;
    let access = agent_access(&node, &identity, &agent);
    let epoch = Utc::now();
    let lifecycle = observed_case_request(&node, &identity, &agent, case, epoch).await;
    let row = request_row(&access, &lifecycle.request.doc_id).await;
    let deadline = DateTime::parse_from_rfc3339(row.execution_lease_expires_at.as_deref().unwrap())
        .unwrap()
        .with_timezone(&Utc);
    let now = epoch + chrono::Duration::seconds(case.pre.now as i64);
    (
        node, dir, identity, agent, access, lifecycle, row, deadline, now,
    )
}

async fn admission_sequence(access: &crate::ConfigAccess, doc_id: &str) -> u64 {
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(docID: "{}") {{ background_admission_sequence }} }}"#,
            escape_graphql_string(doc_id),
        ))
        .await
        .unwrap();
    response["data"]["AgentRequest"][0]["background_admission_sequence"]
        .as_u64()
        .unwrap_or(0)
}

async fn bump_admission_sequence(access: &crate::ConfigAccess, doc_id: &str) {
    access
        .transact_write("test.background_admission_bump", |txn| {
            Box::pin(async move {
                crate::tools::call::hook::persistence::reserve_background_slot_in_txn(
                    txn,
                    doc_id,
                    crate::tools::call::hook::persistence::BackgroundSlot::Requested,
                    "",
                )
                .await
            })
        })
        .await
        .unwrap();
}

/// A renewal writes only the lease deadline and an admission bump only
/// `background_admission_sequence`, yet the bump committing first aborts the
/// open renewal: writers of one request document conflict whichever fields
/// they change.
#[tokio::test]
async fn background_admission_bump_invalidates_an_open_renewal() {
    let (node, _dir, identity, agent, access, lifecycle, row, deadline, now) =
        renewal_fixture().await;
    let doc_id = lifecycle.request.doc_id.clone();
    let renewal =
        crate::store::config_client::ConfigApplyTxn::begin_execution_renewal_for_test_with_signer(
            &node,
            identity::Did::new(&agent.agent_did).unwrap(),
            agent_signer(&identity, &agent),
        )
        .await
        .unwrap();
    let staged = crate::request::lifecycle::renew_in_transaction(
        &renewal,
        &doc_id,
        row.execution_generation.as_deref().unwrap(),
        Some(deadline),
        Some(now),
    )
    .await
    .unwrap();
    assert_eq!(
        staged,
        crate::request::lifecycle::RenewalAttemptOutcome::Committed
    );
    let before = admission_sequence(&access, &doc_id).await;
    bump_admission_sequence(&access, &doc_id).await;
    assert_eq!(admission_sequence(&access, &doc_id).await, before + 1);
    let result = renewal.commit().await;
    assert!(crate::store::config_client::is_classified_transaction_conflict(&result.unwrap_err()));
    assert_eq!(
        request_row(&access, &doc_id)
            .await
            .execution_lease_expires_at,
        row.execution_lease_expires_at
    );
}

/// The found race: sibling admissions committing during every attempt of a
/// due renewal poll exhaust it. The poll is re-driven at once rather than at
/// the next poll, which a lease period may not have before it lapses
/// (`MCBackgroundAdmissionContentionLeaseLapse`).
#[tokio::test]
async fn renewal_poll_exhausted_by_sibling_admissions_is_redriven_before_expiry() {
    let (_node, _dir, _identity, _agent, access, lifecycle, row, deadline, now) =
        renewal_fixture().await;
    let doc_id = lifecycle.request.doc_id.clone();
    let poll_attempts = 4;
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let contender = tokio::spawn({
        let access = access.clone();
        let doc_id = doc_id.clone();
        let reached = Arc::clone(&reached);
        let release = Arc::clone(&release);
        async move {
            for _ in 0..poll_attempts {
                reached.notified().await;
                bump_admission_sequence(&access, &doc_id).await;
                release.notify_one();
            }
            loop {
                reached.notified().await;
                release.notify_one();
            }
        }
    });
    let before = admission_sequence(&access, &doc_id).await;
    let (renewed, mutations) =
        crate::store::config_client::ConfigApplyTxn::with_each_successful_mutation_pause(
            Arc::clone(&reached),
            Arc::clone(&release),
            crate::request::lifecycle::renew_execution_lease_once_at_with_access(
                &access,
                &doc_id,
                row.execution_generation.as_deref().unwrap(),
                deadline,
                now,
            ),
        )
        .await;
    contender.abort();
    assert_eq!(
        renewed.expect("an exhausted renewal poll is re-driven while the lease is live"),
        crate::request::lifecycle::RenewalAttemptOutcome::Committed
    );
    assert_eq!(mutations, poll_attempts + 1);
    assert_eq!(
        admission_sequence(&access, &doc_id).await,
        before + poll_attempts as u64
    );
    assert_ne!(
        request_row(&access, &doc_id)
            .await
            .execution_lease_expires_at,
        row.execution_lease_expires_at
    );
}

/// A background admission and a dispatch start read the lease deadline to
/// fence their write, so a renewal committing meanwhile invalidates them: each
/// renewal costs a sibling writer one attempt.
#[tokio::test]
async fn committed_renewal_invalidates_an_open_background_admission() {
    let (node, _dir, identity, agent, access, lifecycle, row, deadline, now) =
        renewal_fixture().await;
    let doc_id = lifecycle.request.doc_id.clone();
    let admission = crate::store::config_client::ConfigApplyTxn::begin_local_with_signer(
        &node,
        identity::Did::new(&agent.agent_did).unwrap(),
        agent_signer(&identity, &agent),
    )
    .await
    .unwrap();
    crate::tools::call::hook::persistence::reserve_background_slot_in_txn(
        &admission,
        &doc_id,
        crate::tools::call::hook::persistence::BackgroundSlot::Requested,
        "lifecycle_state execution_generation execution_lease_expires_at",
    )
    .await
    .unwrap();
    let renewed = tokio::time::timeout(
        Duration::from_secs(10),
        crate::request::lifecycle::renew_execution_lease_once_at_with_access(
            &access,
            &doc_id,
            row.execution_generation.as_deref().unwrap(),
            deadline,
            now,
        ),
    )
    .await
    .expect("renewal does not wait for the open admission")
    .unwrap();
    assert_eq!(
        renewed,
        crate::request::lifecycle::RenewalAttemptOutcome::Committed
    );
    let result = admission.commit().await;
    assert!(crate::store::config_client::is_classified_transaction_conflict(&result.unwrap_err()));
}
