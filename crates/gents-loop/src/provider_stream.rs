//! Preserve provider bytes while refusing EOF without a protocol terminal event.
//! Some provider adapters synthesize a final response on raw EOF; this guard
//! makes transport truncation an error before that adapter can report success.
use std::sync::Arc;

use futures::StreamExt;
use rig::http_client::{self, StreamingResponse};

use crate::provider_activity::ProviderActivity;

pub const MAX_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub enum ProviderStreamProtocol {
    ChatCompletions,
    Responses,
    Anthropic,
}

impl ProviderStreamProtocol {
    pub fn for_path(path: &str) -> Option<Self> {
        if path.ends_with("/chat/completions") {
            Some(Self::ChatCompletions)
        } else if path.ends_with("/responses") {
            Some(Self::Responses)
        } else if path.ends_with("/messages") {
            Some(Self::Anthropic)
        } else {
            None
        }
    }
}

/// `activity`, when present, is stamped on every received chunk and settled
/// at the protocol terminal event, end of body, or a body error.
pub fn guard_response(
    response: StreamingResponse,
    protocol: Option<ProviderStreamProtocol>,
    activity: Option<Arc<ProviderActivity>>,
) -> StreamingResponse {
    let protocol = protocol.filter(|_| response.status().is_success());
    if protocol.is_none() && activity.is_none() {
        return response;
    }
    let (parts, body) = response.into_parts();
    let stream = futures::stream::unfold(
        (body, protocol.map(TerminalEvents::new), activity, false),
        |(mut body, mut events, activity, ended)| async move {
            if ended {
                return None;
            }
            let settle = |activity: &Option<Arc<ProviderActivity>>| {
                if let Some(activity) = activity {
                    activity.settle();
                }
            };
            match body.next().await {
                Some(Ok(bytes)) => {
                    if let Some(activity) = &activity {
                        activity.touch();
                    }
                    if let Some(events) = events.as_mut() {
                        events.feed(&bytes);
                        if events.terminal {
                            settle(&activity);
                        }
                    }
                    Some((Ok(bytes), (body, events, activity, false)))
                }
                Some(Err(error)) => {
                    settle(&activity);
                    Some((Err(error), (body, events, activity, true)))
                }
                None => {
                    settle(&activity);
                    if events.as_ref().is_some_and(|events| {
                        crate::execution_policy::provider_eof_is_failure(events.terminal)
                    }) {
                        let error = std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "provider stream ended without an explicit protocol terminal event",
                        );
                        Some((
                            Err(http_client::Error::Instance(Box::new(error))),
                            (body, events, activity, true),
                        ))
                    } else {
                        None
                    }
                }
            }
        },
    );
    http_client::Response::from_parts(parts, Box::pin(stream))
}

pub struct TerminalEvents {
    protocol: ProviderStreamProtocol,
    pub line: Vec<u8>,
    pub data: Vec<u8>,
    event_bytes: usize,
    oversized: bool,
    previous_cr: bool,
    pub terminal: bool,
}

impl TerminalEvents {
    pub fn new(protocol: ProviderStreamProtocol) -> Self {
        Self {
            protocol,
            line: Vec::new(),
            data: Vec::new(),
            event_bytes: 0,
            oversized: false,
            previous_cr: false,
            terminal: false,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.terminal {
                return;
            }
            if self.previous_cr {
                self.previous_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte == b'\r' || byte == b'\n' {
                self.finish_line();
                self.previous_cr = byte == b'\r';
            } else {
                self.event_bytes = self.event_bytes.saturating_add(1);
                if self.event_bytes <= MAX_EVENT_BYTES {
                    self.line.push(byte);
                } else {
                    // Discard oversized events, but keep recognizing subsequent
                    // event boundaries and a later small terminal marker.
                    self.oversized = true;
                    // A nonempty sentinel distinguishes this line from a blank.
                    if self.line.is_empty() {
                        self.line.push(0);
                    }
                }
            }
        }
    }

    fn finish_line(&mut self) {
        if self.line.is_empty() {
            if !self.oversized {
                self.finish_event();
            }
            self.data.clear();
            self.event_bytes = 0;
            self.oversized = false;
        } else if !self.oversized {
            if let Some(value) = self.line.strip_prefix(b"data:") {
                let value = value.strip_prefix(b" ").unwrap_or(value);
                if !self.data.is_empty() {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(value);
            }
        }
        self.line.clear();
    }

    fn finish_event(&mut self) {
        // Only delimited events count: an unterminated last JSON fragment at
        // EOF must not authenticate an otherwise truncated response.
        if matches!(self.protocol, ProviderStreamProtocol::ChatCompletions)
            && self.data == b"[DONE]"
        {
            self.terminal = true;
            return;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&self.data) else {
            return;
        };
        self.terminal = match self.protocol {
            ProviderStreamProtocol::ChatCompletions => value
                .get("choices")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|choices| {
                    choices.iter().any(|choice| {
                        choice
                            .get("finish_reason")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|reason| !reason.is_empty())
                    })
                }),
            ProviderStreamProtocol::Responses => {
                value.get("type").and_then(serde_json::Value::as_str) == Some("response.completed")
            }
            ProviderStreamProtocol::Anthropic => {
                value.get("type").and_then(serde_json::Value::as_str) == Some("message_stop")
            }
        };
    }
}
