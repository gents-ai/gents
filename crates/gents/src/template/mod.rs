//! Template rendering for event-driven tasks.

#[cfg(test)]
mod tests;

use minijinja::{machinery, AutoEscape, Environment, ErrorKind, UndefinedBehavior};

pub mod catalog;

pub struct TemplateScope {
    pub event: serde_json::Value,
    pub doc: Option<serde_json::Value>,
    pub args: Option<serde_json::Value>,
    pub group: Option<serde_json::Value>,
    pub node: serde_json::Value,
    pub ctx: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("template parse error: {0}")]
    Parse(String),
    #[error("template render error: {0}")]
    Render(String),
    #[error("rendered output exceeds size cap ({0} bytes)")]
    SizeCap(usize),
    #[error(
        "template uses unknown {kind} `{name}`; the runtime template engine does not provide it"
    )]
    UnknownName { kind: &'static str, name: String },
}

pub(crate) const MAX_TEMPLATE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_RENDERED_BYTES: usize = 1024 * 1024;

/// Authoring and execution use the same grammar, filters, tests and functions.
/// Only the undefined-value policy differs: execution is strict, while
/// authoring cannot be, because invocation values do not exist yet.
fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_auto_escape_callback(|_| AutoEscape::None);
    env
}

/// Undefined lookups stay legal so the admission probe can reach the whole
/// template: a document or argument field it cannot know about is not an
/// authoring error.
fn authoring_environment() -> Environment<'static> {
    let mut env = environment();
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    env
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableRef {
    pub path: Vec<String>,
}

impl VariableRef {
    pub fn root(&self) -> Option<&str> {
        self.path.first().map(|s| s.as_str())
    }
}

pub fn render_template(template: &str, scope: &TemplateScope) -> Result<String, TemplateError> {
    if template.len() > MAX_TEMPLATE_BYTES {
        return Err(TemplateError::Parse(format!(
            "template exceeds {} bytes",
            MAX_TEMPLATE_BYTES
        )));
    }

    let env = environment();

    let context = template_context(scope);

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

fn template_context(scope: &TemplateScope) -> serde_json::Value {
    let mut ctx = serde_json::Map::new();
    ctx.insert("event".to_string(), scope.event.clone());
    if let Some(doc) = scope.doc.clone() {
        ctx.insert("doc".to_string(), doc);
    }
    if let Some(args) = scope.args.clone() {
        ctx.insert("args".to_string(), args);
    }
    if let Some(group) = scope.group.clone() {
        ctx.insert("group".to_string(), group);
    }
    ctx.insert("node".to_string(), scope.node.clone());
    ctx.insert("ctx".to_string(), scope.ctx.clone());
    serde_json::Value::Object(ctx)
}

/// Every root and runtime-owned field a fire owner can supply, so a name the
/// runtime provides is never unknown to authoring. Documents and task arguments
/// carry caller-defined fields that configuration cannot enumerate; those stay
/// undefined here and undefined is never an authoring failure.
fn authoring_scope() -> TemplateScope {
    let (node, ctx) = task_node_ctx("did:key:zAUTHORING", "authoring", "1970-01-01T00:00:00Z");
    TemplateScope {
        event: serde_json::json!({
            "fired_at": "1970-01-01T00:00:00Z",
            "trigger_id": "authoring",
            "trigger_kind": "manual",
            "source_collection": "Authoring",
            "source_doc_id": "authoring",
            "correlation": "authoring",
            "parent_request_id": "authoring",
            "child_request_id": "authoring",
        }),
        doc: Some(serde_json::json!({})),
        args: Some(serde_json::json!({})),
        group: Some(serde_json::json!({
            "correlation_value": "authoring",
            "count": 1,
            "docs": [{}],
            "complete": true,
        })),
        node,
        ctx,
    }
}

/// Filters, tests and functions resolve while rendering, so compiling a
/// template admits names the engine can never provide and every fire of the
/// task then fails. Configuration admission rejects those names here.
///
/// A rejection reports only that the environment does not provide a name, never
/// that a render failed: invocation values do not exist at authoring time, so
/// value-dependent failures are not admission facts.
pub fn check_template_vocabulary(template: &str) -> Result<(), TemplateError> {
    if template.len() > MAX_TEMPLATE_BYTES {
        return Err(TemplateError::Parse(format!(
            "template exceeds {} bytes",
            MAX_TEMPLATE_BYTES
        )));
    }

    let env = authoring_environment();
    let compiled = env
        .template_from_str(template)
        .map_err(|error| TemplateError::Parse(error.to_string()))?;

    // Filter and test names resolve against the environment alone, never
    // against the invocation scope, so the compiled program names every one a
    // fire can reach - including branches the probe render below never enters.
    let instructions = &machinery::get_compiled_template(&compiled).instructions;
    let mut index = 0u32;
    while let Some(instruction) = instructions.get(index) {
        match instruction {
            machinery::Instruction::ApplyFilter(name, ..) => {
                let name = engine_name(name);
                if !engine_resolves(&env, &format!("{{{{ 0 | {name} }}}}")) {
                    return Err(TemplateError::UnknownName {
                        kind: "filter",
                        name,
                    });
                }
            }
            machinery::Instruction::PerformTest(name, ..) => {
                let name = engine_name(name);
                if !engine_resolves(&env, &format!("{{{{ 0 is {name} }}}}")) {
                    return Err(TemplateError::UnknownName { kind: "test", name });
                }
            }
            _ => {}
        }
        index += 1;
    }

    // A called name may also come from the scope or a loop binding, so only the
    // engine's own lookup can judge it, over the paths this scope executes.
    match env.render_str(template, template_context(&authoring_scope())) {
        Ok(_) => Ok(()),
        Err(error) => match error.kind() {
            ErrorKind::UnknownFunction => Err(unknown_name("function", &error)),
            ErrorKind::UnknownFilter => Err(unknown_name("filter", &error)),
            ErrorKind::UnknownTest => Err(unknown_name("test", &error)),
            _ => Ok(()),
        },
    }
}

/// The virtual machine ignores whitespace inside a filter or test name.
fn engine_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_ascii_whitespace()).collect()
}

/// Only the engine's own refusal of the name counts: any other probe failure is
/// about the probe's operand, so the name exists.
fn engine_resolves(env: &Environment<'static>, probe: &str) -> bool {
    !matches!(
        env.render_str(probe, ()).err().map(|error| error.kind()),
        Some(ErrorKind::UnknownFilter | ErrorKind::UnknownTest)
    )
}

/// MiniJinja carries the unresolved name only in the error detail, as
/// "<kind> <name> is unknown" or "<name> is unknown".
fn unknown_name(kind: &'static str, error: &minijinja::Error) -> TemplateError {
    let detail = error.detail().unwrap_or_default();
    let name = detail
        .trim_end_matches("is unknown")
        .trim()
        .trim_start_matches(kind)
        .trim();
    TemplateError::UnknownName {
        kind,
        name: if name.is_empty() {
            detail.to_string()
        } else {
            name.to_string()
        },
    }
}

pub fn task_node_ctx(
    node_did: &str,
    behavior_id: &str,
    now: &str,
) -> (serde_json::Value, serde_json::Value) {
    (
        serde_json::json!({ "node_did": node_did, "behavior_id": behavior_id }),
        serde_json::json!({ "now": now }),
    )
}

pub fn parse_template_for_validation(template: &str) -> Result<Vec<VariableRef>, TemplateError> {
    if template.len() > MAX_TEMPLATE_BYTES {
        return Err(TemplateError::Parse(format!(
            "template exceeds {} bytes",
            MAX_TEMPLATE_BYTES
        )));
    }

    // The reference scanner is not a syntax parser. Compile with the rendering
    // owner before accepting configuration, without needing invocation values.
    environment().template_from_str(template).map_err(|error| {
        TemplateError::Parse(format!(
            "{error}; use MiniJinja syntax, e.g. {{{{ doc.message }}}} for a document field or {{{{ args.name }}}} for a task argument"
        ))
    })?;

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
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'#' && bytes[i + 1] == b'}') {
                    i += 1;
                }
                if i + 1 < bytes.len() {
                    i += 2;
                }
            }
            b'{' => {
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
                let start = i + 2;
                let end = find_close(bytes, start, b'%');
                if let Some(end_idx) = end {
                    let body = &template[start..end_idx];
                    if body.trim() == "raw" {
                        match find_endraw(bytes, end_idx + 2) {
                            Some(after_endraw) => {
                                i = after_endraw;
                                continue;
                            }
                            None => break,
                        }
                    }
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

fn find_endraw(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'%' {
            let start = i + 2;
            let end = find_close(bytes, start, b'%')?;
            let body = std::str::from_utf8(&bytes[start..end]).ok()?.trim();
            if body == "endraw" {
                return Some(end + 2);
            }
            i = end + 2;
        } else {
            i += 1;
        }
    }
    None
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

fn collect_refs_in_body(body: &str, out: &mut Vec<VariableRef>) {
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if is_ident_start(c) {
            let prev = prev_non_ws_char(body, i);
            let ident_start = i;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            let ident = &body[ident_start..i];
            if prev != Some('.') && is_tracked_root(ident) {
                let mut path: Vec<String> = vec![ident.to_string()];
                loop {
                    let save = i;
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
                                i += 1;
                                while i < bytes.len() && is_ws(bytes[i]) {
                                    i += 1;
                                }
                                if i < bytes.len() && bytes[i] == b']' {
                                    i += 1;
                                    path.push(key);
                                    continue;
                                }
                            }
                            i = save;
                            break;
                        } else {
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
            let step = utf8_char_len(c);
            i += step;
        }
    }
}

fn utf8_char_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first < 0xC0 {
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
    matches!(ident, "event" | "doc" | "args" | "group" | "node" | "ctx")
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
