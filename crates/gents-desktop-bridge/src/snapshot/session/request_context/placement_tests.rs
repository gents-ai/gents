use super::*;

fn placement_entry(
    key: &str,
    request_doc_id: Option<&str>,
) -> gents_protocol::row::CompactionEntryRow {
    serde_json::from_value(serde_json::json!({
        "compaction_key": key,
        "request_doc_id": request_doc_id,
    }))
    .unwrap()
}

/// `(requester, request_doc_id, sequence)` messages in session `session` on `node`.
async fn seed_placement_messages(
    node: &std::sync::Arc<gents::defra_node::Node>,
    messages: &[(String, String, i64)],
) {
    for chunk in messages.chunks(64) {
        let mutations = chunk
            .iter()
            .map(|(requester, request_doc_id, sequence)| {
                let requester = gents::store::graphql::escape_graphql_string(requester);
                let request_doc_id = gents::store::graphql::escape_graphql_string(request_doc_id);
                format!(
                    r#"m{sequence}: create_AgentMessage(input: {{
                    message_key: "{request_doc_id}:{sequence}", session_id: "session",
                    node_did: "node", request_doc_id: "{request_doc_id}",
                    requester_did: "{requester}",
                    publication: {{kind: "request_execution", execution_generation: "generation"}},
                    outcome: "complete", sequence: {sequence}, role: "assistant", blocks: null,
                    created_at: "2026-10-31T00:00:00Z"
                }}) {{ _docID }}"#
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        ConfigAccess::write_local(
            node,
            "test.compaction_placement.seed",
            &format!("mutation {{ {mutations} }}"),
        )
        .await
        .unwrap();
    }
}

fn placement_messages(
    requester: &str,
    request_doc_id: &str,
    sequences: std::ops::Range<i64>,
) -> Vec<(String, String, i64)> {
    sequences
        .map(|sequence| (requester.into(), request_doc_id.into(), sequence))
        .collect()
}

#[tokio::test]
async fn a_start_of_request_compaction_reads_one_first_message_per_request() {
    let node = std::sync::Arc::new(crate::tests::support::node_builder().build().await.unwrap());
    let access = ConfigAccess::local(node.clone());
    access
        .add_collection(gents_protocol::schemas::AGENT_MESSAGE)
        .await
        .unwrap();
    let mut messages = placement_messages("reader", "earlier", 1..3);
    /* another requester's copy of the request is outside the read's scope */
    messages.extend(placement_messages("other", "long", 3..4));
    messages.extend(placement_messages("reader", "long", 4..40));
    messages.extend(placement_messages("reader", "short", 40..43));
    messages.extend((0..33).flat_map(|index| {
        placement_messages(
            "reader",
            &format!("many-{index:02}"),
            100 + index..101 + index,
        )
    }));
    seed_placement_messages(&node, &messages).await;

    let entries = [
        placement_entry("long-start", Some("long")),
        placement_entry("short-start", Some("short")),
        placement_entry("short-again", Some("short")),
        placement_entry("fork-copy", None),
        placement_entry("unwritten", Some("gone")),
    ];
    let reads = crate::tests::support::CountingRead::new(access.clone());
    let starts =
        load_compaction_request_starts(&reads, "node", "session", Some("reader"), &entries)
            .await
            .unwrap();
    assert_eq!(
        starts.into_iter().collect::<BTreeMap<_, _>>(),
        BTreeMap::from([
            ("long-start".to_string(), 4),
            ("short-again".to_string(), 40),
            ("short-start".to_string(), 40),
        ])
    );
    assert_eq!(reads.queries(), 1);

    /* requests past one query's width go to the next query */
    let many = (0..33)
        .map(|index| {
            let request = format!("many-{index:02}");
            placement_entry(&format!("{request}-start"), Some(&request))
        })
        .collect::<Vec<_>>();
    let reads = crate::tests::support::CountingRead::new(access.clone());
    let starts = load_compaction_request_starts(&reads, "node", "session", Some("reader"), &many)
        .await
        .unwrap();
    assert_eq!(starts.len(), 33);
    assert_eq!(starts["many-32-start"], 132);
    assert_eq!(reads.queries(), 2);
    assert!(
        load_compaction_request_starts(&reads, "node", "session", Some("reader"), &[])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(reads.queries(), 2);
    node.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "measures compaction placement reads against a long disposable session"]
async fn compaction_placement_read_benchmark() {
    const REQUESTS: i64 = 128;
    const MESSAGES_PER_REQUEST: i64 = 32;
    const COMPACTED_EVERY: i64 = 4;
    let node = std::sync::Arc::new(crate::tests::support::node_builder().build().await.unwrap());
    let access = ConfigAccess::local(node.clone());
    for sdl in [
        gents_protocol::schemas::AGENT_MESSAGE,
        gents_protocol::schemas::COMPACTION_ENTRY,
    ] {
        access.add_collection(sdl).await.unwrap();
    }
    let messages = (0..REQUESTS)
        .flat_map(|request| {
            let first = request * MESSAGES_PER_REQUEST;
            placement_messages(
                "reader",
                &format!("request-{request:03}"),
                first..first + MESSAGES_PER_REQUEST,
            )
        })
        .collect::<Vec<_>>();
    seed_placement_messages(&node, &messages).await;
    let compacted = (0..REQUESTS)
        .filter(|request| request % COMPACTED_EVERY == COMPACTED_EVERY - 1)
        .collect::<Vec<_>>();
    let entries = compacted
        .iter()
        .map(|request| {
            format!(
                r#"c{request}: create_CompactionEntry(input: {{
                compaction_key: "compaction-{request:03}", session_id: "session",
                node_did: "node", requester_did: "reader",
                request_id: "req-{request:03}", request_doc_id: "request-{request:03}",
                sequence: {request}, messages_compacted: 4,
                compacted_through_sequence: {through},
                created_at: "2026-10-31T00:00:00Z"
            }}) {{ _docID }}"#,
                through = request * MESSAGES_PER_REQUEST - 1,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    ConfigAccess::write_local(
        &node,
        "test.compaction_placement.seed",
        &format!("mutation {{ {entries} }}"),
    )
    .await
    .unwrap();
    let entries = gents_desktop_core::client::load_session_compaction_entries_on(
        &access,
        "session",
        "node",
        Some("reader"),
    )
    .await
    .unwrap();
    assert_eq!(entries.len(), compacted.len());

    /* the read before: the compactions' requests again, then every message of each */
    let scope = r#"node_did: { _eq: "node" }, session_id: { _eq: "session" }, requester_did: { _eq: "reader" }"#;
    let before_entries = format!(
        "query {{ CompactionEntry(filter: {{ {scope} }}) {{ compaction_key request_doc_id }} }}"
    );
    let ids = compacted
        .iter()
        .map(|request| format!(r#""request-{request:03}""#))
        .collect::<Vec<_>>()
        .join(", ");
    let before_messages = format!(
        "query {{ AgentMessage(filter: {{ {scope}, request_doc_id: {{ _in: [{ids}] }} }}) {{ request_doc_id sequence }} }}"
    );
    let before_rows = access.execute(&before_messages).await.unwrap()["data"]["AgentMessage"]
        .as_array()
        .unwrap()
        .clone();
    let mut before_starts = BTreeMap::<String, i64>::new();
    for row in &before_rows {
        let sequence = row["sequence"].as_i64().unwrap();
        before_starts
            .entry(row["request_doc_id"].as_str().unwrap().to_string())
            .and_modify(|first| *first = (*first).min(sequence))
            .or_insert(sequence);
    }
    let reads = crate::tests::support::CountingRead::new(access.clone());
    let after_starts =
        load_compaction_request_starts(&reads, "node", "session", Some("reader"), &entries)
            .await
            .unwrap();
    let after_queries = reads.queries();
    assert_eq!(after_starts.len(), compacted.len());
    for entry in &entries {
        assert_eq!(
            after_starts.get(&entry.compaction_key),
            before_starts.get(entry.request_doc_id.as_deref().unwrap()),
        );
    }

    let mut timings = Vec::new();
    for round in 0..5 {
        for shape in if round % 2 == 0 {
            ["before", "after"]
        } else {
            ["after", "before"]
        } {
            let started = std::time::Instant::now();
            if shape == "before" {
                access.execute(&before_entries).await.unwrap();
                access.execute(&before_messages).await.unwrap();
            } else {
                load_compaction_request_starts(
                    &access,
                    "node",
                    "session",
                    Some("reader"),
                    &entries,
                )
                .await
                .unwrap();
            }
            timings.push(serde_json::json!({
                "shape": shape, "wall_ms": started.elapsed().as_secs_f64() * 1000.0
            }));
        }
    }
    let explain = async |query: &str| {
        access
            .execute(&query.replacen(" {", " @explain(type: execute) {", 1))
            .await
            .unwrap_or_else(|error| serde_json::json!({ "error": error.to_string() }))
    };
    let requests = entries
        .iter()
        .filter_map(|entry| entry.request_doc_id.as_deref())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut after_plans = Vec::new();
    for chunk in requests.chunks(REQUEST_STARTS_PER_QUERY) {
        let query = request_starts_query("node", "session", Some("reader"), chunk);
        after_plans.push(explain(&query).await);
    }
    let report = serde_json::json!({
        "source_revision": std::env::var("GENTS_COMPACTION_PLACEMENT_SOURCE_REVISION").ok(),
        "session_messages": messages.len(),
        "messages_per_request": MESSAGES_PER_REQUEST,
        "compacted_requests": compacted.len(),
        "before": {
            "queries": 2, "agent_message_rows": before_rows.len(),
            "plans": [explain(&before_entries).await, explain(&before_messages).await],
        },
        "after": {
            "queries": after_queries, "agent_message_rows": after_starts.len(),
            "plans": after_plans,
        },
        "timings": timings,
    });
    if let Ok(path) = std::env::var("GENTS_COMPACTION_PLACEMENT_BENCHMARK_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    tracing::info!(report = %report, "compaction placement read benchmark");
    node.shutdown().await.unwrap();
}
