use super::observe_until;
use std::time::Duration;

const CEILING: Duration = Duration::from_secs(10);

/// A collection another actor writes every second, read through a query
/// whose answer never satisfies the wait (the rows its reader cannot see):
/// the writes wake the wait, and it still fails at the ceiling with the
/// answer it last saw.
#[tokio::test(start_paused = true)]
async fn a_wait_whose_answer_never_changes_fails_at_the_ceiling_despite_writes() {
    let mut writes = tokio::time::interval(Duration::from_secs(1));
    let mut reads = 0u32;
    let outcome = observe_until(
        CEILING,
        async || {
            writes.tick().await;
        },
        async || {
            reads += 1;
            Err::<(), _>("AgentRequest: []")
        },
    )
    .await;
    assert_eq!(outcome, Err("AgentRequest: []".to_owned()));
    assert!(
        reads > 2,
        "the periodic writes woke the wait: {reads} reads"
    );
}

/// A wait whose answer keeps changing makes progress, so it outlives the
/// ceiling and resolves once the answer satisfies it.
#[tokio::test(start_paused = true)]
async fn a_wait_whose_answer_changes_outlives_the_ceiling() {
    let mut writes = tokio::time::interval(Duration::from_secs(1));
    let mut step = 0u64;
    let finish = 3 * CEILING.as_secs();
    let outcome = observe_until(
        CEILING,
        async || {
            writes.tick().await;
        },
        async || {
            step += 1;
            if step > finish {
                Ok(step)
            } else {
                Err(step)
            }
        },
    )
    .await;
    assert_eq!(outcome, Ok(finish + 1));
}

#[tokio::test]
async fn template_cloned_test_stores_stay_isolated_and_survive_crash_reopen() {
    use crate::config_client::ConfigAccess;
    use crate::node_extension::Extensions;

    let mut first = super::TestStore::start("template-iso-a", Extensions::none()).await;
    let second = super::TestStore::start("template-iso-b", Extensions::none()).await;
    assert_ne!(
        first.did(),
        second.did(),
        "concurrently live stores must not share a node DID"
    );
    assert_ne!(first.path(), second.path());
    assert_ne!(first.graphql_endpoint(), second.graphql_endpoint());

    let access =
        ConfigAccess::local_as(first.node.clone(), identity::Did::new(first.did()).unwrap());
    access
        .write(
            "test.template_crash_write",
            &format!(
                r#"mutation {{ create_NodeRuntime(input: {{ node_did: "{}", reconcile_phase: "idle" }}) {{ _docID }} }}"#,
                gents_protocol::graphql::escape_graphql_string(first.did())
            ),
        )
        .await
        .unwrap();
    drop(access);

    first.simulate_process_crash().await.unwrap();
    let after_crash = ConfigAccess::local_as(
        first.node.clone(),
        identity::Did::new(first.did()).unwrap(),
    )
    .execute(&format!(
        r#"{{ NodeRuntime(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ _docID reconcile_phase }} }}"#,
        gents_protocol::graphql::escape_graphql_string(first.did())
    ))
    .await
    .unwrap();
    assert_eq!(
        gents_protocol::graphql::graphql_rows_from_response(&after_crash, "NodeRuntime").len(),
        1,
        "committed write must survive simulate_process_crash on a template-cloned store"
    );

    let first_did = first.did().to_owned();
    first.node.shutdown().await.unwrap();
    drop(first);

    let third = super::TestStore::start("template-iso-c", Extensions::none()).await;
    assert_ne!(third.did(), second.did());
    let third_rows =
        ConfigAccess::local_as(third.node.clone(), identity::Did::new(third.did()).unwrap())
            .execute(&format!(
                r#"{{ NodeRuntime(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                gents_protocol::graphql::escape_graphql_string(&first_did)
            ))
            .await
            .unwrap();
    assert!(
        gents_protocol::graphql::graphql_rows_from_response(&third_rows, "NodeRuntime").is_empty(),
        "reused template slot must never leak documents from a dropped store"
    );

    second.node.shutdown().await.unwrap();
    third.node.shutdown().await.unwrap();
}
