use super::*;
use crate::client::mutations::submission_journal as journal;
use gents::config_client::ConfigAccess;
use gents::graphql::graphql_with_transaction_retry;
use std::sync::Arc;

async fn stored(node: &defra_node::EmbeddedNode) -> Result<Vec<Record>> {
    let response = graphql_with_transaction_retry(
        node,
        "{ PendingChatSubmission { payload } }",
        "test.goal_delivery.snapshot",
    )
    .await?;
    response
        .data
        .as_ref()
        .and_then(|data| data["PendingChatSubmission"].as_array())
        .context("durable Goal snapshot")?
        .iter()
        .map(|row| {
            let envelope: Value =
                serde_json::from_str(row["payload"].as_str().context("payload")?)?;
            serde_json::from_value(envelope["record"].clone()).map_err(Into::into)
        })
        .collect()
}

#[tokio::test]
async fn generated_goal_delivery_histories_survive_the_existing_journal() -> Result<()> {
    let cases = tokio::task::spawn_blocking(model_cases).await??;
    let node = Arc::new(crate::test_support::node_builder().build().await?);
    node.add_schema(gents_protocol::schemas::PENDING_CHAT_SUBMISSION)
        .await?;
    let checked = async {
        let mut count = 0;
        for case in cases {
            let steps = case["trace"].as_array().context("trace")?;
            if !case["initial"].as_array().context("initial")?.is_empty()
                || steps.iter().any(|step| {
                    step["input"]["durable"] == false || step["input"]["verified"] == false
                })
            {
                continue;
            }
            ConfigAccess::write_local(
                &node,
                "test.goal_delivery.clear",
                "mutation { delete_PendingChatSubmission { _docID } }",
            )
            .await?;
            let mut facts = Facts::new()?;
            for step in steps {
                let input = &step["input"];
                match input["kind"].as_str().context("action")? {
                    "reserve" => {
                        let proposal = facts
                            .record(&json!({"intent":input["intent"],
                            "prepared":input["prepared"],"receipt":null,
                            "retainedBytes":true,"acknowledged":false}))
                            .await?;
                        let decision = match journal::goal::reserve(&node, &proposal).await {
                            Ok(record) => record.decision(),
                            Err(_) if step["output"]["result"]["kind"] == "refused" => {
                                Decision::Refused
                            }
                            Err(error) => return Err(error),
                        };
                        assert_eq!(
                            facts.output(decision, None)?,
                            step["output"],
                            "{}",
                            case["name"]
                        );
                    }
                    "query" | "resume" => {
                        let intent = facts.intent(&input["intent"])?;
                        let records = match journal::goal::recover(&node, &intent).await {
                            Ok(record) => record.into_iter().collect::<Vec<_>>(),
                            Err(_) if step["output"]["result"]["kind"] == "refused" => {
                                assert_eq!(facts.output(Decision::Refused, None)?, step["output"]);
                                assert_eq!(
                                    facts.records_json(&stored(&node).await?)?,
                                    step["after"]
                                );
                                continue;
                            }
                            Err(error) => return Err(error),
                        };
                        let (decision, effect) = if input["kind"] == "resume" {
                            resume(&records, &intent)
                        } else {
                            (query(&records, &intent), None)
                        };
                        assert_eq!(
                            facts.output(decision, effect)?,
                            step["output"],
                            "{}",
                            case["name"]
                        );
                    }
                    "observe" => {
                        let intent = facts.intent(&input["intent"])?;
                        let receipt = facts.receipt(&input["receipt"], &input["intent"]).await?;
                        let record = journal::goal::recover(&node, &intent)
                            .await?
                            .context("reservation")?;
                        journal::goal::update(&node, &intent, &record.reference, Some(&receipt))
                            .await?;
                    }
                    "acknowledge" => {
                        let intent = facts.intent(&input["intent"])?;
                        let command = facts.command(&input["prepared"], &input["intent"]).await?;
                        let reference = Reference {
                            command_id: command.command_id.clone(),
                            command_digest: command.computed_digest()?,
                            initial_request_id: command
                                .initial_request
                                .as_ref()
                                .map(|r| r.request_id.clone()),
                            requester_did: command.requester_did,
                        };
                        journal::goal::update(&node, &intent, &reference, None).await?;
                    }
                    "reconnect" => {}
                    other => anyhow::bail!("unknown action {other}"),
                }
                let records = stored(&node).await?;
                assert_eq!(
                    facts.records_json(&records)?,
                    step["after"],
                    "{} {}",
                    case["name"],
                    input["kind"]
                );
                for record in records {
                    let retained = journal::goal::recover(&node, &record.intent)
                        .await?
                        .context("retained")?;
                    assert_eq!(
                        facts.records_json(&[retained])?,
                        facts.records_json(std::slice::from_ref(&record))?
                    );
                    assert!(journal::load(&node, &record.intent.command_id)
                        .await?
                        .is_none());
                    journal::forget(&node, &record.intent.command_id).await;
                    assert!(journal::goal::recover(&node, &record.intent)
                        .await?
                        .is_some());
                }
            }
            count += 1;
        }
        anyhow::ensure!(count == 16, "executed {count} durable Goal histories");
        Ok::<(), anyhow::Error>(())
    }
    .await;
    node.shutdown().await;
    checked
}

#[tokio::test]
async fn competing_goal_reservations_and_acknowledgement_survive_reopen() -> Result<()> {
    let cases = tokio::task::spawn_blocking(model_cases).await??;
    let trace = cases
        .iter()
        .find(|case| case["name"] == "acknowledgement_loss")
        .context("modeled acknowledgement loss")?["trace"]
        .as_array()
        .context("trace")?;
    let mut facts = Facts::new()?;
    let proposal = facts
        .record(&json!({"intent":trace[0]["input"]["intent"],
        "prepared":trace[0]["input"]["prepared"],"receipt":null,
        "retainedBytes":true,"acknowledged":false}))
        .await?;
    let signed = serde_json::to_vec(proposal.prepared.as_ref().unwrap())?;
    let home = tempfile::tempdir()?;
    let open = || async {
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(&proposal.intent.node_did)
                .data_path(home.path())
                .build()
                .await?,
        );
        Ok::<_, anyhow::Error>(node)
    };
    let node = open().await?;
    node.add_schema(gents_protocol::schemas::PENDING_CHAT_SUBMISSION)
        .await?;
    let (left, right) = tokio::join!(
        journal::goal::reserve(&node, &proposal),
        journal::goal::reserve(&node, &proposal)
    );
    assert_eq!(left?.reference, right?.reference);
    node.shutdown().await;
    drop(node);
    let node = open().await?;
    let record = journal::goal::recover(&node, &proposal.intent)
        .await?
        .context("reopened reservation")?;
    assert_eq!(
        serde_json::to_vec(record.prepared.as_ref().unwrap())?,
        signed
    );
    assert_eq!(facts.records_json(&[record])?, trace[0]["after"]);
    let receipt = facts
        .receipt(&trace[1]["input"]["receipt"], &trace[1]["input"]["intent"])
        .await?;
    let settled =
        journal::goal::update(&node, &proposal.intent, &proposal.reference, Some(&receipt)).await?;
    assert_eq!(facts.records_json(&[settled])?, trace[1]["after"]);
    let acknowledged =
        journal::goal::update(&node, &proposal.intent, &proposal.reference, None).await?;
    assert_eq!(facts.records_json(&[acknowledged])?, trace[2]["after"]);
    node.shutdown().await;
    drop(node);
    let node = open().await?;
    let record = journal::goal::recover(&node, &proposal.intent)
        .await?
        .context("reopened acknowledgement")?;
    assert_eq!(
        facts.records_json(std::slice::from_ref(&record))?,
        trace[2]["after"]
    );
    let replay = journal::goal::reserve(&node, &proposal).await?;
    assert_eq!(replay.reference, record.reference);
    assert!(replay.prepared.is_none());
    node.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn competing_distinct_signed_proposals_share_one_committed_goal_preparation() -> Result<()> {
    let cases = tokio::task::spawn_blocking(model_cases).await??;
    let trace = cases
        .iter()
        .find(|case| case["name"] == "racing_first_request_proposal")
        .context("modeled competing proposals")?["trace"]
        .as_array()
        .context("trace")?;
    let mut facts = Facts::new()?;
    let mut proposals = Vec::new();
    for step in &trace[..2] {
        let input = &step["input"];
        proposals.push(
            facts
                .record(&json!({
                    "intent":input["intent"], "prepared":input["prepared"],
                    "receipt":null, "retainedBytes":true, "acknowledged":false,
                }))
                .await?,
        );
    }
    let node = Arc::new(crate::test_support::node_builder().build().await?);
    node.add_schema(gents_protocol::schemas::PENDING_CHAT_SUBMISSION)
        .await?;
    let checked = async {
        assert_ne!(proposals[0].reference, proposals[1].reference);
        let (left, right) = tokio::join!(
            journal::goal::reserve(&node, &proposals[0]),
            journal::goal::reserve(&node, &proposals[1]),
        );
        let left = left?;
        let right = right?;
        assert_eq!(left.reference, right.reference);
        let rows = stored(&node).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reference, left.reference);
        let winner = proposals
            .iter()
            .find(|proposal| proposal.reference == left.reference)
            .context("a signed proposal must win")?;
        assert_eq!(
            serde_json::to_vec(&left.prepared)?,
            serde_json::to_vec(&winner.prepared)?
        );
        assert_eq!(
            serde_json::to_vec(&right.prepared)?,
            serde_json::to_vec(&winner.prepared)?
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    node.shutdown().await;
    checked
}
