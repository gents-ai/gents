use super::presentation::{append_json_channel, append_plain_channel, channel_plan, push_literal};
use super::writer::drain_utf8;
use super::*;
use gents_protocol::output::PresentationPart;

#[tokio::test]
async fn deferred_host_batches_keep_distinct_producers_and_replace_only_their_own_type() {
    let registry = LiveToolOutputRegistry::default();
    let writer = registry.writer_for("physical-tool").await;
    writer
        .record_deferred_writes(DeferredToolWrites::new(String::from("captured snapshot")))
        .await
        .unwrap();
    writer
        .record_deferred_writes(DeferredToolWrites::new(1u64))
        .await
        .unwrap();
    writer
        .record_deferred_writes(DeferredToolWrites::new(2u64))
        .await
        .unwrap();
    let pending = registry.deferred_writes("physical-tool").await.unwrap();
    assert_eq!(
        pending.downcast_ref::<String>().unwrap(),
        "captured snapshot"
    );
    assert_eq!(pending.downcast_ref::<u64>(), Some(&2));
    assert!(pending.downcast_ref::<bool>().is_none());
}

#[tokio::test]
async fn concurrent_deferred_host_batches_stay_bound_to_their_physical_tool() {
    let registry = LiveToolOutputRegistry::default();
    let first = registry.writer_for("physical-tool").await;
    let second = registry.writer_for("physical-tool").await;
    let unrelated = registry.writer_for("other-tool").await;
    let (left, right, other) = tokio::join!(
        first.record_deferred_writes(DeferredToolWrites::new(7u64)),
        second.record_deferred_writes(DeferredToolWrites::new(String::from("snapshot"))),
        unrelated.record_deferred_writes(DeferredToolWrites::new(false)),
    );
    left.unwrap();
    right.unwrap();
    other.unwrap();
    let pending = registry.deferred_writes("physical-tool").await.unwrap();
    assert_eq!(pending.downcast_ref::<u64>(), Some(&7));
    assert_eq!(pending.downcast_ref::<String>().unwrap(), "snapshot");
    assert!(pending.downcast_ref::<bool>().is_none());
    let pending = registry.deferred_writes("other-tool").await.unwrap();
    assert_eq!(pending.downcast_ref::<bool>(), Some(&false));
    assert!(pending.downcast_ref::<u64>().is_none());
    registry.remove("physical-tool").await;
    assert!(first
        .record_deferred_writes(DeferredToolWrites::new(9u64))
        .await
        .is_err());
    assert!(registry.deferred_writes("physical-tool").await.is_none());
}

fn render(source: &str, parts: &[PresentationPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            PresentationPart::Literal { text } => text.clone(),
            PresentationPart::OutputRange {
                start_byte,
                end_byte,
            } => source[*start_byte as usize..*end_byte as usize].to_owned(),
        })
        .collect()
}

fn whole(raw: &str) -> CapturedChannel<'_> {
    CapturedChannel {
        raw,
        rendered: raw,
        shown: 0..raw.len(),
    }
}

#[derive(Debug, Default)]
struct MemoryAppender(std::sync::Mutex<String>);

impl CanonicalOutputAppender for MemoryAppender {
    fn append<'a>(
        &'a self,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<std::ops::Range<u64>, LiveOutputError>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut source = self.0.lock().unwrap();
            let start = source.len() as u64;
            source.push_str(text);
            Ok(start..source.len() as u64)
        })
    }
}

async fn prepared(
    layout: CommandPresentationLayout<'_>,
    expected: &str,
    stdout: CapturedChannel<'_>,
    stderr: CapturedChannel<'_>,
) -> String {
    let registry = LiveToolOutputRegistry::default();
    let appender = Arc::new(MemoryAppender::default());
    let writer = registry
        .canonical_writer_for("tool".into(), appender.clone())
        .await;
    let (out_a, out_b) = stdout.raw.split_at(stdout.raw.len() / 2);
    writer
        .append(LiveOutputStream::Stdout, out_a.as_bytes())
        .await;
    writer
        .append(LiveOutputStream::Stderr, stderr.raw.as_bytes())
        .await;
    writer
        .append(LiveOutputStream::Stdout, out_b.as_bytes())
        .await;
    writer
        .prepare_command_presentation(expected, layout, stdout, stderr)
        .await
        .unwrap();
    let PayloadPresentation::Composed { parts } = registry
        .take_prepared_presentation("tool", expected)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("command presentation is composed")
    };
    let source = appender.0.lock().unwrap().clone();
    render(&source, &parts)
}

#[tokio::test]
async fn command_layouts_reconstruct_their_exact_text() {
    let stdout = "line \"one\"\nline two\n";
    let stderr = "warn\tthree";
    let metadata = r#"{"ok":true}"#;

    let object = format!(
        r#"{{"stdout":{},"stderr":{},"ok":true}}"#,
        serde_json::to_string(stdout).unwrap(),
        serde_json::to_string(stderr).unwrap(),
    );
    let rendered = prepared(
        CommandPresentationLayout::JsonObject {
            metadata_json: metadata,
        },
        &object,
        whole(stdout),
        whole(stderr),
    )
    .await;
    assert_eq!(rendered, object);

    let labeled = format!("gents_exec: {metadata}\nstdout:\n{stdout}\nstderr:\n(empty)");
    let rendered = prepared(
        CommandPresentationLayout::Labeled {
            head: &format!("gents_exec: {metadata}\n"),
            json_string: false,
        },
        &labeled,
        whole(stdout),
        CapturedChannel {
            raw: "",
            rendered: "",
            shown: 0..0,
        },
    )
    .await;
    assert_eq!(rendered, labeled);

    let text = format!("exit_code: 0\nstdout:\n{stdout}\nstderr:\n{stderr}");
    let encoded = serde_json::to_string(&text).unwrap();
    let rendered = prepared(
        CommandPresentationLayout::Labeled {
            head: "exit_code: 0\n",
            json_string: true,
        },
        &encoded,
        whole(stdout),
        whole(stderr),
    )
    .await;
    assert_eq!(rendered, encoded);
}

#[tokio::test]
async fn suffix_channels_select_only_the_retained_tail() {
    let stdout = "head that scrolled away\ntail é kept";
    let start = stdout.find("tail").unwrap();
    let shown = format!("[notice]\n\n{}", &stdout[start..]);
    let text = format!("timed out\nstdout:\n{shown}\nstderr:\n(empty)");
    let rendered = prepared(
        CommandPresentationLayout::Labeled {
            head: "timed out\n",
            json_string: false,
        },
        &text,
        CapturedChannel {
            raw: stdout,
            rendered: &shown,
            shown: start..stdout.len(),
        },
        whole(""),
    )
    .await;
    assert_eq!(rendered, text);

    let mismatched = CapturedChannel {
        raw: stdout,
        rendered: "unrelated",
        shown: start..stdout.len(),
    };
    assert!(channel_plan(&mismatched, true).is_err());
    let interior = CapturedChannel {
        raw: stdout,
        rendered: "that",
        shown: 5..9,
    };
    assert!(channel_plan(&interior, true).is_err());
}

#[test]
fn composed_command_presentation_keeps_interleaved_unicode_raw_bytes_once() {
    let source = "é!x";
    let stdout = vec![
        OutputReceipt {
            channel_start: 0,
            channel_end: 2,
            source_start: 0,
            source_end: 2,
        },
        OutputReceipt {
            channel_start: 2,
            channel_end: 3,
            source_start: 3,
            source_end: 4,
        },
    ];
    let stderr = vec![OutputReceipt {
        channel_start: 0,
        channel_end: 1,
        source_start: 2,
        source_end: 3,
    }];
    let mut parts = Vec::new();
    append_plain_channel(
        &mut parts,
        &channel_plan(&whole("éx"), true).unwrap(),
        &stdout,
    )
    .unwrap();
    push_literal(&mut parts, "|".into());
    append_plain_channel(
        &mut parts,
        &channel_plan(&whole("!"), true).unwrap(),
        &stderr,
    )
    .unwrap();
    assert_eq!(render(source, &parts), "éx|!");

    let mut json = Vec::new();
    append_json_channel(
        &mut json,
        "éx",
        &channel_plan(&whole("éx"), false).unwrap(),
        &stdout,
    )
    .unwrap();
    assert_eq!(render(source, &json), "éx");
}

#[test]
fn drained_reads_decode_as_the_whole_stream_does() {
    let stream: &[u8] = b"a\xff\xc3\xa9\xc3\xe2\x82\xacz\xe2\x82";
    for split in 0..=stream.len() {
        for second in split..=stream.len() {
            let mut pending = Vec::new();
            let mut text = String::new();
            for read in [&stream[..split], &stream[split..second], &stream[second..]] {
                pending.extend_from_slice(read);
                text.push_str(&drain_utf8(&mut pending, false));
            }
            text.push_str(&drain_utf8(&mut pending, true));
            assert!(pending.is_empty());
            assert_eq!(text, String::from_utf8_lossy(stream), "{split}/{second}");
        }
    }
}

#[tokio::test]
async fn invalid_and_split_bytes_cover_the_lossy_channel() {
    let registry = LiveToolOutputRegistry::default();
    let appender = Arc::new(MemoryAppender::default());
    let writer = registry
        .canonical_writer_for("tool".into(), appender.clone())
        .await;
    for read in [&b"\xff\xc3"[..], b"\xa9\xe2"] {
        writer.append(LiveOutputStream::Stdout, read).await;
    }
    let raw = String::from_utf8_lossy(b"\xff\xc3\xa9\xe2").into_owned();
    assert_eq!(raw, "\u{FFFD}é\u{FFFD}");
    let text = format!("x\nstdout:\n{raw}\nstderr:\n");
    writer
        .prepare_command_presentation(
            &text,
            CommandPresentationLayout::Labeled {
                head: "x\n",
                json_string: false,
            },
            whole(&raw),
            whole(""),
        )
        .await
        .unwrap();
    assert_eq!(*appender.0.lock().unwrap(), raw);
}

#[tokio::test]
async fn writer_registration_is_visible_before_first_output_byte() {
    let registry = LiveToolOutputRegistry::default();
    let _writer = registry.writer_for("tool-1").await;

    let snapshot = registry
        .snapshot("tool-1")
        .await
        .expect("writer_for must eagerly register an empty live buffer");
    assert_eq!(snapshot.combined.total_bytes_seen, 0);
    assert!(snapshot.combined.bytes.is_empty());
}
