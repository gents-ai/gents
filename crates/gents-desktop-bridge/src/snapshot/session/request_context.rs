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
) -> anyhow::Result<Option<gents_tools::native::SessionContextDetails>> {
    let calls = access
        .read_view(
            &gents_inference::views::activity::InferenceCalls::new(node_did)
                .context_call(request_id)
                .call_id(call_id),
        )
        .await?;
    let [row] = calls.as_slice() else {
        return Ok(None);
    };
    let row = serde_json::to_value(row)?;
    if !has_context_accounting(&row) {
        return Ok(None);
    }
    let context = decode_request_context_row(&row)?;
    let Some(physical_id) = context.request_doc_id.as_deref() else {
        return Ok(None);
    };
    let scope =
        gents_session::views::transcript::Scope::Session(gents_protocol::session::SessionScope {
            node_did: node_did.to_owned(),
            session_id: session_id.to_owned(),
            requester_did: requester_did.map(str::to_owned),
        });
    let owners = access
        .read_view(
            &gents_session::views::requests::RequestRows::new(scope)
                .public()
                .doc_ids(vec![physical_id.to_owned()])
                .request_ids(vec![request_id.to_owned()])
                .limit(2),
        )
        .await?;
    if owners.len() != 1 {
        return Ok(None);
    }
    core.session_context_details(
        access,
        node_did,
        requester_did,
        session_id,
        physical_id,
        &gents_tools::native::LastRequestContextSnapshot {
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
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> anyhow::Result<Option<LoadedRequestContext>> {
    let rows = gents_tools::native::load_latest_session_context_calls(
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
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_doc_ids: &[String],
) -> anyhow::Result<Vec<LoadedRequestContext>> {
    if request_doc_ids.is_empty() {
        return Ok(Vec::new());
    }
    let scope =
        gents_session::views::transcript::Scope::Session(gents_protocol::session::SessionScope {
            node_did: node_did.to_owned(),
            session_id: session_id.to_owned(),
            requester_did: requester_did.map(str::to_owned),
        });
    let mut messages = gents_session::views::transcript::MessageRows::new(scope);
    messages.request_doc_ids = Some(request_doc_ids.to_vec());
    let calls = gents_inference::views::activity::InferenceCalls::new(node_did)
        .contexts(request_doc_ids.to_vec());
    let (messages, calls) = access
        .read_views(&(
            gents_session::views::transcript::MessageIdentities(messages),
            calls,
        ))
        .await?;
    let mut anchors = std::collections::BTreeMap::<String, i64>::new();
    for row in messages {
        let (Some(doc_id), Some(sequence)) = (row.request_doc_id, row.sequence) else {
            anyhow::bail!("page AgentMessage row has no request anchor");
        };
        anchors
            .entry(doc_id)
            .and_modify(|current| *current = (*current).min(sequence))
            .or_insert(sequence);
    }
    calls
        .into_iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()?
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
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> anyhow::Result<Vec<LoadedReduction>> {
    let rows = access
        .read_view(&gents_session::views::reductions::ReductionRows::new(
            gents_session::views::transcript::Scope::Session(
                gents_protocol::session::SessionScope {
                    node_did: node_did.to_owned(),
                    session_id: session_id.to_owned(),
                    requester_did: requester_did.map(str::to_owned),
                },
            ),
        ))
        .await?;
    rows.into_iter()
        .map(|row| decode_reduction_row(&serde_json::to_value(row)?))
        .collect()
}

/// Requests whose first message one placement query reads; each is a
/// top-level selection, and DefraDB bounds a query level's width.
const REQUEST_STARTS_PER_QUERY: usize = 32;

/// Where each start-of-request compaction happened: the first message of the
/// request it ran for, by compaction key. A compaction whose request is
/// unknown (a fork's copy has none) or has no messages here is left out.
pub(super) async fn load_compaction_request_starts(
    access: &dyn gents_store::config_client::ConfigRead,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    entries: &[gents_protocol::row::CompactionEntryRow],
) -> anyhow::Result<RapidHashMap<String, i64>> {
    let requests = entries
        .iter()
        .filter_map(|entry| entry.request_doc_id.as_deref())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut starts = RapidHashMap::<&str, i64>::new();
    for chunk in requests.chunks(REQUEST_STARTS_PER_QUERY) {
        let views = chunk
            .iter()
            .map(|doc_id| {
                let mut view = gents_session::views::transcript::MessageRows::new(
                    gents_session::views::transcript::Scope::Session(
                        gents_protocol::session::SessionScope {
                            node_did: node_did.to_owned(),
                            session_id: session_id.to_owned(),
                            requester_did: requester_did.map(str::to_owned),
                        },
                    ),
                );
                view.request_doc_ids = Some(vec![(*doc_id).to_owned()]);
                view.limit = Some(1);
                gents_session::views::transcript::MessageIdentities(view)
            })
            .collect::<Vec<_>>();
        let pages = gents_store::views::read_views(access, &views).await?;
        for (doc_id, page) in chunk.iter().zip(pages) {
            if let Some(sequence) = page
                .first()
                .filter(|row| row.request_doc_id.as_deref() == Some(*doc_id))
                .and_then(|row| row.sequence)
            {
                starts.insert(*doc_id, sequence);
            }
        }
    }
    Ok(entries
        .iter()
        .filter_map(|entry| {
            let start = *starts.get(entry.request_doc_id.as_deref()?)?;
            Some((entry.compaction_key.clone(), start))
        })
        .collect())
}

pub(super) struct EntrySummary {
    pub(super) summary: Option<String>,
    pub(super) files_read: Option<String>,
    pub(super) files_modified: Option<String>,
}

/// What the agent kept of the compactions a read draws, by key: only those
/// markers carry their summaries, so a read doesn't carry every one.
#[derive(Default)]
pub(super) struct MarkerSummaries {
    pub(super) entries: RapidHashMap<String, EntrySummary>,
    pub(super) reductions: RapidHashMap<String, Option<String>>,
}

pub(super) async fn load_marker_summaries(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    entry_keys: &[String],
    reduction_keys: &[String],
) -> anyhow::Result<MarkerSummaries> {
    let mut summaries = MarkerSummaries::default();
    if entry_keys.is_empty() && reduction_keys.is_empty() {
        return Ok(summaries);
    }
    let scope =
        gents_session::views::transcript::Scope::Session(gents_protocol::session::SessionScope {
            node_did: node_did.to_owned(),
            session_id: session_id.to_owned(),
            requester_did: requester_did.map(str::to_owned),
        });
    let mut entries = gents_session::views::transcript::CompactionRows::new(scope.clone());
    entries.compaction_keys = Some(entry_keys.to_vec());
    let mut reductions = gents_session::views::reductions::ReductionRows::new(scope);
    reductions.reduction_keys = Some(reduction_keys.to_vec());
    let (entries, reductions) = access.read_views(&(entries, reductions)).await?;
    for row in entries {
        summaries.entries.insert(
            row.compaction_key,
            EntrySummary {
                summary: row.summary,
                files_read: row.files_read,
                files_modified: row.files_modified,
            },
        );
    }
    for row in reductions {
        summaries
            .reductions
            .insert(row.reduction_key, Some(row.summary));
    }
    Ok(summaries)
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
            serde_json::from_str::<gents_protocol::transcript::SourceBoundary>(&json).ok()
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
    async fn context_history_propagates_explicit_database_denial() {
        use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "errors": [{"message":"opaque", "extensions":{"code":"PERMISSION_DENIED"}}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let access = ConfigAccess::graphql(server.uri());
        let mut items = Vec::new();
        let result =
            attach_context_history_rows(&access, "session", "node", None, &[], &mut items).await;
        let Err(error) = result else {
            panic!("explicit denial must be returned");
        };
        assert!(error.permission_denied());
    }

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
                "compacted_through_sequence": 7,
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
                &RapidHashMap::new(),
                Some(&reductions)
            )),
            ["mid-request", "session-entry"]
        );
        /* each names the row that marks it and the message it follows, so
        the meter can open the session there */
        /* a start-of-request entry is placed at its request's first message */
        let starts = [("session-entry".to_string(), 20_i64)]
            .into_iter()
            .collect();
        let placed =
            session_compaction_views(std::slice::from_ref(&entry), &starts, Some(&reductions))
                .into_iter()
                .map(|view| (view.item_key, view.message_sequence))
                .collect::<Vec<_>>();
        assert_eq!(
            placed,
            [
                (Some("request-compaction:mid-request".to_string()), Some(13)),
                (
                    Some("session-compaction:session-entry".to_string()),
                    Some(20)
                ),
            ]
        );
        /* reductions that failed to load leave the session entries alone */
        assert_eq!(
            keys(session_compaction_views(
                std::slice::from_ref(&entry),
                &RapidHashMap::new(),
                None
            )),
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

    fn marker(key: &str, sequence: i64, timestamp: &str) -> (i64, RenderedTimelineItem) {
        (
            sequence,
            RenderedTimelineItem::SessionCompaction {
                item_key: key.into(),
                sequence,
                messages_compacted: None,
                original_tokens: None,
                compacted_tokens: None,
                timestamp: Some(timestamp.into()),
                summary: None,
                files_read: None,
                files_modified: None,
            },
        )
    }

    fn message(key: &str, sequence: i64) -> RenderedTimelineItem {
        RenderedTimelineItem::AssistantMessage {
            request_turn: None,
            item_key: key.into(),
            request_id: None,
            sequence: Some(sequence),
            content: Some(key.into()),
            reasoning: None,
            timestamp: None,
            reconstruction: MessageReconstructionView {
                state: ReconstructionState::Ready,
                error: None,
                denied_dependency_doc_id: None,
            },
        }
    }

    fn keys(items: &[RenderedTimelineItem]) -> Vec<&str> {
        items
            .iter()
            .map(|item| match item {
                RenderedTimelineItem::SessionCompaction { item_key, .. }
                | RenderedTimelineItem::AssistantMessage { item_key, .. }
                | RenderedTimelineItem::PendingUserTurn { item_key, .. } => item_key.as_str(),
                _ => "other",
            })
            .collect()
    }

    #[test]
    fn compactions_after_one_message_read_in_the_order_they_happened() {
        let mut items = vec![message("m13", 13), message("m14", 14)];
        attach_compaction_markers(
            &mut items,
            vec![
                marker("today", 13, "2026-10-30T17:06:48Z"),
                marker("yesterday", 13, "2026-10-29T21:14:59.563619Z"),
            ],
        );
        assert_eq!(keys(&items), ["m13", "yesterday", "today", "m14"]);
    }

    #[test]
    fn a_compaction_follows_its_message_past_a_pending_turn_drawn_ahead_of_it() {
        let pending = RenderedTimelineItem::PendingUserTurn {
            question_reply: None,
            item_key: "pending".into(),
            request_id: "request".into(),
            started_goal_id: None,
            content: "queued".into(),
            captured_context: None,
            attachments: None,
            selected_skill_ids: vec![],
            lifecycle_state: None,
            folded_into_request_id: None,
            origin: None,
            created_at: None,
        };
        let mut items = vec![pending, message("m5", 5), message("m6", 6)];
        attach_compaction_markers(
            &mut items,
            vec![marker("compaction", 5, "2026-10-30T17:00:00Z")],
        );
        assert_eq!(keys(&items), ["pending", "m5", "compaction", "m6"]);
    }

    #[test]
    fn a_start_of_request_compaction_is_placed_where_its_request_began() {
        let entry: gents_protocol::row::CompactionEntryRow =
            serde_json::from_value(serde_json::json!({
                "compaction_key": "entry",
                "compacted_through_sequence": 13
            }))
            .unwrap();
        let starts = [("entry".to_string(), 86_i64)].into_iter().collect();
        assert_eq!(session_compaction_anchor(&entry, &starts), Some(86));
        /* a fork's copy names no request: the last message it compacted */
        assert_eq!(
            session_compaction_anchor(&entry, &RapidHashMap::new()),
            Some(13)
        );
    }

    #[tokio::test]
    async fn a_read_carries_the_summaries_of_the_markers_it_draws_only() {
        use gents_store::identity::NodeIdentity as _;
        let home = tempfile::tempdir().unwrap();
        let identity =
            gents_store::identity::KeyIdentity::load_or_create(home.path().join("node.key"), None)
                .unwrap();
        let reader = identity.did().to_owned();
        let node = std::sync::Arc::new(
            gents::defra_node::Node::builder()
                .with_node_identity_did(&reader)
                .build()
                .await
                .unwrap(),
        );
        gents_store::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        let inputs = vec![
            (
                "mutation($input: CompactionEntryMutationInputArg!) { create_CompactionEntry(input: $input) { _docID } }",
                serde_json::json!({
                    "compaction_key": "drawn", "node_did": reader, "session_id": "session",
                    "requester_did": reader, "summary": "## Goal\nKept.",
                    "files_read": "[\"src/lib.rs\"]"
                }),
            ),
            (
                "mutation($input: ProviderContextReductionMutationInputArg!) { create_ProviderContextReduction(input: $input) { _docID } }",
                serde_json::json!({
                    "reduction_key": "mid", "node_did": reader, "session_id": "session",
                    "requester_did": reader, "request_id": "request", "request_doc_id": "physical",
                    "request_commit_cid": "cid", "reduction_index": 0, "turn_index": 0,
                    "source_boundary_json": "{}", "compacted_prefix_json": "[]",
                    "retained_suffix_json": "[]", "pair_closed": true,
                    "checkpoint_messages_json": "[]", "replay_associations_json": "{}",
                    "summary": "  ", "messages_compacted": 0, "original_tokens": 0,
                    "compacted_tokens": 0, "created_at": "2026-10-30T17:01:00Z"
                }),
            ),
        ];
        ConfigAccess::transact_local_write(&node, None, "test.marker_summaries", |txn| {
            let inputs = inputs.clone();
            Box::pin(async move {
                for (mutation, input) in inputs {
                    txn.execute_with_variables(mutation, &serde_json::json!({"input": input}))
                        .await?;
                }
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
        let access = ConfigAccess::local_as(node, reader.parse().unwrap());
        let mut markers = vec![
            marker("session-compaction:drawn", 4, "2026-10-30T17:00:00Z"),
            marker("request-compaction:mid", 9, "2026-10-30T17:01:00Z"),
        ];
        fill_marker_summaries(&access, "session", &reader, Some(&reader), &mut markers)
            .await
            .expect("marker summaries");
        let summaries = markers
            .iter()
            .map(|(_, item)| match item {
                RenderedTimelineItem::SessionCompaction {
                    summary,
                    files_read,
                    ..
                } => (summary.clone(), files_read.clone()),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            summaries,
            [
                (
                    Some("## Goal\nKept.".into()),
                    Some(vec!["src/lib.rs".into()])
                ),
                /* a blank summary is no summary: nothing to open */
                (None, None),
            ]
        );
    }
}

#[cfg(test)]
#[path = "request_context/placement_tests.rs"]
mod placement_tests;
