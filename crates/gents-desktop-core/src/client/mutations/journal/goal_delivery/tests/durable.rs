use super::*;
use crate::client::mutations::submission_journal as journal;
use gents_store::graphql::graphql_with_transaction_retry;
use std::sync::Arc;

async fn stored(node: &defra_node::Node) -> Result<Vec<Record>> {
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
    for case in gents_lean_vocab::families::goal_client_delivery_contracts_durable_trace::cases() {
        let input = serde_json::to_value(&case.input)?;
        let actions = input["actions"].as_array().context("actions")?;
        // Each history runs against the journal of the desktop whose client
        // issues its first command.
        let mut facts = Facts::new()?;
        let client = facts.client(number(&actions[0]["intent"], "client")?)?;
        let node = crate::test_support::client_journal_node(&client).await?;
        let mut steps = Vec::new();
        for action in actions {
            let before = facts.records_json(&stored(&node).await?)?;
            let output = match action["kind"].as_str().context("action")? {
                "reserve" => {
                    let proposal = facts.record(&proposal(action)).await?;
                    let decision = match journal::goal::reserve(&node, &proposal).await {
                        Ok(record) => record.decision(),
                        Err(_) => Decision::Refused,
                    };
                    facts.output(decision, None)?
                }
                "query" | "resume" => {
                    let intent = facts.intent(&action["intent"])?;
                    match journal::goal::recover(&node, &intent).await {
                        Ok(record) => {
                            let records = record.into_iter().collect::<Vec<_>>();
                            let (decision, effect) = if action["kind"] == "resume" {
                                resume(&records, &intent)
                            } else {
                                (query(&records, &intent), None)
                            };
                            facts.output(decision, effect)?
                        }
                        Err(_) => facts.output(Decision::Refused, None)?,
                    }
                }
                "observe" => {
                    let intent = facts.intent(&action["intent"])?;
                    let receipt = facts.receipt(&action["receipt"], &action["intent"]).await?;
                    let record = journal::goal::recover(&node, &intent)
                        .await?
                        .context("reservation")?;
                    journal::goal::update(&node, &intent, &record.reference, Some(&receipt))
                        .await?;
                    Value::Null
                }
                "acknowledge" => {
                    let intent = facts.intent(&action["intent"])?;
                    let command = facts
                        .command(&action["prepared"], &action["intent"])
                        .await?;
                    journal::goal::update(&node, &intent, &reference_of(command)?, None).await?;
                    Value::Null
                }
                "reconnect" => Value::Null,
                other => anyhow::bail!("unknown action {other}"),
            };
            let records = stored(&node).await?;
            steps.push(
                json!({"before":before,"after":facts.records_json(&records)?,"output":output}),
            );
            for record in records {
                let retained = journal::goal::recover(&node, &record.intent)
                    .await?
                    .context("retained")?;
                assert_eq!(
                    facts.records_json(&[retained])?,
                    facts.records_json(std::slice::from_ref(&record))?
                );
            }
        }
        node.shutdown().await.unwrap();
        case.check(serde_json::from_value(Value::Array(steps))?);
    }
    Ok(())
}

#[tokio::test]
async fn competing_goal_reservations_and_acknowledgement_survive_reopen() -> Result<()> {
    for case in
        gents_lean_vocab::families::goal_client_delivery_contracts_reopened_acknowledgement::cases()
    {
        let input = serde_json::to_value(&case.input)?;
        let actions = input["actions"].as_array().context("actions")?;
        let mut facts = Facts::new()?;
        let proposal = facts.record(&proposal(&actions[0])).await?;
        let signed = serde_json::to_vec(proposal.prepared.as_ref().unwrap())?;
        let home = tempfile::tempdir()?;
        let open = || async {
            let node = Arc::new(
                defra_node::Node::builder()
                    .with_node_identity_did(&proposal.intent.client_did)
                    .data_path(home.path())
                    .build()
                    .await?,
            );
            Ok::<_, anyhow::Error>(node)
        };
        let node = open().await?;
        node.add_collection(&crate::test_support::unbound_journal_schema())
            .await?;
        let (left, right) = tokio::join!(
            journal::goal::reserve(&node, &proposal),
            journal::goal::reserve(&node, &proposal)
        );
        assert_eq!(left?.reference, right?.reference);
        node.shutdown().await.unwrap();
        drop(node);
        let node = open().await?;
        let record = journal::goal::recover(&node, &proposal.intent)
            .await?
            .context("reopened reservation")?;
        assert_eq!(
            serde_json::to_vec(record.prepared.as_ref().unwrap())?,
            signed
        );
        let reserved = facts.records_json(&[record])?;
        let receipt = facts
            .receipt(&actions[1]["receipt"], &actions[1]["intent"])
            .await?;
        let settled =
            journal::goal::update(&node, &proposal.intent, &proposal.reference, Some(&receipt))
                .await?;
        let settled = facts.records_json(&[settled])?;
        let acknowledged =
            journal::goal::update(&node, &proposal.intent, &proposal.reference, None).await?;
        let acknowledged = facts.records_json(&[acknowledged])?;
        node.shutdown().await.unwrap();
        drop(node);
        let node = open().await?;
        let record = journal::goal::recover(&node, &proposal.intent)
            .await?
            .context("reopened acknowledgement")?;
        assert_eq!(
            facts.records_json(std::slice::from_ref(&record))?,
            acknowledged
        );
        let replay = journal::goal::reserve(&node, &proposal).await?;
        assert_eq!(replay.reference, record.reference);
        assert!(replay.prepared.is_none());
        node.shutdown().await.unwrap();
        case.check(serde_json::from_value(json!([
            reserved,
            settled,
            acknowledged
        ]))?);
    }
    Ok(())
}

#[tokio::test]
async fn competing_distinct_signed_proposals_share_one_committed_goal_preparation() -> Result<()> {
    for case in gents_lean_vocab::families::goal_client_delivery_contracts_racing_proposals::cases()
    {
        let input = serde_json::to_value(&case.input)?;
        let mut facts = Facts::new()?;
        let mut proposals = Vec::new();
        for action in input["actions"].as_array().context("actions")? {
            proposals.push(facts.record(&proposal(action)).await?);
        }
        let node =
            crate::test_support::client_journal_node(&proposals[0].intent.client_did).await?;
        let checked = async {
        assert_ne!(proposals[0].reference, proposals[1].reference);
        let (left, right) = tokio::join!(
            journal::goal::reserve(&node, &proposals[0]),
            journal::goal::reserve(&node, &proposals[1]),
        );
        let left = left?;
        let right = right?;
        let rows = stored(&node).await?;
        assert!(rows.iter().all(|row| row.reference == left.reference));
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
        Ok::<_, anyhow::Error>(
            gents_lean_vocab::families::goal_client_delivery_contracts_racing_proposals::RacingOutput {
                shared_preparation: left.reference == right.reference,
                records: rows.len() as u64,
            },
        )
    }
    .await;
        node.shutdown().await.unwrap();
        case.check(checked?);
    }
    Ok(())
}
