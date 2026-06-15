use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use minijinja::{AutoEscape, Environment, UndefinedBehavior};
use serde::Serialize;

use crate::template::{MAX_RENDERED_BYTES, MAX_TEMPLATE_BYTES};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptContextTemplates {
    system_context_template: Option<String>,
    request_context_template: Option<String>,
}

#[derive(Debug, Serialize)]
struct AgentScope<'a> {
    did: &'a str,
}

#[derive(Debug, Serialize)]
struct BehaviorScope<'a> {
    id: &'a str,
}

#[derive(Debug, Serialize)]
struct ModelScope<'a> {
    name: &'a str,
}

#[derive(Debug, Serialize)]
struct RunScope<'a> {
    agent: AgentScope<'a>,
    behavior: BehaviorScope<'a>,
    model: ModelScope<'a>,
}

#[derive(Debug, Serialize)]
struct RequestScope<'a> {
    id: &'a str,
    session_id: &'a str,
    agent_did: &'a str,
    behavior_id: &'a str,
    content: &'a str,
    metadata: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct TimeScope {
    utc_now: String,
}

#[derive(Debug, Serialize)]
struct RequestRenderScope<'a> {
    agent: AgentScope<'a>,
    behavior: BehaviorScope<'a>,
    model: ModelScope<'a>,
    request: RequestScope<'a>,
    time: TimeScope,
}

impl PromptContextTemplates {
    pub fn new(
        system_context_template: Option<&str>,
        request_context_template: Option<&str>,
    ) -> Result<Self> {
        let system_context_template = normalize_template(system_context_template);
        let request_context_template = normalize_template(request_context_template);

        if let Some(template) = system_context_template.as_deref() {
            validate_template(template).context("system_context_template is invalid")?;
            reject_system_dynamic_roots(template)?;
        }
        if let Some(template) = request_context_template.as_deref() {
            validate_template(template).context("request_context_template is invalid")?;
        }

        Ok(Self {
            system_context_template,
            request_context_template,
        })
    }

    pub(crate) fn render_system_context(
        &self,
        behavior: &crate::config::AgentBehavior,
    ) -> Result<Option<String>> {
        self.render_system_context_values(
            behavior.agent_did(),
            behavior.behavior_id.as_str(),
            behavior.model_name.as_str(),
        )
    }

    pub(crate) fn render_system_context_values(
        &self,
        agent_did: &str,
        behavior_id: &str,
        model_name: &str,
    ) -> Result<Option<String>> {
        let Some(template) = self.system_context_template.as_deref() else {
            return Ok(None);
        };
        let scope = RunScope {
            agent: AgentScope { did: agent_did },
            behavior: BehaviorScope { id: behavior_id },
            model: ModelScope { name: model_name },
        };
        render_prompt_context_template(template, &scope)
            .context("render system_context_template")
            .map(non_empty_rendered)
    }

    pub(crate) fn render_request_context_values(
        &self,
        agent_did: &str,
        behavior_id: &str,
        model_name: &str,
        request: &crate::watcher::AgentRequest,
        now: DateTime<Utc>,
    ) -> Result<Option<String>> {
        let Some(template) = self.request_context_template.as_deref() else {
            return Ok(None);
        };
        let metadata = request
            .metadata
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or(serde_json::Value::Null);
        let request_behavior_id = request.behavior_id.as_deref().unwrap_or(behavior_id);
        let scope = RequestRenderScope {
            agent: AgentScope { did: agent_did },
            behavior: BehaviorScope { id: behavior_id },
            model: ModelScope { name: model_name },
            request: RequestScope {
                id: request.request_id.as_str(),
                session_id: request.session_id.as_str(),
                agent_did: request.agent_did.as_str(),
                behavior_id: request_behavior_id,
                content: request.content.as_str(),
                metadata,
            },
            time: TimeScope {
                utc_now: now.to_rfc3339(),
            },
        };
        render_prompt_context_template(template, &scope)
            .context("render request_context_template")
            .map(non_empty_rendered)
    }
}

fn normalize_template(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn non_empty_rendered(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn validate_template(template: &str) -> Result<()> {
    if template.len() > MAX_TEMPLATE_BYTES {
        anyhow::bail!("template exceeds {MAX_TEMPLATE_BYTES} bytes");
    }
    environment()
        .template_from_str(template)
        .map(|_| ())
        .map_err(|error| anyhow::anyhow!("template parse error: {error}"))
}

fn render_prompt_context_template<T: Serialize>(template: &str, context: &T) -> Result<String> {
    if template.len() > MAX_TEMPLATE_BYTES {
        anyhow::bail!("template exceeds {MAX_TEMPLATE_BYTES} bytes");
    }
    let rendered = environment()
        .template_from_str(template)
        .map_err(|error| anyhow::anyhow!("template parse error: {error}"))?
        .render(context)
        .map_err(|error| anyhow::anyhow!("template render error: {error}"))?;
    if rendered.len() > MAX_RENDERED_BYTES {
        anyhow::bail!("rendered output exceeds size cap ({MAX_RENDERED_BYTES} bytes)");
    }
    Ok(rendered)
}

fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_auto_escape_callback(|_| AutoEscape::None);
    env
}

fn reject_system_dynamic_roots(template: &str) -> Result<()> {
    for root in ["request", "time"] {
        if references_root(template, root) {
            anyhow::bail!(
                "system_context_template may not reference per-request `{root}.*` values"
            );
        }
    }
    Ok(())
}

fn references_root(template: &str, root: &str) -> bool {
    let bytes = template.as_bytes();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        match bytes[i + 1] {
            b'#' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'#' && bytes[i + 1] == b'}') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            b'{' => {
                let start = i + 2;
                let Some(end) = find_close(bytes, start, b'}') else {
                    break;
                };
                if body_references_root(&template[start..end], root) {
                    return true;
                }
                i = end + 2;
            }
            b'%' => {
                let start = i + 2;
                let Some(end) = find_close(bytes, start, b'%') else {
                    break;
                };
                if body_references_root(&template[start..end], root) {
                    return true;
                }
                i = end + 2;
            }
            _ => i += 1,
        }
    }
    false
}

fn find_close(bytes: &[u8], from: usize, close: u8) -> Option<usize> {
    let mut i = from;
    while i + 1 < bytes.len() {
        if bytes[i] == close && bytes[i + 1] == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn body_references_root(body: &str, root: &str) -> bool {
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\'' || bytes[i] == b'"' {
            i = skip_quoted_string(bytes, i);
        } else if is_ident_start(bytes[i]) {
            let prev = prev_non_ws_char(body, i);
            let start = i;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            if prev != Some('.') && &body[start..i] == root {
                return true;
            }
        } else {
            i += 1;
        }
    }
    false
}

fn skip_quoted_string(bytes: &[u8], quote_index: usize) -> usize {
    let quote = bytes[quote_index];
    let mut i = quote_index + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i = (i + 2).min(bytes.len());
        } else if bytes[i] == quote {
            return i + 1;
        } else {
            i += 1;
        }
    }
    bytes.len()
}

fn prev_non_ws_char(s: &str, idx: usize) -> Option<char> {
    s[..idx].chars().rev().find(|ch| !ch.is_whitespace())
}

fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

fn is_ident_continue(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_request_values_in_system_context_template() {
        let error = PromptContextTemplates::new(Some("request {{ request.id }}"), None)
            .expect_err("dynamic request values must be rejected");

        assert!(error
            .to_string()
            .contains("system_context_template may not reference per-request"));
    }

    #[test]
    fn ignores_dynamic_root_inside_comment() {
        PromptContextTemplates::new(Some("static {# request.id #} ok"), None).unwrap();
    }

    #[test]
    fn ignores_dynamic_root_inside_string_literal() {
        PromptContextTemplates::new(Some(r#"static {{ "request.id" }} ok"#), None).unwrap();
    }
}
