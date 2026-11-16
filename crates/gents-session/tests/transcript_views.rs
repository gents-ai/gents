use gents_protocol::document_query::{
    fields::{
        agent_tool_call as tool, compaction_entry as compaction,
        provider_context_reduction as reduction,
    },
    Create, Input,
};
use gents_protocol::output::rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::output::{
    MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource, OutputWriter,
    SourceClose, TranscriptMessage,
};
use gents_protocol::session::SessionScope;
use gents_session::views::reductions::ReductionRows;
use gents_session::views::transcript::{
    CompactionRows, MessageRows, Scope, SegmentRows, SequenceRange, ToolCallRows,
};
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{GrantSet, NodeIdentity};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Window {
    All,
    Before,
    At,
    After,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Source {
    Inference,
    Title,
    Tool,
    Authored,
}

fn source(kind: Source, same: bool) -> OutputSource {
    let number = if same { 1 } else { 2 };
    match kind {
        Source::Inference => OutputSource::ProviderTurn {
            scope: "inference.1".parse().unwrap(),
            turn_index: number,
            attempt: 0,
        },
        Source::Title => OutputSource::ProviderTurn {
            scope: "title.1".parse().unwrap(),
            turn_index: number,
            attempt: 0,
        },
        Source::Tool => OutputSource::ToolCall {
            tool_call_doc_id: format!("tool-{number}"),
        },
        Source::Authored => OutputSource::Authored {
            key: format!("authored-{number}"),
        },
    }
}

#[tokio::test]
async fn transcript_views_match_model_on_served_node() -> anyhow::Result<()> {
    let db = gents_inference::test_support::test_db(
        "transcript-views",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let owner = ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse()?);
    let reader = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("transcript-view-reader")
    })
    .await?;
    let stranger = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("transcript-view-stranger")
    })
    .await?;
    let reader_did = reader.did().parse()?;
    GrantSet::OperatorUser.grant(&owner, &reader_did).await?;
    GrantSet::OperatorUser
        .grant(&owner, &stranger.did().parse()?)
        .await?;
    for sequence in 1..=3 {
        let message = TranscriptMessage {
            message_key: format!("view-message-{sequence}"),
            session_id: "view-session".into(),
            node_did: db.node_identity.did().into(),
            requester_did: Some(reader.did().into()),
            request_doc_id: Some("view-request".into()),
            publication: MessagePublication::RequestExecution {
                execution_generation: "view-generation".into(),
            },
            outcome: OutputOutcome::Complete,
            sequence,
            role: MessageRole::User,
            native_id: None,
            native_request_id: None,
            blocks: Vec::new(),
            created_at: "2026-11-12T00:00:00Z".into(),
        };
        let response = owner
            .write_with_variables(
                "test.transcript_views.message",
                CREATE_AGENT_MESSAGE_MUTATION,
                &transcript_message_create_variables(&message)?,
            )
            .await?;
        let doc = gents_store::graphql::created_doc_id(&response, "AgentMessage")?;
        owner
            .add_actor_relationship("AgentMessage", &doc, "reader", &reader_did)
            .await?;
    }
    let mut segment_ids = Vec::new();
    for kind in [
        Source::Inference,
        Source::Title,
        Source::Tool,
        Source::Authored,
    ] {
        let segment = OutputSegment {
            node_did: db.node_identity.did().into(),
            requester_did: Some(reader.did().into()),
            session_id: "view-session".into(),
            request_doc_id: "view-request".into(),
            source: source(kind, true),
            writer: OutputWriter::RequestExecution {
                execution_generation: "view-generation".into(),
            },
            ordinal: None,
            runs: Vec::new(),
            payload: String::new(),
            close: Some(SourceClose::Retracted),
            created_at: "2026-11-12T00:00:00Z".into(),
        };
        let response = owner
            .write_with_variables(
                "test.transcript_views.segment",
                CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                &output_segment_create_variables(&segment)?,
            )
            .await?;
        let doc = gents_store::graphql::created_doc_id(&response, "AgentOutputSegment")?;
        owner
            .add_actor_relationship("AgentOutputSegment", &doc, "reader", &reader_did)
            .await?;
        segment_ids.push(doc);
    }
    let tool = Create::new(
        Input::new()
            .set(tool::TOOL_CALL_KEY, "view-tool")
            .set(tool::NODE_DID, db.node_identity.did())
            .set(tool::SESSION_ID, "view-session")
            .set(tool::REQUESTER_DID, reader.did())
            .set(tool::REQUEST_DOC_ID, "view-request")
            .set(tool::MESSAGE_SEQUENCE, 2_i64),
    )
    .render();
    let compaction = Create::new(
        Input::new()
            .set(compaction::COMPACTION_KEY, "view-compaction")
            .set(compaction::NODE_DID, db.node_identity.did())
            .set(compaction::SESSION_ID, "view-session")
            .set(compaction::REQUESTER_DID, reader.did())
            .set(compaction::REQUEST_DOC_ID, "view-request")
            .set(compaction::SEQUENCE, 2_i64),
    )
    .render();
    let reduction = Create::new(
        Input::new()
            .set(reduction::REDUCTION_KEY, "view-reduction")
            .set(reduction::NODE_DID, db.node_identity.did())
            .set(reduction::SESSION_ID, "view-session")
            .set(reduction::REQUESTER_DID, reader.did())
            .set(reduction::REQUEST_ID, "view-request-id")
            .set(reduction::REQUEST_DOC_ID, "view-request")
            .set(reduction::REQUEST_COMMIT_CID, "view-commit")
            .set(reduction::REDUCTION_INDEX, 0_i64)
            .set(reduction::TURN_INDEX, 0_i64)
            .set(reduction::SOURCE_BOUNDARY_JSON, "{}")
            .set(reduction::COMPACTED_PREFIX_JSON, "[]")
            .set(reduction::RETAINED_SUFFIX_JSON, "[]")
            .set(reduction::PAIR_CLOSED, true)
            .set(reduction::CHECKPOINT_MESSAGES_JSON, "[]")
            .set(reduction::REPLAY_ASSOCIATIONS_JSON, "{}")
            .set(reduction::SUMMARY, "view summary")
            .set(reduction::MESSAGES_COMPACTED, 0_i64)
            .set(reduction::ORIGINAL_TOKENS, 0_i64)
            .set(reduction::COMPACTED_TOKENS, 0_i64)
            .set(reduction::CREATED_AT, "2026-11-12T00:00:00Z"),
    )
    .render();
    for (collection, create) in [
        ("AgentToolCall", tool),
        ("CompactionEntry", compaction),
        ("ProviderContextReduction", reduction),
    ] {
        let response = owner
            .write_rendered("test.transcript_views.context", &create)
            .await?;
        let doc = gents_store::graphql::created_doc_id(&response, collection)?;
        owner
            .add_actor_relationship(collection, &doc, "reader", &reader_did)
            .await?;
    }
    let scope = Scope::Session(SessionScope {
        node_did: db.node_identity.did().into(),
        session_id: "view-session".into(),
        requester_did: Some(reader.did().into()),
    });
    let http = db.client_access(&reader);
    let denied = db.client_access(&stranger);
    let visibility = gents_lean_vocab::families::transcript_views_visible::cases();
    assert_eq!(visibility.len(), 32);
    for case in visibility {
        let input = &case.input;
        let mut view = MessageRows::new(Scope::Session(SessionScope {
            node_did: if input.same_node {
                db.node_identity.did().into()
            } else {
                "other-node".into()
            },
            session_id: if input.same_session {
                "view-session".into()
            } else {
                "other-session".into()
            },
            requester_did: Some(if input.same_requester {
                reader.did().into()
            } else {
                stranger.did().into()
            }),
        }));
        view.message_key = Some("view-message-2".into());
        view.request_doc_ids = Some(vec![if input.same_request {
            "view-request".into()
        } else {
            "other-request".into()
        }]);
        let access = if input.readable { &http } else { &denied };
        case.check(!access.read_view(&view).await?.is_empty());
        let mut segments = SegmentRows::new(view.scope.clone());
        segments.document_ids = Some(vec![segment_ids[0].clone()]);
        segments.request_doc_ids = view.request_doc_ids.clone();
        case.check(!access.read_view(&segments).await?.is_empty());
        let mut tools = ToolCallRows::new(view.scope.clone());
        tools.request_doc_ids = view.request_doc_ids.clone();
        case.check(!access.read_view(&tools).await?.is_empty());
        let mut compactions = CompactionRows::new(view.scope.clone());
        compactions.request_doc_ids = view.request_doc_ids.clone();
        case.check(!access.read_view(&compactions).await?.is_empty());
        let mut reductions = ReductionRows::new(view.scope.clone());
        reductions.request_doc_ids = view.request_doc_ids.clone();
        case.check(!access.read_view(&reductions).await?.is_empty());
    }
    let windows = gents_lean_vocab::families::transcript_views_window::cases::<Window>();
    assert_eq!(windows.len(), 4);
    for case in windows {
        let mut view = MessageRows::new(scope.clone());
        view.sequence = match case.input.selection {
            Window::All => SequenceRange::All,
            Window::Before => SequenceRange::Before(2),
            Window::At => SequenceRange::At(2),
            Window::After => SequenceRange::After(2),
        };
        let rows = http.read_view(&view).await?;
        case.check(
            rows.into_iter()
                .map(|row| u64::from(row.message.sequence))
                .collect(),
        );
    }
    let sources = gents_lean_vocab::families::transcript_views_source_visible::cases::<Source>();
    assert_eq!(sources.len(), 16);
    for case in sources {
        let input = &case.input;
        let index = match input.source {
            Source::Inference => 0,
            Source::Title => 1,
            Source::Tool => 2,
            Source::Authored => 3,
        };
        let mut view = SegmentRows::new(scope.clone());
        view.document_ids = Some(vec![segment_ids[index].clone()]);
        view.sources = vec![source(input.source, input.same_source)];
        view.provider_live_only = input.live_only;
        case.check(!http.read_view(&view).await?.is_empty());
    }
    db.node.shutdown().await?;
    Ok(())
}
