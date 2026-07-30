use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fires_on_matching_source_doc_create() {
    let db = test_db("trigger-conformance-fires").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(&db, "trigger-conformance-fires", "backend-fires").await;
    let initial_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-fires",
        &agent.default_behavior_id,
        "plain prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-fires",
        "task-fires",
        "WebhookEvent",
        "created",
        None,
        true,
        "serial",
    )
    .await;

    wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > initial_gen && snap.last_reconcile_result == "applied"
    })
    .await;

    let _source_doc_id = write_webhook_event(db.node.as_ref(), "ext-1", "any").await;
    wait_for_request_count(
        db.node.as_ref(),
        "trigger-fires",
        1,
        Duration::from_secs(10),
    )
    .await;

    let fired = wait_for_last_status(
        db.node.as_ref(),
        "trigger-fires",
        "fired",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(fired.fire_count, Some(1));
    assert_eq!(fired.task_id.as_deref(), Some("task-fires"));

    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn does_not_fire_when_source_doc_fails_filter() {
    let db = test_db("trigger-conformance-filter-miss").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(
        &db,
        "trigger-conformance-filter-miss",
        "backend-filter-miss",
    )
    .await;
    let initial_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-filter-miss",
        &agent.default_behavior_id,
        "prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-filter-miss",
        "task-filter-miss",
        "WebhookEvent",
        "created",
        Some(r#"{ kind: { _eq: "signup" } }"#),
        true,
        "serial",
    )
    .await;
    wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > initial_gen && snap.last_reconcile_result == "applied"
    })
    .await;

    let _ = write_webhook_event(db.node.as_ref(), "ext-other", "other").await;
    assert_no_request_within(
        db.node.as_ref(),
        "trigger-filter-miss",
        Duration::from_secs(2),
    )
    .await;

    let row = fetch_event_trigger_row(db.node.as_ref(), "trigger-filter-miss")
        .await
        .expect("EventTrigger doc present");
    assert_eq!(row.last_status, None);
    assert_eq!(row.last_error, None);
    assert_eq!(row.fire_count.unwrap_or(0), 0);
    assert_eq!(row.last_fired_source_doc_id, None);

    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enabled_false_does_not_fire() {
    let db = test_db("trigger-conformance-disabled").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(&db, "trigger-conformance-disabled", "backend-disabled").await;

    create_task(
        db.node.as_ref(),
        "task-disabled",
        &agent.default_behavior_id,
        "prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-disabled",
        "task-disabled",
        "WebhookEvent",
        "created",
        None,
        false,
        "serial",
    )
    .await;

    tokio::time::sleep(Duration::from_secs(7)).await;

    let _ = write_webhook_event(db.node.as_ref(), "ext-disabled", "any").await;
    assert_no_request_within(db.node.as_ref(), "trigger-disabled", Duration::from_secs(2)).await;

    let row = fetch_event_trigger_row(db.node.as_ref(), "trigger-disabled")
        .await
        .expect("EventTrigger doc present");
    assert_eq!(
        row.enabled,
        Some(false),
        "disabled trigger must persist enabled=false"
    );
    assert_eq!(
        row.fire_count.unwrap_or(0),
        0,
        "disabled trigger must not fire"
    );

    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfill_is_forward_only() {
    let db = test_db("trigger-conformance-backfill").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    let _ = write_dynamic_event(db.node.as_ref(), "WebhookEvent", "pre-1", "signup").await;
    let _ = write_dynamic_event(db.node.as_ref(), "WebhookEvent", "pre-2", "signup").await;
    let _ = write_dynamic_event(db.node.as_ref(), "WebhookEvent", "pre-3", "signup").await;

    let agent = boot_agent(&db, "trigger-conformance-backfill", "backend-backfill").await;
    let initial_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-backfill",
        &agent.default_behavior_id,
        "prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-backfill",
        "task-backfill",
        "WebhookEvent",
        "created",
        None,
        true,
        "serial",
    )
    .await;

    wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > initial_gen && snap.last_reconcile_result == "applied"
    })
    .await;

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-backfill", "event").await,
        0,
        "backfill must not replay pre-existing source docs"
    );

    let _ = write_webhook_event(db.node.as_ref(), "post-1", "signup").await;
    wait_for_request_count(
        db.node.as_ref(),
        "trigger-backfill",
        1,
        Duration::from_secs(10),
    )
    .await;

    let fired = wait_for_last_status(
        db.node.as_ref(),
        "trigger-backfill",
        "fired",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(fired.fire_count, Some(1), "exactly one fire recorded");

    agent.shutdown().await;
}

/// Re-pointing the trigger's `source_collection` drives the control watcher
/// to reconcile and bump `active_generation`. The EventSource observes the
/// bump at the next `next_fire` tick and reconciles its `desired_collections`
/// set (the exact internal effect is pinned directly by the in-crate
/// `event_source_reconciles_subscriptions_on_generation_bump` test — Task 19).
///
/// Here we pin the externally-observable side of the contract:
///   1. Inserting the trigger bumps `active_generation` past startup and the
///      resolved snapshot classifies the trigger as applied cleanly.
///   2. Updating the trigger's `source_collection` drives **another** gen
///      bump with `last_reconcile_result = "applied"`, which is the signal
///      the EventSource receives via `snapshot_rx.changed()` to reconcile
///      its subscription set.
///   3. Post-flip WebhookEvent creates do NOT fire (the trigger no longer
///      resolves to `source_collection = WebhookEvent`). The positive side
///      of the flip (AuditEvent creates now firing the trigger) is covered
///      by `fires_on_matching_source_doc_create` for a fresh collection and
///      by the in-crate reconcile test for the internal subscription swap;
///      reproducing the end-to-end cross-collection flip here would add a
///      timing-sensitive dependency on the event-bus delivery ordering
///      immediately across a reconcile tick that adds nothing over the
///      in-crate coverage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscription_reconciles_on_generation_bump() {
    let db = test_db("trigger-conformance-subscription-reconcile").await;
    register_webhook_event_schema(db.node.as_ref()).await;
    register_audit_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(
        &db,
        "trigger-conformance-subscription-reconcile",
        "backend-subscription-reconcile",
    )
    .await;
    let startup_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-reconcile",
        &agent.default_behavior_id,
        "prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-reconcile",
        "task-reconcile",
        "WebhookEvent",
        "created",
        None,
        true,
        "serial",
    )
    .await;
    let post_insert_snap = wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > startup_gen && snap.last_reconcile_result == "applied"
    })
    .await;
    assert!(
        post_insert_snap.active_generation > startup_gen,
        "active_generation must bump after EventTrigger insert"
    );

    update_event_trigger_source_collection(db.node.as_ref(), "trigger-reconcile", "AuditEvent")
        .await;
    let post_flip_snap = wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > post_insert_snap.active_generation
            && snap.last_reconcile_result == "applied"
    })
    .await;
    assert!(
        post_flip_snap.active_generation > post_insert_snap.active_generation,
        "active_generation must bump again after source_collection flip"
    );

    let row = fetch_event_trigger_row(db.node.as_ref(), "trigger-reconcile")
        .await
        .expect("EventTrigger doc present");
    assert_eq!(
        row.source_collection.as_deref(),
        Some("AuditEvent"),
        "post-flip source_collection must be AuditEvent: {row:?}"
    );

    let before_flip =
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-reconcile", "event").await;
    let _ = write_webhook_event(db.node.as_ref(), "post-flip-webhook", "any").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after_webhook =
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-reconcile", "event").await;
    assert_eq!(
        after_webhook, before_flip,
        "post-flip WebhookEvent must not fire trigger-reconcile \
         (subscription set has moved to AuditEvent)"
    );

    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn template_render_failure_records_error_status() {
    let db = test_db("trigger-conformance-render-err").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(&db, "trigger-conformance-render-err", "backend-render-err").await;
    let initial_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-render-err",
        &agent.default_behavior_id,
        "{{ event.missing_field }}",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-render-err",
        "task-render-err",
        "WebhookEvent",
        "created",
        None,
        true,
        "serial",
    )
    .await;
    wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > initial_gen && snap.last_reconcile_result == "applied"
    })
    .await;

    let _ = write_webhook_event(db.node.as_ref(), "ext-render-err", "any").await;

    let errored = wait_for_last_status(
        db.node.as_ref(),
        "trigger-render-err",
        "error",
        Duration::from_secs(10),
    )
    .await;
    assert!(
        !errored.last_error.as_deref().unwrap_or("").is_empty(),
        "last_error must carry a render-failure reason: {errored:?}"
    );
    assert_eq!(
        errored.fire_count.unwrap_or(0),
        0,
        "render failure must not bump fire_count"
    );
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-render-err", "event").await,
        0,
        "render failure must not materialize an AgentRequest"
    );

    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_triggers_same_source_collection_each_evaluate_filter_independently() {
    let db = test_db("trigger-conformance-two-filters").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(
        &db,
        "trigger-conformance-two-filters",
        "backend-two-filters",
    )
    .await;
    let initial_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-two-a",
        &agent.default_behavior_id,
        "prompt-a",
    )
    .await;
    create_task(
        db.node.as_ref(),
        "task-two-b",
        &agent.default_behavior_id,
        "prompt-b",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-two-a",
        "task-two-a",
        "WebhookEvent",
        "created",
        Some(r#"{ kind: { _eq: "signup" } }"#),
        true,
        "serial",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-two-b",
        "task-two-b",
        "WebhookEvent",
        "created",
        Some(r#"{ kind: { _eq: "login" } }"#),
        true,
        "serial",
    )
    .await;
    wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > initial_gen && snap.last_reconcile_result == "applied"
    })
    .await;

    let _ = write_webhook_event(db.node.as_ref(), "ext-signup", "signup").await;

    wait_for_request_count(
        db.node.as_ref(),
        "trigger-two-a",
        1,
        Duration::from_secs(10),
    )
    .await;
    assert_no_request_within(db.node.as_ref(), "trigger-two-b", Duration::from_secs(2)).await;

    let a = wait_for_last_status(
        db.node.as_ref(),
        "trigger-two-a",
        "fired",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(a.fire_count, Some(1));
    let b = fetch_event_trigger_row(db.node.as_ref(), "trigger-two-b")
        .await
        .expect("EventTrigger B row present");
    assert_eq!(
        b.fire_count.unwrap_or(0),
        0,
        "trigger B must not have fired for a signup event"
    );
    assert_eq!(b.last_status, None);

    agent.shutdown().await;
}
