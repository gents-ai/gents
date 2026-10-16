use super::{payload_fingerprint, CursorAdvance, NovelProjectionEvent, RequestCursor};
use serde_json::json;

#[derive(Clone, Debug)]
pub(crate) struct OutputReceipt {
    start: Option<u64>,
    end: u64,
    hash: u64,
}

fn hash(text: &str) -> u64 {
    payload_fingerprint(&json!(text))
}

fn append_projection(
    previous: Option<&OutputReceipt>,
    start: Option<u64>,
    text: &str,
    truncated: bool,
) -> (String, OutputReceipt) {
    let origin = start.unwrap_or(0);
    let end = origin.saturating_add(text.len() as u64);
    let receipt = OutputReceipt {
        start,
        end,
        hash: hash(text),
    };
    if text.is_empty() {
        return (String::new(), previous.cloned().unwrap_or(receipt));
    }
    let Some(previous) = previous else {
        let notice = if origin > 0 || truncated {
            "[Earlier output was truncated]\n"
        } else {
            ""
        };
        return (format!("{notice}{text}"), receipt);
    };
    // An unchanged unknown-offset tail is not a new append. Do not infer
    // overlap from repeated text when the source has no byte identity.
    if start == previous.start && end == previous.end && receipt.hash == previous.hash {
        return (String::new(), receipt);
    }
    if let (Some(start), Some(old_start)) = (start, previous.start) {
        if start > previous.end {
            return (
                format!(
                    "\n[{} output bytes unavailable]\n{text}",
                    start - previous.end
                ),
                receipt,
            );
        }
        if end >= previous.end {
            let same_stream = start > old_start
                || text
                    .get(
                        (old_start.saturating_sub(start)) as usize..(previous.end - start) as usize,
                    )
                    .is_some_and(|old| hash(old) == previous.hash);
            if same_stream {
                let mut offset = (previous.end - start) as usize;
                while offset < text.len() && !text.is_char_boundary(offset) {
                    offset += 1;
                }
                return (text[offset..].to_owned(), receipt);
            }
        }
    }
    (
        format!("\n[Captured output snapshot; stream continuity unavailable]\n{text}"),
        receipt,
    )
}

impl RequestCursor {
    pub(crate) fn child_output_event(
        &self,
        mut event: NovelProjectionEvent,
    ) -> NovelProjectionEvent {
        let CursorAdvance::BackgroundOutput {
            key, output_start, ..
        } = &event.advance
        else {
            return event;
        };
        let raw = &event.payload["rawOutput"];
        let text = raw["output_for_prompt"].as_str().unwrap_or("");
        let (delta, receipt) = append_projection(
            self.child_outputs.get(key),
            *output_start,
            text,
            raw["truncated"].as_bool().unwrap_or(false),
        );
        let key = key.clone();
        event.method = "x.ai/monitor_event";
        event.payload = json!({"sessionUpdate":"monitor_event", "task_id":key,
            "description":"", "event_text":delta});
        event.advance = CursorAdvance::Many(vec![
            event.advance,
            CursorAdvance::ChildOutput { key, receipt },
        ]);
        event
    }
}
