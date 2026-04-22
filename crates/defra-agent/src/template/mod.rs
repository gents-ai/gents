//! Template rendering for event-driven tasks.
//!
//! Trigger configurations can embed MiniJinja templates whose variables are
//! bound from a [`TemplateScope`] — the firing event, and optionally the
//! originating document and user-supplied arguments. Rendering uses strict
//! undefined semantics (missing variables raise errors) with auto-escape
//! disabled so that rendered output stays literal.

#[cfg(test)]
mod tests;

use minijinja::{AutoEscape, Environment, UndefinedBehavior};

pub(crate) struct TemplateScope {
    pub(crate) event: serde_json::Value,
    pub(crate) doc: Option<serde_json::Value>,
    pub(crate) args: Option<serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("template parse error: {0}")]
    Parse(String),
    #[error("template render error: {0}")]
    Render(String),
    #[error("rendered output exceeds size cap ({0} bytes)")]
    SizeCap(usize),
}

pub(crate) const MAX_TEMPLATE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_RENDERED_BYTES: usize = 1024 * 1024;

/// A variable access path extracted from a template body, e.g.
/// `{{ event.fired_at }}` yields a [`VariableRef`] with `path = ["event",
/// "fired_at"]`. Used by apply-time validation to reject templates whose scope
/// does not match the trigger kind (a Schedule may only read `event.*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableRef {
    pub path: Vec<String>,
}

impl VariableRef {
    pub fn root(&self) -> Option<&str> {
        self.path.first().map(|s| s.as_str())
    }
}

/// Render `template` against `scope` using MiniJinja with strict-undefined
/// semantics and auto-escape disabled.
///
/// The input template is rejected if it exceeds [`MAX_TEMPLATE_BYTES`]; the
/// rendered output is rejected if it exceeds [`MAX_RENDERED_BYTES`]. Both
/// caps keep trigger evaluation bounded regardless of event payload size.
pub(crate) fn render_template(
    template: &str,
    scope: &TemplateScope,
) -> Result<String, TemplateError> {
    if template.len() > MAX_TEMPLATE_BYTES {
        return Err(TemplateError::Parse(format!(
            "template exceeds {} bytes",
            MAX_TEMPLATE_BYTES
        )));
    }

    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_auto_escape_callback(|_| AutoEscape::None);

    let context = {
        let mut ctx = serde_json::Map::new();
        ctx.insert("event".to_string(), scope.event.clone());
        if let Some(doc) = scope.doc.clone() {
            ctx.insert("doc".to_string(), doc);
        }
        if let Some(args) = scope.args.clone() {
            ctx.insert("args".to_string(), args);
        }
        serde_json::Value::Object(ctx)
    };

    let tmpl = env
        .template_from_str(template)
        .map_err(|e| TemplateError::Parse(e.to_string()))?;
    let rendered = tmpl
        .render(&context)
        .map_err(|e| TemplateError::Render(e.to_string()))?;

    if rendered.len() > MAX_RENDERED_BYTES {
        return Err(TemplateError::SizeCap(rendered.len()));
    }
    Ok(rendered)
}

/// Parse `template` and return every variable access whose root identifier is
/// `event`, `doc`, or `args`. Used to validate that a template only references
/// scopes the trigger kind actually supplies (e.g. a Schedule provides `event`
/// but not `doc` or `args`).
///
/// # Approach and limitations
///
/// MiniJinja exposes its parser only under the `unstable_machinery` cargo
/// feature, whose API the upstream crate does not guarantee. Rather than pin
/// an unstable surface, this function uses a narrow textual scan:
///
/// 1. Walk the template, tracking which Jinja block we are inside
///    (`{{ ... }}`, `{% ... %}`, or `{# ... #}`).
/// 2. Skip comment blocks entirely.
/// 3. Inside expression and statement blocks, collect identifier chains that
///    begin with one of `event`, `doc`, or `args` at top level, and extend
///    through `.ident` and `["literal"]` accesses.
///
/// This is deliberately conservative: it does NOT follow Jinja loop
/// variables, macro parameters, or filter arguments that rebind names (for
/// example `{% for item in event.items %}{{ item.x }}{% endfor %}` will only
/// see `event.items`, not `item.x`). PR 1's validation only needs to catch
/// the straightforward `{{ doc.foo }}` / `{{ args.bar }}` patterns that would
/// reference a scope the Schedule trigger does not supply, so this suffices.
/// The function never panics; on a malformed block it simply stops scanning
/// at the malformed point and returns what it found so far.
pub fn parse_template_for_validation(template: &str) -> Result<Vec<VariableRef>, TemplateError> {
    if template.len() > MAX_TEMPLATE_BYTES {
        return Err(TemplateError::Parse(format!(
            "template exceeds {} bytes",
            MAX_TEMPLATE_BYTES
        )));
    }

    let bytes = template.as_bytes();
    let mut refs: Vec<VariableRef> = Vec::new();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        match bytes[i + 1] {
            b'#' => {
                // Comment: skip to matching `#}`.
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'#' && bytes[i + 1] == b'}') {
                    i += 1;
                }
                if i + 1 < bytes.len() {
                    i += 2;
                }
            }
            b'{' => {
                // Expression block: collect until `}}`.
                let start = i + 2;
                let end = find_close(bytes, start, b'}');
                if let Some(end_idx) = end {
                    let body = &template[start..end_idx];
                    collect_refs_in_body(body, &mut refs);
                    i = end_idx + 2;
                } else {
                    break;
                }
            }
            b'%' => {
                // Statement block: collect until `%}`. Keywords inside are
                // filtered by the identifier scan (they don't start with one
                // of the tracked roots).
                let start = i + 2;
                let end = find_close(bytes, start, b'%');
                if let Some(end_idx) = end {
                    let body = &template[start..end_idx];
                    collect_refs_in_body(body, &mut refs);
                    i = end_idx + 2;
                } else {
                    break;
                }
            }
            _ => {
                i += 1;
            }
        }
    }

    Ok(refs)
}

/// Locate the closing sequence `<close>}` starting from `from`, returning the
/// index of `<close>` (so the `}` is at `idx + 1`). Returns `None` if no close
/// is found (malformed block).
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

/// Scan the body of a `{{ ... }}` or `{% ... %}` block for identifier chains
/// that start with `event`, `doc`, or `args` at top level. A chain starts at
/// a position where the preceding non-whitespace character is NOT `.` (so we
/// don't treat the `event` in `foo.event` as a root) and extends through
/// `.name` and `["literal"]` access steps.
fn collect_refs_in_body(body: &str, out: &mut Vec<VariableRef>) {
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if is_ident_start(c) {
            // Skip identifiers that are a continuation of a `.name` access —
            // those are already consumed as part of the chain they belong to.
            let prev = prev_non_ws_char(body, i);
            let ident_start = i;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            let ident = &body[ident_start..i];
            if prev != Some('.') && is_tracked_root(ident) {
                let mut path: Vec<String> = vec![ident.to_string()];
                // Extend through `.name` / `["key"]` / `['key']`.
                loop {
                    let save = i;
                    // Skip whitespace.
                    while i < bytes.len() && is_ws(bytes[i]) {
                        i += 1;
                    }
                    if i < bytes.len() && bytes[i] == b'.' {
                        i += 1;
                        while i < bytes.len() && is_ws(bytes[i]) {
                            i += 1;
                        }
                        let name_start = i;
                        if i < bytes.len() && is_ident_start(bytes[i]) {
                            while i < bytes.len() && is_ident_continue(bytes[i]) {
                                i += 1;
                            }
                            path.push(body[name_start..i].to_string());
                            continue;
                        } else {
                            // Malformed `.` with no identifier: stop chain.
                            i = save;
                            break;
                        }
                    } else if i < bytes.len() && bytes[i] == b'[' {
                        i += 1;
                        while i < bytes.len() && is_ws(bytes[i]) {
                            i += 1;
                        }
                        if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                            let quote = bytes[i];
                            i += 1;
                            let key_start = i;
                            while i < bytes.len() && bytes[i] != quote {
                                i += 1;
                            }
                            if i < bytes.len() {
                                let key = body[key_start..i].to_string();
                                i += 1; // consume closing quote
                                while i < bytes.len() && is_ws(bytes[i]) {
                                    i += 1;
                                }
                                if i < bytes.len() && bytes[i] == b']' {
                                    i += 1;
                                    path.push(key);
                                    continue;
                                }
                            }
                            // Malformed bracket expression: stop chain.
                            i = save;
                            break;
                        } else {
                            // Numeric / computed index: don't extend with a
                            // stable key, stop the chain at what we have.
                            i = save;
                            break;
                        }
                    } else {
                        i = save;
                        break;
                    }
                }
                out.push(VariableRef { path });
            }
        } else if c == b'"' || c == b'\'' {
            // Skip over string literals so identifiers inside them don't
            // register as variable references.
            let quote = c;
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i < bytes.len() {
                i += 1;
            }
        } else {
            // Advance by one char (may be multi-byte in UTF-8) to keep
            // byte index aligned on char boundaries.
            let step = utf8_char_len(c);
            i += step;
        }
    }
}

fn utf8_char_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first < 0xC0 {
        // Continuation byte — shouldn't be seen at a char boundary, but keep
        // forward progress.
        1
    } else if first < 0xE0 {
        2
    } else if first < 0xF0 {
        3
    } else {
        4
    }
}

fn is_tracked_root(ident: &str) -> bool {
    matches!(ident, "event" | "doc" | "args")
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_continue(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

fn prev_non_ws_char(body: &str, idx: usize) -> Option<char> {
    body[..idx].chars().rev().find(|c| !c.is_whitespace())
}
