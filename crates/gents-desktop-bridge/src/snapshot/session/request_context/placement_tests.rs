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
                let requester = gents_store::graphql::escape_graphql_string(requester);
                let request_doc_id = gents_store::graphql::escape_graphql_string(request_doc_id);
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
    gents_store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let access = ConfigAccess::local(node.clone());
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
