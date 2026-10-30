use anyhow::{Context as _, Result};
use serde_json::{json, Value};

pub(in crate::commands::grok_shim) const SCREEN_MODES: [&str; 3] =
    ["fullscreen", "inline", "minimal"];

#[derive(Debug, Clone, PartialEq)]
pub(in crate::commands::grok_shim) struct PromptBlock {
    pub kind: String,
    pub text: String,
    pub meta: Option<Value>,
}

impl PromptBlock {
    #[cfg(test)]
    pub(in crate::commands::grok_shim) fn bash_command(&self) -> Option<String> {
        let meta = self.meta.as_ref()?;
        let command = meta
            .get("bash")
            .and_then(|bash| bash.get("command"))
            .and_then(Value::as_str)?;
        (!command.is_empty()).then(|| command.to_string())
    }

    #[cfg(test)]
    pub(in crate::commands::grok_shim) fn has_skill_token_ranges(&self) -> bool {
        self.meta
            .as_ref()
            .and_then(|meta| meta.get("skillTokenRanges"))
            .and_then(Value::as_array)
            .is_some_and(|ranges| !ranges.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::commands::grok_shim) struct PromptRequest {
    pub session_id: String,
    pub prompt: Vec<PromptBlock>,
    pub prompt_id: Option<String>,
    pub screen_mode: Option<String>,
    pub send_now: bool,
    pub id: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::commands::grok_shim) struct CancelNotification {
    pub session_id: String,
    pub cancel_trigger: Option<String>,
    pub rewind_if_no_output: bool,
    pub rewind_if_pristine: bool,
    pub prompt_id: Option<String>,
}

impl CancelNotification {
    #[cfg(test)]
    pub(in crate::commands::grok_shim) fn meta(&self) -> Value {
        let mut meta = json!({
            "rewindIfNoOutput": self.rewind_if_no_output,
            "rewindIfPristine": self.rewind_if_pristine,
        });
        if let Some(trigger) = self.cancel_trigger.as_deref() {
            meta["cancelTrigger"] = json!(trigger);
        }
        if let Some(prompt_id) = self.prompt_id.as_deref() {
            meta["promptId"] = json!(prompt_id);
        }
        meta
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::commands::grok_shim) enum StopReason {
    EndTurn,
    Cancelled,
    Refusal,
    Error,
}

impl StopReason {
    pub(in crate::commands::grok_shim) fn wire_name(self) -> &'static str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::Cancelled => "cancelled",
            StopReason::Refusal => "refusal",
            StopReason::Error => "error",
        }
    }
}

#[derive(Debug)]
pub(super) struct TerminalOutcome {
    pub(super) stop_reason: StopReason,
    pub(super) error_message: Option<String>,
}

impl From<StopReason> for TerminalOutcome {
    fn from(stop_reason: StopReason) -> Self {
        Self {
            stop_reason,
            error_message: None,
        }
    }
}

pub(super) fn prompt_result(outcome: TerminalOutcome) -> Result<Value> {
    if outcome.stop_reason == StopReason::Error {
        let message = outcome
            .error_message
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| "gents request failed".to_string());
        anyhow::bail!("{message}");
    }
    Ok(json!({"stopReason": outcome.stop_reason.wire_name()}))
}

pub(super) fn stop_reason_from_wire(name: &str) -> StopReason {
    match name {
        "end_turn" => StopReason::EndTurn,
        "refusal" => StopReason::Refusal,
        "error" => StopReason::Error,
        _ => StopReason::Cancelled,
    }
}

pub(in crate::commands::grok_shim) fn stop_reason_from_rows(
    lifecycle_state: &str,
    interrupt_requested_at: Option<&str>,
) -> Option<StopReason> {
    let interrupted_at_nonempty = interrupt_requested_at
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    match lifecycle_state {
        "interrupted" => Some(StopReason::Cancelled),
        "completed" => Some(StopReason::EndTurn),
        "failed" => Some(StopReason::Error),
        "superseded" | "dead" => {
            if interrupted_at_nonempty {
                Some(StopReason::Cancelled)
            } else {
                Some(StopReason::Error)
            }
        }
        _ => None,
    }
}

pub(in crate::commands::grok_shim) fn prompt_text(request: &PromptRequest) -> String {
    request
        .prompt
        .iter()
        .map(|block| {
            if block.kind == "text" {
                block.text.clone()
            } else {
                serde_json::to_string(&json!({
                    "type": block.kind,
                    "text": block.text,
                    "meta": block.meta,
                }))
                .unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(in crate::commands::grok_shim) fn parse_prompt_request(
    params: &Value,
    id: Option<Value>,
) -> Result<PromptRequest> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("session/prompt requires sessionId")?
        .to_string();
    let prompt_rows = params
        .get("prompt")
        .and_then(Value::as_array)
        .context("session/prompt requires a prompt array")?;
    if prompt_rows.is_empty() {
        anyhow::bail!("session/prompt requires at least one prompt block");
    }
    let mut prompt = Vec::with_capacity(prompt_rows.len());
    for row in prompt_rows {
        let kind = row
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("text")
            .to_string();
        let text = row
            .get("text")
            .and_then(Value::as_str)
            .context("session/prompt prompt block missing text")?
            .to_string();
        let meta = row.get("meta").cloned().filter(|meta| !meta.is_null());
        prompt.push(PromptBlock { kind, text, meta });
    }
    let meta = params.get("_meta");
    let prompt_id = meta
        .and_then(|meta| meta.get("promptId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let screen_mode = meta
        .and_then(|meta| meta.get("screenMode"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    if let Some(screen_mode) = screen_mode.as_deref() {
        if !SCREEN_MODES.contains(&screen_mode) {
            anyhow::bail!("unknown screenMode {screen_mode:?}");
        }
    }
    let send_now = meta
        .and_then(|meta| meta.get("sendNow"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(PromptRequest {
        session_id,
        prompt,
        prompt_id,
        screen_mode,
        send_now,
        id,
    })
}

pub(in crate::commands::grok_shim) fn parse_cancel_notification(
    params: &Value,
) -> Result<CancelNotification> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("session/cancel requires sessionId")?
        .to_string();
    let meta = params.get("_meta");
    let cancel_trigger = meta
        .and_then(|meta| meta.get("cancelTrigger"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let rewind_if_no_output = meta
        .and_then(|meta| meta.get("rewindIfNoOutput"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let rewind_if_pristine = meta
        .and_then(|meta| meta.get("rewindIfPristine"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let prompt_id = meta
        .and_then(|meta| meta.get("promptId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    Ok(CancelNotification {
        session_id,
        cancel_trigger,
        rewind_if_no_output,
        rewind_if_pristine,
        prompt_id,
    })
}
