use chrono::{DateTime, TimeDelta, Utc};
use gents_lean_vocab::tla::replay::{Red, Trace};
use gents_protocol::message::Message;
use gents_protocol::output::rows::authored_message_key;
use gents_session::request::lifecycle::renew_execution_lease_once_at_with_access;
use gents_session::test_support::{AgentHome, ClaimedRequest, Stage};
use gents_store::test_support::UNREACHED_DEADLINE;

async fn deadline(request: &ClaimedRequest) -> DateTime<Utc> {
    let doc = gents_store::graphql::escape_graphql_string(&request.doc_id);
    let query = format!(
        r#"{{AgentRequest(filter:{{_docID:{{_eq:"{doc}"}}}}){{execution_lease_expires_at}}}}"#
    );
    let data = request.agent_access.execute(&query).await.unwrap();
    data["data"]["AgentRequest"][0]["execution_lease_expires_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn poll(request: &ClaimedRequest, now: DateTime<Utc>) {
    renew_execution_lease_once_at_with_access(
        &request.agent_access,
        &request.doc_id,
        request.lifecycle.execution_generation().unwrap(),
        deadline(request).await,
        now,
    )
    .await
    .unwrap();
}

async fn replay(trace: Trace) {
    let home = AgentHome::start("lease-starvation").await;
    let lease = TimeDelta::from_std(UNREACHED_DEADLINE).unwrap();
    let tick = lease / i32::try_from(trace.constants["Lease"].as_i64().unwrap()).unwrap();
    let free = trace.config == "MCLeaseRenewalStarvationSyncInsideLease";
    let mut claimed = None;
    let mut clock = None;
    let mut load = None;
    let mut accepted = None;
    for step in &trace.steps {
        match step.action.as_str() {
            "Claim" => {
                let request = ClaimedRequest::client_signed_reserved(&home, &home.binding)
                    .ids("lease-owner", "lease-starvation")
                    .lease(UNREACHED_DEADLINE)
                    .drive(Stage::Executing)
                    .await;
                clock = Some(deadline(&request).await - lease);
                claimed = Some(request);
            }
            "SyncStart" => {
                load = Some(tokio::spawn(gents_lean_vocab::snapshot()));
            }
            "SyncEnd" => {
                load.take().unwrap().await.unwrap();
            }
            "Tick" => {
                let now = clock.as_mut().unwrap();
                *now += tick;
                if free && load.is_some() {
                    poll(claimed.as_ref().unwrap(), *now).await;
                }
            }
            "Poll" => poll(claimed.as_ref().unwrap(), clock.unwrap()).await,
            "Publish" => {
                let request = claimed.as_ref().unwrap();
                accepted = Some(
                    request
                        .writer()
                        .publish_authored_message_with_time(
                            &request.lifecycle,
                            "lease-publication",
                            &Message::user("observed producer"),
                            clock,
                        )
                        .await
                        .is_ok(),
                );
            }
            action => panic!("unmapped lease step {action}"),
        }
    }
    let request = claimed.unwrap();
    let live = clock.unwrap() < deadline(&request).await;
    let key = gents_store::graphql::escape_graphql_string(&authored_message_key(
        &request.doc_id,
        "lease-publication",
    ));
    let rows = request
        .agent_access
        .execute(&format!(
            r#"{{AgentMessage(filter:{{message_key:{{_eq:"{key}"}}}}){{_docID}}}}"#
        ))
        .await
        .unwrap();
    assert_eq!(
        rows["data"]["AgentMessage"].as_array().unwrap().len(),
        usize::from(accepted == Some(true))
    );
    let margin = trace.config == "MCLeaseRenewalStarvationMarginSpent";
    let mut props = vec![("SingleWriter", accepted != Some(true) || live)];
    if margin {
        props.push(("LeaseHeld", live));
    }
    if free {
        props.push(("OwnerPublishes", accepted == Some(true)));
    }
    trace.check(if margin { Red::Premise } else { Red::Defect }, &props);
}

#[tokio::test]
async fn lease_renewal_starvation_traces() {
    for json in [
        include_str!(
            "../../proofs/tla/generated/traces/MCLeaseRenewalStarvationSyncInsideLease.json"
        ),
        include_str!("../../proofs/tla/generated/traces/MCLeaseRenewalStarvationMarginSpent.json"),
        include_str!("../../proofs/tla/generated/traces/MCLeaseRenewalStarvationUnfenced.json"),
    ] {
        replay(Trace::load(json, "LeaseRenewalStarvation", &[])).await;
    }
}
