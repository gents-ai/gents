use gents_inference::llm::message::{AssistantContent, Message, ToolCall, ToolResult, UserContent};

use gents_lean_vocab::{
    native_claude_reasoning_parts, native_claude_replay_content,
};

fn native_replay_tag(
    tag: &gents_lean_vocab::LeanCanonicalCoordinate,
) -> gents_loop::claude_messages_body::ReplayTag {
    use gents_lean_vocab::LeanCanonicalSource;
    use gents_protocol::output::OutputSource;
    let source = match &tag.source {
        LeanCanonicalSource::Provider {
            scope,
            turn,
            attempt,
        } => OutputSource::ProviderTurn {
            scope: format!("inference.{}", scope.checked_add(1).expect("scope fits"))
                .parse()
                .expect("modeled provider scope"),
            turn_index: (*turn).try_into().expect("modeled turn fits"),
            attempt: (*attempt).try_into().expect("modeled attempt fits"),
        },
        LeanCanonicalSource::Tool { call } => OutputSource::ToolCall {
            tool_call_doc_id: format!("tool-{call}"),
        },
        LeanCanonicalSource::Auxiliary {
            auxiliary_kind,
            scope,
            turn,
            attempt,
        } => OutputSource::ProviderTurn {
            scope: gents_protocol::rendered_request::CaptureScope {
                kind: match auxiliary_kind {
                    gents_lean_vocab::LeanAuxiliaryKind::Compaction => {
                        gents_protocol::rendered_request::CaptureScopeKind::Compaction
                    }
                    gents_lean_vocab::LeanAuxiliaryKind::CompactionFallback => {
                        gents_protocol::rendered_request::CaptureScopeKind::CompactionFallback
                    }
                    gents_lean_vocab::LeanAuxiliaryKind::Title => {
                        gents_protocol::rendered_request::CaptureScopeKind::Title
                    }
                    gents_lean_vocab::LeanAuxiliaryKind::AccessReview => {
                        gents_protocol::rendered_request::CaptureScopeKind::AccessReview
                    }
                },
                seq: *scope,
            },
            turn_index: (*turn).try_into().expect("modeled turn fits"),
            attempt: (*attempt).try_into().expect("modeled attempt fits"),
        },
        LeanCanonicalSource::Authored { key } => OutputSource::Authored {
            key: format!("authored-{key}"),
        },
    };
    gents_loop::claude_messages_body::ReplayTag {
        request_doc_id: format!("request-{}", tag.request),
        source,
    }
}

fn native_replay_origin(
    origin: &gents_lean_vocab::LeanClaudeReplayOrigin,
) -> gents_loop::claude_messages_body::ReplayOrigin {
    use gents_lean_vocab::LeanClaudeReplayOrigin as Lean;
    use gents_loop::claude_messages_body::ReplayOrigin as Native;
    match origin {
        Lean::ClaudeSubscription => Native::ClaudeSubscription,
        Lean::AcceptedProvider => Native::AcceptedProvider,
        Lean::RecoveredProvider => Native::RecoveredProvider,
        Lean::Foreign => Native::Foreign,
        Lean::Missing => Native::Missing,
        Lean::Ambiguous => Native::Ambiguous,
    }
}

fn native_replay_wire(
    wire: gents_lean_vocab::LeanReasoningReplayWire,
) -> gents_loop::claude_messages_body::ReplayWire {
    use gents_lean_vocab::LeanReasoningReplayWire as Lean;
    use gents_loop::claude_messages_body::ReplayWire as Native;
    match wire {
        Lean::ClaudeMessages => Native::ClaudeMessages,
        Lean::Responses => Native::Responses,
    }
}

fn native_replay_resolution(
    entry: &gents_lean_vocab::LeanClaudeCheckpointResolution,
) -> gents_loop::claude_messages_body::ResolvedReplayEvidence {
    use gents_loop::claude_messages_body::{ReplayIssuer, ResolvedReplayEvidence};
    ResolvedReplayEvidence {
        origin: native_replay_origin(&entry.origin),
        issuer: ReplayIssuer {
            family: entry.issuer_family.clone(),
            endpoint: entry.issuer_endpoint.clone(),
        },
        wire: native_replay_wire(entry.wire),
        physical_header: entry.physical_header.clone(),
        complete: entry.complete,
        captured: entry.captured.as_deref().map(native_flat_items),
        reasoning: entry
            .expected_reasoning
            .iter()
            .map(|block| {
                (
                    block.block_index,
                    native_claude_reasoning_parts(&block.parts),
                )
            })
            .collect(),
    }
}

fn native_flat_items(
    items: &[gents_lean_vocab::LeanReplayFlatItem],
) -> Vec<gents_loop::provider_input::replay_frontier::FlatItem> {
    use gents_lean_vocab::LeanReplayFlatKind;
    use gents_loop::provider_input::replay_frontier::FlatItem;
    items
        .iter()
        .map(|item| match item.kind {
            LeanReplayFlatKind::Ordinary => FlatItem::Ordinary(item.bytes.clone()),
            LeanReplayFlatKind::Reasoning => FlatItem::Reasoning(item.bytes.clone()),
        })
        .collect()
}

fn native_tagged_replay_row(
    row: &gents_lean_vocab::LeanClaudeTaggedReplayRow,
) -> gents_loop::claude_messages_body::TaggedAssistantRow {
    gents_loop::claude_messages_body::TaggedAssistantRow {
        source: row.source.as_ref().map(native_replay_tag),
        physical_header: row.physical_header.clone(),
        block_indices: row.block_indices.clone(),
        id: None,
        content: native_claude_replay_content(&row.blocks),
    }
}

fn native_tagged_message(
    row: &gents_lean_vocab::LeanClaudeTaggedReplayRow,
) -> gents_loop::loop_stream::TaggedMessage {
    gents_loop::loop_stream::TaggedMessage {
        message: Message::Assistant {
            request_id: None,
            id: None,
            content: native_claude_replay_content(&row.blocks),
        },
        source: row.source.as_ref().map(native_replay_tag),
        physical_header: row.physical_header.clone(),
        block_indices: row.block_indices.clone(),
    }
}

fn native_modeled_provider_view(
    messages: &[Message],
    rows: &[gents_lean_vocab::LeanClaudeTaggedReplayRow],
) -> Vec<gents_loop::loop_stream::TaggedMessage> {
    let mut modeled = rows.iter();
    let mut announced: std::collections::VecDeque<ToolCall> = std::collections::VecDeque::new();
    let mut view = Vec::new();
    for message in messages {
        match message {
            Message::Assistant { .. } => {
                let tagged =
                    native_tagged_message(modeled.next().expect("one modeled row per assistant"));
                if let Message::Assistant { content, .. } = &tagged.message {
                    announced = content
                        .iter()
                        .filter_map(|item| match item {
                            AssistantContent::ToolCall(call) => Some(call.clone()),
                            _ => None,
                        })
                        .collect();
                }
                view.push(tagged);
            }
            Message::User { content } => {
                let content = content
                    .iter()
                    .map(|item| match item {
                        UserContent::ToolResult(result) => {
                            let call = announced
                                .pop_front()
                                .expect("a modeled tool call for every fixture tool result");
                            UserContent::ToolResult(ToolResult {
                                id: call.id,
                                call_id: call.call_id,
                                content: result.content.clone(),
                            })
                        }
                        other => other.clone(),
                    })
                    .collect();
                view.push(gents_loop::loop_stream::TaggedMessage::unassociated(
                    Message::User { content },
                ));
            }
            other => view.push(gents_loop::loop_stream::TaggedMessage::unassociated(
                other.clone(),
            )),
        }
    }
    assert!(
        modeled.next().is_none(),
        "every modeled row needs an assistant position"
    );
    view
}

#[test]
fn generated_reasoning_suffix_cases_bind_native_selection() {
    use gents_lean_vocab::lean_prompt_assembly_reasoning_suffix_cases;
    use gents_loop::claude_messages_body::{replay_stage, select_replay, ReplayIssuer};

    let cases = lean_prompt_assembly_reasoning_suffix_cases();
    assert!(
        cases.iter().any(|case| case.name.contains("/before-turn-")),
        "Lean emitted no replay scenario steps"
    );
    for case in cases {
        let rows = case
            .rows
            .iter()
            .map(native_tagged_replay_row)
            .collect::<Vec<_>>();
        let issuer = ReplayIssuer {
            family: case.issuer_family.clone(),
            endpoint: case.issuer_endpoint.clone(),
        };
        let wire = native_replay_wire(case.wire);
        let resolutions = case
            .resolutions
            .iter()
            .map(|entry| {
                (
                    native_replay_tag(&entry.tag),
                    native_replay_resolution(entry),
                )
            })
            .collect::<Vec<_>>();
        let resolve = |tag: &gents_loop::claude_messages_body::ReplayTag| {
            resolutions
                .iter()
                .filter(|(candidate, _)| candidate == tag)
                .map(|(_, evidence)| evidence.clone())
                .collect::<Vec<_>>()
        };
        let rows = gents_loop::claude_messages_body::project_recovered_prefix(&rows, resolve);
        let stage = replay_stage(&rows, &issuer, wire, resolve);
        let expected_stage = case
            .stage
            .iter()
            .map(native_tagged_replay_row)
            .collect::<Vec<_>>();
        assert_eq!(stage, expected_stage, "{}: stage rows", case.name);
        let actual = select_replay(
            &rows,
            &issuer,
            wire,
            resolve,
            &native_flat_items(&case.assembled),
        );
        let expected = case
            .replay
            .iter()
            .map(native_tagged_replay_row)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "{}", case.name);
    }
}

fn replay_codec_outcome(
    error: &gents_loop::claude_messages_body::AssistantCodecError,
) -> &'static str {
    use gents_loop::claude_messages_body::AssistantCodecError as Error;
    match error {
        Error::UnsupportedImage => "unsupportedReplayBlock",
        Error::MissingSignature => "missingSignature",
        Error::EmptyRedacted => "malformedRedacted",
        Error::UnsupportedReasoning => "unsupportedReasoning",
        Error::UnencodableToolInput => "unencodableToolInput",
    }
}

fn replay_checkpoint_outcome(
    error: &gents_loop::claude_messages_body::ReplayCheckpointError,
) -> &'static str {
    use gents_loop::claude_messages_body::ReplayCheckpointError as Error;
    match error {
        Error::InvalidSplit => "invalidReplaySplit",
        Error::InvalidAssociation => "invalidReplayAssociation",
        Error::DuplicateAssociation => "duplicateReplayAssociation",
        Error::RequiredInPrefix => "requiredReplayInPrefix",
        Error::MissingRequired => "missingRequiredReplay",
        Error::Codec(error) => replay_codec_outcome(error),
    }
}

#[test]
fn generated_protected_replay_compaction_cases_bind_native_split_and_checkpoint() {
    use gents_lean_vocab::lean_protected_replay_compaction_cases;
    use gents_loop::claude_messages_body::prepare_replay_checkpoint;
    use gents_loop::compaction::history::protected_pair_safe_split_index;

    let cases = lean_protected_replay_compaction_cases();
    assert_eq!(cases.len(), 6);
    for case in cases {
        let roles = super::streaming_compaction::compaction_messages_for_count(case.message_count);
        let assistant_count = roles
            .iter()
            .filter(|message| matches!(message, Message::Assistant { .. }))
            .count();
        assert_eq!(case.rows.len(), assistant_count, "{}", case.name);

        let provider_view = native_modeled_provider_view(&roles, &case.rows);
        let messages = provider_view
            .iter()
            .map(|row| row.message.clone())
            .collect::<Vec<_>>();
        let required = case
            .required
            .iter()
            .map(native_replay_tag)
            .collect::<Vec<_>>();

        let ceiling = match gents_loop::loop_stream::replay_compaction_prefix_bound(
            &provider_view,
            &required,
        ) {
            Err(error) => {
                assert_eq!(
                    replay_checkpoint_outcome(&error),
                    case.ceiling_error,
                    "{}: refused ceiling",
                    case.name
                );
                assert_eq!(case.outcome, "ceiling_refused", "{}", case.name);
                continue;
            }
            Ok(ceiling) => ceiling,
        };
        assert!(
            case.ceiling_error.is_empty(),
            "{}: the model refused this ceiling",
            case.name
        );
        assert_eq!(ceiling, case.ceiling, "{}: derived ceiling", case.name);
        let Some(ceiling) = ceiling else {
            assert_eq!(case.outcome, "unbounded", "{}", case.name);
            continue;
        };

        let split = protected_pair_safe_split_index(&messages, case.raw_index, ceiling);
        assert_eq!(
            split, case.selected_split,
            "{}: pair-safe protected split",
            case.name
        );
        let outcome = match split {
            None => "cannot_fit",
            Some(split) => {
                let assistant_split = messages[..split]
                    .iter()
                    .filter(|message| matches!(message, Message::Assistant { .. }))
                    .count();
                let rows = case.rows.iter().map(native_tagged_replay_row).collect();
                match prepare_replay_checkpoint(
                    case.required.iter().map(native_replay_tag).collect(),
                    rows,
                    assistant_split,
                ) {
                    Ok(_) => "ok",
                    Err(error) => replay_checkpoint_outcome(&error),
                }
            }
        };
        assert_eq!(outcome, case.outcome, "{}: composed checkpoint", case.name);
    }
}

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|path| path.join("crates/gents/proofs/lakefile.lean").exists())
        .expect("repository root should contain crates/gents/proofs/lakefile.lean")
        .to_path_buf()
}

fn production_sources() -> Vec<(String, String)> {
    let root = repo_root();
    let mut out = Vec::new();
    let mut stack = [
        "gents",
        "gents-loop",
        "gents-inference",
        "gents-session",
        "gents-tools",
        "gents-toolcalls",
        "gents-automation",
        "gents-observe",
    ]
    .into_iter()
    .map(|name| root.join("crates").join(name).join("src"))
    .collect::<Vec<_>>();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
        {
            let path = entry.expect("readable dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .expect("path under repo root")
                .to_string_lossy()
                .replace('\\', "/");
            let file_name = relative.rsplit('/').next().unwrap_or("");
            if relative.ends_with("/tests.rs")
                || relative.contains("/tests/")
                || file_name.ends_with("_tests.rs")
                || file_name.ends_with("_test.rs")
                || relative.contains("test_support")
            {
                continue;
            }
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
            out.push((relative, source));
        }
    }
    out.sort();
    out
}

fn files_containing(needles: &[&str]) -> BTreeSet<String> {
    production_sources()
        .into_iter()
        .filter(|(_, source)| needles.iter().any(|needle| source.contains(needle)))
        .map(|(path, _)| path)
        .collect()
}

#[test]
fn provider_invocations_are_confined_to_the_owned_loop_seam() {
    let expected = BTreeSet::from([
        "crates/gents-loop/src/loop_stream.rs".to_string(),
        "crates/gents-inference/src/llm/rig_compat.rs".to_string(),
    ]);
    let found = files_containing(&[".completion_request(", ".completion(", ".stream("]);
    assert_eq!(
        found, expected,
        "provider invocation escaped the owned-loop seam; every completion request \
         must be born in run_loop_stream, which sanitizes loaded history at entry"
    );
}

#[test]
fn owned_loop_entry_points_are_registered() {
    let expected = BTreeSet::from([
        "crates/gents-loop/src/loop_stream/one_shot.rs".to_string(),
        "crates/gents/src/runtime/agent/daemon/inference.rs".to_string(),
        "crates/gents/src/runtime/agent/daemon/title.rs".to_string(),
        "crates/gents/src/runtime/agent/daemon/title/review.rs".to_string(),
        "crates/gents-loop/src/compaction_rolling.rs".to_string(),
        "crates/gents/src/runtime/oneshot.rs".to_string(),
    ]);
    let found = files_containing(&[
        "run_loop_stream(",
        "run_loop_stream::<",
        "run_loop_to_text(",
        "run_loop_to_text::<",
        "run_loop_to_typed(",
        "run_loop_to_typed::<",
    ]);
    assert_eq!(
        found, expected,
        "the set of owned-loop entry points changed; a new completion path must be \
         reviewed against the sanitize-at-entry chokepoint"
    );
}

#[test]
fn empty_skill_catalog_contributes_no_reminders() {
    use gents::turn::prompt::LayeredPromptBuilder;

    let builder =
        LayeredPromptBuilder::for_agent("literal preamble", "fence", &["bash"], false, &[]);

    assert!(
        builder.selected_skill_reminders(&[]).is_empty(),
        "an empty whitelist must not produce skill reminders"
    );

    assert!(
        builder
            .selected_skill_reminders(&["not-installed".to_string()])
            .is_empty(),
        "an unresolvable selection must not produce skill reminders"
    );
    assert!(
        builder
            .selected_skill_reminders(&["  ".to_string()])
            .is_empty(),
        "a blank selection entry must not produce skill reminders"
    );
}
