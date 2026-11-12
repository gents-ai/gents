use super::*;
use crate::event_consumer::{
    exclude_arrival, load_or_seed_for_source, load_or_seed_for_source_with_node_binding,
};
use crate::test_support::event_cursor::{
    create_source, cursor, fixture, publish, source, trigger, trigger_consumer,
};
use crate::DesiredStateApplyPlan;
use gents_store::test_support::PIN_FIXED_DID;

#[test]
fn callback_checkpoint_claim_markers_match_lean() {
    let cases = &gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap()
        .callback_checkpoint_claim_cases;
    assert!(!cases.is_empty(), "Lean emitted no callback claim cases");
    for case in cases {
        let claim_time = case["claim_time"].as_bool().unwrap();
        let retained_configuration = case["retained_configuration"].as_bool().unwrap();
        let row = json!({
            "claimed_at": claim_time.then_some("2024-01-01T00:00:00Z"),
            "claimed_callback": retained_configuration.then_some("{}"),
        });
        assert_eq!(
            callback_claim_recorded(&row),
            case["expected"].as_bool().unwrap(),
            "{case}"
        );
    }
}

#[tokio::test]
async fn held_agent_cursor_seed_uses_runtime_observed_node_binding() -> Result<()> {
    let fixture = fixture(false)
        .await
        .context("build the held-Agent cursor fixture")?;
    let agent = &fixture.agent;
    let hidden = agent.execute("{ Node { _docID } }").await?;
    assert!(hidden["data"]["Node"].as_array().unwrap().is_empty());
    let node = fixture.user.execute("{ Node { _docID node_did } }").await?;
    let node_doc = node["data"]["Node"][0]["_docID"]
        .as_str()
        .context("fixture Node has no physical ID")?
        .to_owned();
    let binding = super::super::ConfigurationNodeBinding {
        node_did: identity::Did::new(PIN_FIXED_DID)?,
        doc_id: node_doc,
        enabled: true,
        default_agent_did: None,
    };
    let without_binding = agent
        .transact_write("test.cursor.no_node_binding", |txn| {
            Box::pin(async move {
                load_or_seed_for_source(txn, PIN_FIXED_DID, &trigger_consumer(), "WorkB").await
            })
        })
        .await;
    assert!(
        without_binding.is_err(),
        "the Agent cannot substitute for the Node"
    );
    let agent_did = fixture.agent_identity.did().to_owned();
    let record = agent
        .transact_write("test.cursor.held_node_binding", |txn| {
            let binding = &binding;
            let agent_did = &agent_did;
            Box::pin(async move {
                assert_eq!(txn.actor_did(), Some(agent_did.as_str()));
                load_or_seed_for_source_with_node_binding(
                    txn,
                    PIN_FIXED_DID,
                    &trigger_consumer(),
                    "WorkB",
                    Some(binding),
                )
                .await
            })
        })
        .await
        .context("seed a cursor using the held Node binding")?;
    assert!(!record.doc_id.is_empty());
    assert_eq!(record.cursor.source_collection, "WorkB");
    assert_eq!(cursor(agent, "WorkB").await?, record.cursor.after);

    let mut new_source = source("WorkA");
    new_source.add["event_source_id"] = json!("source-agent-created");
    new_source.update["event_source_id"] = json!("source-agent-created");
    let mut new_trigger = trigger(false);
    new_trigger.add["trigger_id"] = json!("trigger-agent-created");
    new_trigger.update["trigger_id"] = json!("trigger-agent-created");
    new_trigger.add["source"]["event_source_id"] = json!("source-agent-created");
    new_trigger.update["source"]["event_source_id"] = json!("source-agent-created");
    let plan = DesiredStateApplyPlan::new(vec![new_source, new_trigger])?;
    agent
        .transact_write("test.cursor.agent_trigger", |txn| {
            let plan = &plan;
            let binding = &binding;
            Box::pin(async move {
                crate::apply_desired_state_plan_with_node_binding(
                    txn,
                    plan,
                    Some(binding),
                    &crate::test_support::AdmitAllProfiles,
                )
                .await
            })
        })
        .await
        .context("create an Agent-owned source and Trigger with the bound Node")?;
    let consumer = EventConsumer::Trigger {
        trigger_id: "trigger-agent-created".into(),
    };
    let seeded = agent
        .transact_readonly("test.cursor.agent_trigger_read", |txn| {
            let consumer = &consumer;
            Box::pin(async move { load_for_source(txn, PIN_FIXED_DID, consumer, "WorkA").await })
        })
        .await?;
    assert!(
        seeded.is_some(),
        "configuration committed its initial cursor"
    );
    fixture.node.shutdown().await.expect("node shutdown");
    Ok(())
}

#[tokio::test]
async fn source_only_replacement_seeds_disabled_consumers_before_later_arrivals() -> Result<()> {
    let fixture = fixture(false).await?;
    let node = &fixture.node;
    let access = &fixture.user;
    let agent = &fixture.agent;
    create_source(access, "WorkB", "before-replacement").await?;
    publish(access, vec![source("WorkB")]).await?;
    let held = create_source(access, "WorkB", "after-replacement").await?;
    assert_eq!(cursor(agent, "WorkB").await?, "1");
    publish(access, vec![trigger(true)]).await?;
    assert_eq!(cursor(agent, "WorkB").await?, "1");
    let page = access.execute("{_documentArrivals(collection:\"WorkB\",after:\"1\",limit:128){entries{docID cursor}}}").await?;
    assert_eq!(
        page["data"]["_documentArrivals"]["entries"],
        json!([{"docID":held,"cursor":"2"}])
    );
    assert!(agent
        .transact_write("test.cursor.stale_source", |txn| Box::pin(async move {
            exclude_arrival(txn, PIN_FIXED_DID, &trigger_consumer(), "WorkA", "1").await
        }))
        .await
        .is_err());
    // The source change revoked the Agent's writer on the stale WorkA cursor
    // (Lean `moveEventCursor`); the Agent cannot read it or seed a duplicate.
    assert!(cursor(agent, "WorkA").await.is_err());
    assert_eq!(cursor(access, "WorkA").await?, "0");
    publish(access, vec![source("WorkA")]).await?;
    assert_eq!(cursor(agent, "WorkA").await?, "0");
    // Moving back revokes the Agent's writer on the now-stale WorkB cursor.
    assert!(cursor(agent, "WorkB").await.is_err());
    assert_eq!(cursor(access, "WorkB").await?, "1");
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

#[test]
fn seed_position_matches_the_lean_model() {
    gents_lean_vocab::families::event_source_cursor_seed::replay(
        |i: &gents_lean_vocab::families::event_source_cursor_seed::SeedInput| -> u64 {
            let head = i.collection_exists.then(|| i.head.to_string());
            seed_after(head.as_deref()).parse().unwrap()
        },
    );
}

#[tokio::test]
async fn source_registered_before_collection_retains_its_first_arrival() -> Result<()> {
    let fixture = fixture(false).await?;
    let node = &fixture.node;
    let access = &fixture.user;
    let agent = &fixture.agent;
    publish(access, vec![source("FutureWork")]).await?;
    assert_eq!(cursor(agent, "FutureWork").await?, "0");
    access
        .add_collection("type FutureWork { label: String @immutable }")
        .await?;
    let first = create_source(access, "FutureWork", "first").await?;
    publish(access, vec![trigger(true)]).await?;
    assert_eq!(cursor(agent, "FutureWork").await?, "0");
    let page = access.execute("{_documentArrivals(collection:\"FutureWork\",after:\"0\",limit:128){entries{docID cursor}}}").await?;
    assert_eq!(
        page["data"]["_documentArrivals"]["entries"],
        json!([{"docID":first,"cursor":"1"}])
    );
    node.shutdown().await.expect("node shutdown");
    Ok(())
}
