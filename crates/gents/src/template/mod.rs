//! Template rendering for event-driven tasks.

#[cfg(test)]
mod tests;

use minijinja::{AutoEscape, Environment, UndefinedBehavior};

pub mod catalog;
pub mod reads;

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
}

pub(crate) const MAX_TEMPLATE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_RENDERED_BYTES: usize = 1024 * 1024;

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
        if let Some(group) = scope.group.clone() {
            ctx.insert("group".to_string(), group);
        }
        ctx.insert("node".to_string(), scope.node.clone());
        ctx.insert("ctx".to_string(), scope.ctx.clone());
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

pub fn render_system_prompt(
    template: &str,
    node: serde_json::Value,
    cat: &catalog::Catalog,
) -> Result<String, TemplateError> {
    if !template.contains("{{") && !template.contains("{%") && !template.contains("{#") {
        return Ok(template.to_string());
    }
    reads::validate_system_template(template, cat)?;
    let scope = TemplateScope {
        event: serde_json::json!({}),
        doc: None,
        args: None,
        group: None,
        node,
        ctx: serde_json::json!({}),
    };
    render_template(template, &scope)
}

pub fn render_request_context_template(
    template: &str,
    node: serde_json::Value,
    ctx: serde_json::Value,
    cat: &catalog::Catalog,
) -> Result<String, TemplateError> {
    validate_catalog_scope(template, cat, catalog::Site::RequestContext)?;
    let scope = TemplateScope {
        event: serde_json::json!({}),
        doc: None,
        args: None,
        group: None,
        node,
        ctx,
    };
    render_template(template, &scope)
}

pub fn validate_request_context_template(
    template: &str,
    cat: &catalog::Catalog,
) -> Result<(), TemplateError> {
    validate_catalog_scope(template, cat, catalog::Site::RequestContext)
}

fn validate_catalog_scope(
    template: &str,
    cat: &catalog::Catalog,
    site: catalog::Site,
) -> Result<(), TemplateError> {
    let reads = reads::collect_request_reads(template)?;
    for var in reads {
        if !is_catalog_scoped_ref(&var) {
            continue;
        }
        if !cat.is_available_at(&var, site) {
            return Err(TemplateError::Render(format!(
                "template references unavailable variable `{var}` at {site:?}"
            )));
        }
    }
    Ok(())
}

fn is_catalog_scoped_ref(var: &str) -> bool {
    var == "node" || var == "ctx" || var.starts_with("node.") || var.starts_with("ctx.")
}

pub fn collection_summary(node: &defra_node::EmbeddedNode) -> anyhow::Result<String> {
    let mut names = node.list_collections()?;
    names.sort();
    let mut lines = Vec::with_capacity(names.len() + 1);
    lines.push(format!("collections: {}", names.len()));
    for name in names {
        let Some(collection) = node.get_collection(&name)? else {
            continue;
        };
        lines.push(format!(
            "- {}: {} fields",
            collection.name,
            collection.fields.len()
        ));
    }
    Ok(lines.join("\n"))
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
    matches!(ident, "event" | "doc" | "args" | "node" | "ctx")
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
