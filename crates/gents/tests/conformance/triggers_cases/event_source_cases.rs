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

    let source_doc_id = write_webhook_event(db.node.as_ref(), "ext-1", "any").await;
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

    // Observe the actual materialized request from the existing live source.
    // This old EventTrigger fixture does not yet fence canonical Trigger lookup.
    let response = db
        .node
        .execute(
            r#"{
        AgentRequest(filter: {
            caused_by_trigger_id: { _eq: "trigger-fires" },
            caused_by_trigger_kind: { _eq: "event" }
        }) { content behavior_id execution_origin caused_by_source_doc_id }
    }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "request projection: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .expect("materialized request rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["content"].as_str(), Some("plain prompt"));
    assert_eq!(
        rows[0]["behavior_id"].as_str(),
        Some(agent.default_behavior_id.as_str())
    );
    assert_eq!(rows[0]["execution_origin"].as_str(), Some("scheduled"));
    // The ProductionMaterializer must stamp the causal source document that
    // the live EventSource delivery selected, not leave lineage half-empty.
    assert_eq!(
        rows[0]["caused_by_source_doc_id"].as_str(),
        Some(source_doc_id.as_str()),
        "materialized request must carry the observed source document id"
    );

    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enabled_false_does_not_fire() {
    let db = test_db("trigger-conformance-disabled").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(&db, "trigger-conformance-disabled", "backend-disabled").await;
    let initial_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

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

    // A matching enabled sibling proves the live source processed this row.
    create_event_trigger(
        db.node.as_ref(),
        "trigger-enabled-control",
        "task-disabled",
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
    // Runtime publication precedes source subscription readiness. Keep offering
    // distinct real events until the enabled sibling observes one; do not infer
    // source readiness from an elapsed delay or from the runtime generation.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut sequence = 0;
    loop {
        let _ =
            write_webhook_event(db.node.as_ref(), &format!("ext-disabled-{sequence}"), "any").await;
        if count_agent_requests_for_trigger(db.node.as_ref(), "trigger-enabled-control", "event")
            .await
            > 0
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "enabled sibling never received an event"
        );
        sequence += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-disabled", "event").await,
        0
    );

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

/// Runtime publication of the changed source collection is observable here.
/// The EventSource owner test separately observes its subscription reconciliation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_collection_change_reconciles_runtime_snapshot() {
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

    // Prove the subscription set followed the flip with a live delivery: a
    // document in the NEW collection must fire the trigger. The EventSource
    // owner test separately observes the subscription reconciliation itself;
    // here the load-bearing claim is that the runtime's desired-collection
    // set (driving both the live subscription and the periodic rescan) was
    // re-derived from the flipped source_collection.
    let _ = write_dynamic_event(db.node.as_ref(), "AuditEvent", "ext-audit-1", "any").await;
    wait_for_request_count(
        db.node.as_ref(),
        "trigger-reconcile",
        1,
        Duration::from_secs(10),
    )
    .await;
    let fired = wait_for_last_status(
        db.node.as_ref(),
        "trigger-reconcile",
        "fired",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(fired.fire_count, Some(1));

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

    let a = wait_for_last_status(
        db.node.as_ref(),
        "trigger-two-a",
        "fired",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(a.fire_count, Some(1));
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-two-b", "event").await,
        0,
        "the nonmatching sibling must not fire for the observed signup delivery"
    );
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

/// Serial concurrency is decided by `ProductionMaterializer::
/// has_active_runtime_request_for_trigger` against persisted `AgentRequest`
/// rows in the active runtime states (`pending`, `claimed`, `processing`),
/// scoped to the behavior's `agent_did` (#605). This live delivery proves the
/// gate decides from the store, not from the source's local seen-set: the
/// pre-seeded active row (a claimed request for the same
/// `(agent_did, trigger_id, trigger_kind)` tuple) must make a real matching
/// source doc skip without materializing, and the same delivery must fire
/// once the store no longer holds an active row. Rendering failure paths are
/// separate: this fire is renderable by construction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serial_delivery_skips_on_active_request_then_fires_when_clear() {
    let db = test_db("trigger-conformance-serial-gate").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(
        &db,
        "trigger-conformance-serial-gate",
        "backend-serial-gate",
    )
    .await;
    let initial_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-serial-gate",
        &agent.default_behavior_id,
        "serial gate prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-serial-gate",
        "task-serial-gate",
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

    // Seed an active (claimed) runtime request for the same trigger tuple
    // through the real store — not through any test-local gate model. The
    // production gate (`has_active_runtime_request_for_trigger`) filters on
    // `(agent_did, caused_by_trigger_id, caused_by_trigger_kind)` plus the
    // active lifecycle states, so this document must carry that exact lineage
    // to be a genuine gate input.
    let in_flight = create_request_with_trigger_lineage(
        db.node.as_ref(),
        &agent.agent_did,
        "serial-gate-inflight",
        "claimed",
        "trigger-serial-gate",
        "event",
    )
    .await;

    let _ = write_webhook_event(db.node.as_ref(), "ext-gate-blocked", "any").await;

    // The gate sees the claimed row and must skip. `last_status` flips to
    // "skipped" through the source's callback writeback owner; the only
    // request for this tuple is the seeded in-flight row — the blocked
    // delivery must not materialize its own.
    let skipped = wait_for_last_status(
        db.node.as_ref(),
        "trigger-serial-gate",
        "skipped",
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(
        skipped.fire_count.unwrap_or(0),
        0,
        "a serial skip must not advance fire_count"
    );
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-serial-gate", "event").await,
        1,
        "a serial skip must not materialize a second AgentRequest; only the seeded \
         in-flight row exists"
    );

    // Set the fixture row terminal through the raw test mutation. The
    // gate must then observe no active row for the tuple, so the same
    // trigger fires on the next matching delivery.
    set_request_lifecycle_state(db.node.as_ref(), &in_flight, "superseded").await;

    let _ = write_webhook_event(db.node.as_ref(), "ext-gate-clear", "any").await;
    wait_for_request_count(
        db.node.as_ref(),
        "trigger-serial-gate",
        2,
        Duration::from_secs(10),
    )
    .await;
    let fired = wait_for_last_status(
        db.node.as_ref(),
        "trigger-serial-gate",
        "fired",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(fired.fire_count, Some(1));

    // The fired request must be the new delivery, not the terminalized seed.
    let response = db
        .node
        .execute(
            r#"{
        AgentRequest(filter: {
            caused_by_trigger_id: { _eq: "trigger-serial-gate" },
            caused_by_trigger_kind: { _eq: "event" },
            lifecycle_state: { _neq: "superseded" }
        }) { content }
    }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "fired-request lookup failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .expect("fired request rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["content"].as_str(), Some("serial gate prompt"));

    agent.shutdown().await;
}

/// The per-group fan-in owner is `EventSource::reconcile_group` plus the
/// `ProductionMaterializer::has_materialized_group_request` marker. This live
/// delivery proves: (1) a complete group fires exactly once with
/// `group.count` / `group.complete` rendered into the request, (2) the
/// correlated AgentRequest lineage lands with the correlation value and the
/// representative source doc, and (3) an overfull group (documents beyond
/// expected_count) fails closed — it is durably quiesced in
/// `EventTriggerGroupState` and never materializes a second request.
///
/// Interleaving note: the members of one run may be delivered in one batch or
/// one at a time. With expected_count=1 an overfull run either never fires
/// (both members observed before the first reconcile) or fires once at
/// cardinality one (members observed separately); both admissible interleavings
/// end with at most one request and a durable quiescence row whose reason
/// reports the overfull count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_group_delivery_fires_complete_group_and_quiesces_overfull_group() {
    let db = test_db("trigger-conformance-group-live").await;
    register_group_member_schema(db.node.as_ref()).await;

    let agent = boot_agent(&db, "trigger-conformance-group-live", "backend-group-live").await;
    let initial_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-group-live",
        &agent.default_behavior_id,
        "group run {{ group.correlation_value }} complete={{ group.complete }}",
    )
    .await;
    create_per_group_event_trigger(
        db.node.as_ref(),
        "trigger-group-live",
        "task-group-live",
        "GroupMember",
        2,
    )
    .await;
    create_per_group_event_trigger(
        db.node.as_ref(),
        "trigger-group-overfull",
        "task-group-live",
        "GroupMember",
        1,
    )
    .await;
    wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > initial_gen && snap.last_reconcile_result == "applied"
    })
    .await;

    // Two members sharing run "run-live" complete the expected_count=2 group.
    // A per-group reconcile queries the whole correlation, so the fire observes
    // both members regardless of delivery interleaving.
    write_group_member(db.node.as_ref(), "run-live", "first").await;
    write_group_member(db.node.as_ref(), "run-live", "second").await;
    wait_for_request_count(
        db.node.as_ref(),
        "trigger-group-live",
        1,
        Duration::from_secs(15),
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let fired = loop {
        let row = fetch_event_trigger_row(db.node.as_ref(), "trigger-group-live")
            .await
            .expect("EventTrigger group row present");
        if row.fire_count == Some(1) {
            break row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "group fire writeback did not arrive: {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        fired.fire_count,
        Some(1),
        "a complete group must advance fire_count exactly once: {fired:?}"
    );

    let response = db
        .node
        .execute(
            r#"{
        AgentRequest(filter: {
            caused_by_trigger_id: { _eq: "trigger-group-live" },
            caused_by_trigger_kind: { _eq: "event" }
        }) { content caused_by_correlation caused_by_source_doc_id }
    }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "group request projection: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .expect("group request rows");
    assert_eq!(
        rows.len(),
        1,
        "a complete group fires exactly once: {rows:?}"
    );
    assert_eq!(
        rows[0]["content"].as_str(),
        Some("group run run-live complete=true"),
        "the fire must render through the group.* template scope"
    );
    assert_eq!(
        rows[0]["caused_by_correlation"].as_str(),
        Some("run-live"),
        "the correlated group must stamp its correlation onto the request"
    );
    assert!(
        rows[0]["caused_by_source_doc_id"].as_str().is_some(),
        "the group's representative source doc must be recorded as lineage"
    );

    // An overfull run for the expected_count=1 trigger: two members sharing
    // one correlation exceed the declared cardinality. Reconcile must fail
    // closed — durably quiescing the group instead of firing the overflow.
    //
    // Interleaving admissibility: if each member is reconciled separately,
    // the first observation sees exactly one document (== expected_count)
    // and legitimately fires once; the second observation then sees two
    // documents and must quiesce without firing again. The load-bearing
    // claims are the durable quiescence row above and at most one fired
    // request for THIS correlation — counted with the correlation filter so
    // the run-live group's own cardinality-one fire for this trigger cannot
    // leak into the assertion.
    write_group_member(db.node.as_ref(), "run-overfull", "one").await;
    write_group_member(db.node.as_ref(), "run-overfull", "two").await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let response = db
            .node
            .execute(
                r#"{
            EventTriggerGroupState(filter: {
                trigger_id: { _eq: "trigger-group-overfull" },
                correlation: { _eq: "run-overfull" }
            }) { quiesced_at quiesced_reason }
        }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "group-state query failed: {:?}",
            response.errors
        );
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("EventTriggerGroupState"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        if rows.iter().any(|row| {
            row.get("quiesced_at").is_some_and(|value| !value.is_null())
                && row["quiesced_reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("exceeds expected count"))
        }) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "an overfull group was never durably quiesced: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // Count only the run-overfull correlation: the run-live members also
    // reconcile against this expected_count=1 trigger and may legitimately
    // fire once at cardinality one before the second member lands, so an
    // unscoped count could see that admissible fire too.
    let overfull_response = db
        .node
        .execute(
            r#"{
        AgentRequest(filter: {
            caused_by_trigger_id: { _eq: "trigger-group-overfull" },
            caused_by_trigger_kind: { _eq: "event" },
            caused_by_correlation: { _eq: "run-overfull" }
        }) { _docID }
    }"#,
        )
        .await;
    assert!(
        !overfull_response.has_errors(),
        "overfull-count query failed: {:?}",
        overfull_response.errors
    );
    let overfull_rows = overfull_response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        overfull_rows.len() <= 1,
        "an overfull group must not fire its overflow; at most the first complete \
         cardinality-one observation may have fired before the overfill landed: \
         {overfull_rows:?}"
    );

    agent.shutdown().await;
}
