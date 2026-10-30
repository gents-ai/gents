use super::*;

/// Read one immutable call after checking its physical request against the selected session.
pub async fn load_session_context_details(
    core: &ClientCore,
    access: &ConfigAccess,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    request_id: &str,
    call_id: &str,
) -> anyhow::Result<Option<gents::toolset::SessionContextDetails>> {
    let response = access.execute(&format!(
        r#"{{ InferenceCall(filter: {{
            call_id: {{_eq: "{}"}}, node_did: {{_eq: "{}"}},
            request_id: {{_eq: "{}"}}, call_kind: {{_eq: "inference"}}
        }}, limit: 2) {{ request_doc_id request_id call_id call_seq queued_at context_accounting_json }} }}"#,
        gents::graphql::escape_graphql_string(call_id),
        gents::graphql::escape_graphql_string(node_did),
        gents::graphql::escape_graphql_string(request_id),
    )).await?;
    let rows = response
        .pointer("/data/InferenceCall")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("context detail call response omitted its result array"))?;
    let [row] = rows.as_slice() else {
        return Ok(None);
    };
    if !has_context_accounting(row) {
        return Ok(None);
    }
    let context = decode_request_context_row(row)?;
    let Some(physical_id) = context.request_doc_id.as_deref() else {
        return Ok(None);
    };
    let requester = requester_did
        .map(|did| format!("\"{}\"", gents::graphql::escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{
            _docID: {{_eq: "{}"}}, purpose: {{_eq: "normal"}}, node_did: {{_eq: "{}"}},
            session_id: {{_eq: "{}"}}, request_id: {{_eq: "{}"}},
            requester_did: {{_eq: {requester}}}
        }}, limit: 2) {{_docID}} }}"#,
            gents::graphql::escape_graphql_string(physical_id),
            gents::graphql::escape_graphql_string(node_did),
            gents::graphql::escape_graphql_string(session_id),
            gents::graphql::escape_graphql_string(request_id),
        ))
        .await?;
    let owners = response
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            anyhow::anyhow!("context detail request response omitted its result array")
        })?;
    if owners.len() != 1 {
        return Ok(None);
    }
    core.session_context_details(
        access,
        node_did,
        requester_did,
        session_id,
        physical_id,
        &gents::toolset::LastRequestContextSnapshot {
            request_id: context.request_id.clone(),
            call_id: context.call_id.clone(),
            call_sequence: context.call_sequence,
            queued_at: context.queued_at.clone(),
            accounting: context.accounting,
        },
    )
    .await
}

pub(super) async fn load_latest_session_request_context(
    access: &gents::config_client::ConfigAccess,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> anyhow::Result<Option<LoadedRequestContext>> {
    let rows = gents::toolset::load_latest_session_context_calls(
        access,
        node_did,
        requester_did,
        session_id,
    )
    .await?;
    decode_latest_request_context(&rows)
}

/// `request_doc_ids` come from the scoped transcript page, so each one has a
/// canonical message anchor and needs no second AgentRequest scope check.
pub(super) async fn load_session_request_context_history(
    access: &gents::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_doc_ids: &[String],
) -> anyhow::Result<Vec<LoadedRequestContext>> {
    if request_doc_ids.is_empty() {
        return Ok(Vec::new());
    }
    let physical_ids = request_doc_ids
        .iter()
        .map(|doc_id| format!(r#""{}""#, gents::graphql::escape_graphql_string(doc_id)))
        .collect::<Vec<_>>()
        .join(", ");
    let requester = requester_did
        .map(gents::graphql::escape_graphql_string)
        .map(|did| format!("\"{did}\""))
        .unwrap_or_else(|| "null".to_string());
    let query = format!(
        r#"query {{
            AgentMessage(filter: {{
                node_did: {{ _eq: "{node_did}" }},
                session_id: {{ _eq: "{session_id}" }},
                requester_did: {{ _eq: {requester} }},
                request_doc_id: {{ _in: [{physical_ids}] }}
            }}) {{ request_doc_id sequence }}
            InferenceCall(
                filter: {{ _and: [
                    {{ node_did: {{ _eq: "{node_did}" }} }},
                    {{ call_kind: {{ _eq: "inference" }} }},
                    {{ request_doc_id: {{ _in: [{physical_ids}] }} }}
                ] }},
                order: [{{ queued_at: ASC }}, {{ request_doc_id: ASC }}, {{ call_seq: ASC }}, {{ call_id: ASC }}]
            ) {{ request_doc_id request_id call_id call_seq queued_at context_accounting_json }}
        }}"#,
        node_did = gents::graphql::escape_graphql_string(node_did),
        session_id = gents::graphql::escape_graphql_string(session_id),
    );
    let data = access.execute(&query).await?;
    let result = |collection: &str| {
        data.pointer(&format!("/data/{collection}"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("page {collection} response omitted its result array"))
    };
    let mut anchors = std::collections::BTreeMap::<&str, i64>::new();
    for row in result("AgentMessage")? {
        let (Some(doc_id), Some(sequence)) = (
            row.get("request_doc_id")
                .and_then(serde_json::Value::as_str),
            row.get("sequence").and_then(serde_json::Value::as_i64),
        ) else {
            anyhow::bail!("page AgentMessage row has no request anchor");
        };
        anchors
            .entry(doc_id)
            .and_modify(|current| *current = (*current).min(sequence))
            .or_insert(sequence);
    }
    result("InferenceCall")?
        .iter()
        .filter(|row| has_context_accounting(row))
        .map(|row| {
            let mut context = decode_request_context_row(row)?;
            context.anchor_sequence = context
                .request_doc_id
                .as_deref()
                .and_then(|doc_id| anchors.get(doc_id).copied());
            Ok(context)
        })
        .collect()
}

pub(super) async fn load_session_reductions(
    access: &gents::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> anyhow::Result<Vec<LoadedReduction>> {
    let requester = requester_did
        .map(gents::graphql::escape_graphql_string)
        .map(|did| format!("\"{did}\""))
        .unwrap_or_else(|| "null".to_string());
    let query = format!(
        r#"query {{
            ProviderContextReduction(
                filter: {{
                    node_did: {{ _eq: "{node_did}" }},
                    session_id: {{ _eq: "{session_id}" }},
                    requester_did: {{ _eq: {requester} }}
                }},
                order: [{{ created_at: ASC }}, {{ reduction_key: ASC }}]
            ) {{
                reduction_key source_boundary_json summary messages_compacted
                original_tokens compacted_tokens created_at
            }}
        }}"#,
        node_did = gents::graphql::escape_graphql_string(node_did),
        session_id = gents::graphql::escape_graphql_string(session_id),
    );
    let data = access.execute(&query).await?;
    data.pointer("/data/ProviderContextReduction")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            anyhow::anyhow!("ProviderContextReduction response omitted its result array")
        })?
        .iter()
        .map(decode_reduction_row)
        .collect()
}

fn decode_reduction_row(row: &serde_json::Value) -> anyhow::Result<LoadedReduction> {
    let string = |field: &str| {
        row.get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let int = |field: &str| row.get(field).and_then(serde_json::Value::as_i64);
    let reduction_key = string("reduction_key")
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("ProviderContextReduction row has no reduction_key"))?;
    let anchor_sequence = string("source_boundary_json")
        .and_then(|json| {
            serde_json::from_str::<gents::provider_context_reduction::SourceBoundary>(&json).ok()
        })
        .and_then(|boundary| boundary.canonical_through)
        .map(|fact| fact.sequence);
    Ok(LoadedReduction {
        reduction_key,
        anchor_sequence,
        summary: string("summary"),
        messages_compacted: int("messages_compacted"),
        original_tokens: int("original_tokens"),
        compacted_tokens: int("compacted_tokens"),
        created_at: string("created_at"),
    })
}

fn has_context_accounting(row: &serde_json::Value) -> bool {
    row.get("context_accounting_json")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
}

pub(super) fn decode_latest_request_context(
    rows: &[serde_json::Value],
) -> anyhow::Result<Option<LoadedRequestContext>> {
    rows.iter()
        .filter(|row| has_context_accounting(row))
        .max_by(|left, right| {
            left.get("queued_at")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .cmp(
                    right
                        .get("queued_at")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                )
                .then_with(|| {
                    left.get("call_seq")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                        .cmp(
                            &right
                                .get("call_seq")
                                .and_then(serde_json::Value::as_i64)
                                .unwrap_or_default(),
                        )
                })
                .then_with(|| {
                    left.get("call_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .cmp(
                            right
                                .get("call_id")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default(),
                        )
                })
        })
        .map(decode_request_context_row)
        .transpose()
}

fn decode_request_context_row(row: &serde_json::Value) -> anyhow::Result<LoadedRequestContext> {
    let required_string = |field: &str| {
        row.get(field)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("InferenceCall accounting row has no {field}"))
    };
    let encoded = required_string("context_accounting_json")?;
    Ok(LoadedRequestContext {
        request_id: required_string("request_id")?,
        request_doc_id: required_string("request_doc_id").ok(),
        anchor_sequence: None,
        call_id: required_string("call_id")?,
        call_sequence: row
            .get("call_seq")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("InferenceCall accounting row has no call_seq"))?,
        queued_at: row
            .get("queued_at")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        accounting: serde_json::from_str(&encoded)
            .map_err(|error| anyhow::anyhow!("decoding context_accounting_json: {error}"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn context_details_reject_foreign_physical_requests_before_reading_capture() {
        use gents_desktop_core::client::{ClientCoreOptions, DesktopPaths};
        use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};
        fn query_contains(fragment: &'static str) -> impl wiremock::Match {
            move |request: &wiremock::Request| {
                request
                    .body_json::<serde_json::Value>()
                    .ok()
                    .and_then(|body| body["query"].as_str().map(|query| query.contains(fragment)))
                    .unwrap_or(false)
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let core = std::sync::Arc::new(
            ClientCore::start_with_paths_and_options(
                DesktopPaths::from_root(directory.path()),
                ClientCoreOptions::local_only(),
            )
            .await
            .unwrap(),
        );
        let server = MockServer::start().await;
        let accounting = serde_json::json!({
            "accounting_version":gents_protocol::rendered_request::CONTEXT_ACCOUNTING_VERSION,
            "turn_index":0, "attempt":0, "estimator":"openai_chat_wire_json_bytes_div_4_v1",
            "components":{"messages":1,"documents":0,"tool_schemas":0,"additional_parameters":0,"output_schema":0},
            "estimated_input_tokens":1, "context_window":10000,
            "compaction_threshold_basis_points":7500,"compaction_threshold_tokens":7500,
            "compaction_reason":"below_threshold"
        });
        Mock::given(method("POST")).and(query_contains("InferenceCall("))
            .and(query_contains("call_id: {_eq: \"call\"}"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data":{"InferenceCall":[{
                "request_doc_id":"foreign-physical", "request_id":"request", "call_id":"call", "call_seq":1,
                "context_accounting_json":accounting.to_string()
            }]}}))).expect(1).mount(&server).await;
        Mock::given(method("POST"))
            .and(query_contains("AgentRequest("))
            .and(query_contains("_docID: {_eq: \"foreign-physical\"}"))
            .and(query_contains("requester_did: {_eq: \"requester\"}"))
            .and(query_contains("session_id: {_eq: \"session\"}"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data":{"AgentRequest":[]}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let details = load_session_context_details(
            &core,
            &ConfigAccess::graphql(server.uri()),
            "node",
            Some("requester"),
            "session",
            "request",
            "call",
        )
        .await
        .unwrap();
        assert!(details.is_none());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        core.shutdown().await.unwrap();
        server.verify().await;
    }

    fn reduction(key: &str, through: Option<i64>, created_at: &str) -> LoadedReduction {
        let boundary = serde_json::json!({
            "boundary_version": 2,
            "request_doc_id": "physical-req-1",
            "request_commit_cid": "cid",
            "canonical_through": through.map(|sequence| serde_json::json!({
                "doc_id": "message", "sequence": sequence, "commit_cid": "cid"
            })),
        });
        decode_reduction_row(&serde_json::json!({
            "reduction_key": key,
            "source_boundary_json": boundary.to_string(),
            "summary": "# Continuation checkpoint\n\nWrite snake stories.",
            "messages_compacted": 12,
            "original_tokens": 16523,
            "compacted_tokens": 516,
            "created_at": created_at,
        }))
        .unwrap()
    }

    #[test]
    fn a_mid_request_compaction_is_marked_after_the_last_message_it_covered() {
        let page = [12_i64, 13, 14].into_iter().collect::<RapidHashSet<_>>();
        let mut items = Vec::new();
        attach_reduction_compaction_timeline_events(
            &mut items,
            &[
                reduction("covered", Some(13), "2026-10-29T21:14:59.563619Z"),
                reduction("older-page", Some(4), "2026-10-29T21:00:00Z"),
                reduction("no-boundary", None, "2026-10-29T21:01:00Z"),
            ],
            &page,
        );
        let [RenderedTimelineItem::SessionCompaction {
            item_key,
            sequence,
            messages_compacted,
            summary,
            files_read,
            ..
        }] = items.as_slice()
        else {
            panic!("expected one compaction marker, got {items:?}");
        };
        assert_eq!(item_key, "request-compaction:covered");
        assert_eq!(*sequence, 13);
        assert_eq!(*messages_compacted, Some(12));
        assert_eq!(
            summary.as_deref(),
            Some("# Continuation checkpoint\n\nWrite snake stories.")
        );
        /* a mid-request compaction records no file lists */
        assert_eq!(files_read, &None);
    }

    #[test]
    fn the_session_counts_both_kinds_of_compaction_oldest_first() {
        let entry: gents_protocol::row::CompactionEntryRow =
            serde_json::from_value(serde_json::json!({
                "compaction_key": "session-entry",
                "sequence": 1,
                "messages_compacted": 4,
                "created_at": "2026-10-29T21:20:00Z"
            }))
            .unwrap();
        let reductions = [reduction(
            "mid-request",
            Some(13),
            "2026-10-29T21:14:59.563619Z",
        )];
        let keys = |views: Vec<SessionCompactionView>| {
            views
                .into_iter()
                .map(|view| view.compaction_key)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            keys(session_compaction_views(
                std::slice::from_ref(&entry),
                Some(&reductions)
            )),
            ["mid-request", "session-entry"]
        );
        /* reductions that failed to load leave the session entries alone */
        assert_eq!(
            keys(session_compaction_views(std::slice::from_ref(&entry), None)),
            ["session-entry"]
        );
    }

    #[test]
    fn a_compacted_call_leaves_the_reduction_summary_once_its_marker_is_shown() {
        let call = |call_seq: i64, reason: &str| {
            let mut context = decode_request_context_row(&serde_json::json!({
                "request_doc_id": "physical-req-1",
                "request_id": "req-1",
                "call_id": format!("call-{call_seq}"),
                "call_seq": call_seq,
                "queued_at": format!("2026-10-29T21:14:5{call_seq}Z"),
                "context_accounting_json": serde_json::json!({
                    "accounting_version": 2, "attempt": 0, "compaction_reason": reason,
                    "compaction_threshold_basis_points": 250,
                    "compaction_threshold_tokens": 25000,
                    "components": {"additional_parameters": 35, "documents": 0,
                        "messages": 2611, "output_schema": 0, "tool_schemas": 8212},
                    "configured_max_output_tokens": 64000, "context_window": 1000000,
                    "effective_max_output_tokens": 64000, "estimated_input_tokens": 10858,
                    "estimator": "claude_messages_wire_json_bytes_div_4_v1",
                    "pre_compaction_input_tokens": 26865, "turn_index": 1
                }).to_string(),
            }))
            .unwrap();
            context.anchor_sequence = Some(12);
            context
        };
        let page = [12_i64].into_iter().collect::<RapidHashSet<_>>();
        let counts = |history, compactions_shown| {
            let mut items = Vec::new();
            attach_context_reduction_timeline_events(&mut items, history, &page, compactions_shown);
            items
                .iter()
                .filter_map(|item| match item {
                    RenderedTimelineItem::RequestContextReductionSummary {
                        compacted_count,
                        repaired_count,
                        ..
                    } => Some((*compacted_count, *repaired_count)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert!(counts(vec![call(3, "compacted")], true).is_empty());
        assert_eq!(
            counts(
                vec![call(2, "provider_view_repaired"), call(3, "compacted")],
                true
            ),
            [(0, 1)]
        );
        /* without the reductions there is no marker, so the summary keeps it */
        assert_eq!(counts(vec![call(3, "compacted")], false), [(1, 0)]);
    }
}
