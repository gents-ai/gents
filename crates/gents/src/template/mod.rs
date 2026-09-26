//! Template rendering for event-driven tasks.

#[cfg(test)]
mod tests;

use minijinja::{
    machinery::{self, Instruction, Instructions},
    AutoEscape, Environment, ErrorKind, UndefinedBehavior,
};
use std::collections::HashSet;

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

/// Authoring and execution resolve names against this one environment, so a
/// name admission accepts is a name a fire can run.
///
/// `tojson` rewrites `<`, `>`, `&` and `'` into `\uXXXX` escapes inside the
/// filter body rather than through the formatter, so the auto-escape callback
/// below does not make it escape-neutral: text piped through it reaches the
/// provider carrying those escape sequences instead of the source characters.
/// `{% autoescape %}` likewise decides from the value the tag names rather than
/// from the callback, so a block under `{% autoescape "json" %}` reaches the
/// provider JSON-serialized and one under `{% autoescape true %}` HTML-escaped.
fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_auto_escape_callback(|_| AutoEscape::None);
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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum NameUse {
    Filter,
    Test,
    Function,
    FilterArgument,
    TestArgument,
}

impl NameUse {
    fn label(self) -> &'static str {
        match self {
            NameUse::Filter | NameUse::FilterArgument => "filter",
            NameUse::Test | NameUse::TestArgument => "test",
            NameUse::Function => "function",
        }
    }
}

/// Filters, tests and functions resolve while rendering, so compiling a
/// template admits names the engine can never provide and every fire of the
/// task then fails. Configuration admission rejects those names here.
///
/// MiniJinja names each filter, test and called function in one flat
/// instruction stream, so a branch no render would enter cannot hide one.
/// Block bodies compile into a second stream this walk does not visit, and
/// cannot occur: `{% block %}` needs MiniJinja's `multi_template` feature,
/// which the workspace does not enable, so it fails to parse.
///
/// Compiling is part of the check, so this also reports a template that does
/// not parse or exceeds the size cap, as a parse error rather than a name.
///
/// A rejection reports only that nothing can provide a name, never that a
/// render failed: invocation values do not exist at authoring time, so
/// value-dependent failures are not admission facts. For the same reason a
/// method call (`{{ doc.name.upper() }}`) is out of reach here - it resolves
/// against the value it is called on, which authoring does not have.
///
/// Three kinds of name survive to fire time, each so that a template a fire
/// would run is not refused. A name a builtin resolves from a non-constant
/// argument is unpredictable (`{{ items | map(doc.filter_name) }}`), as is one
/// an argument splat hides (`{{ items | map(*args.spec) }}`). And a called name
/// is admitted when the program binds it anywhere, without regard to scope or
/// order, so `{% if doc.fmt %}{% set f = doc.fmt %}{% endif %}{{ f() }}` and
/// `{{ f() }}{% set f = args.formatter %}` are admitted and fail on a fire.
pub fn check_template_vocabulary(template: &str) -> Result<(), TemplateError> {
    if template.len() > MAX_TEMPLATE_BYTES {
        return Err(TemplateError::Parse(format!(
            "template exceeds {} bytes",
            MAX_TEMPLATE_BYTES
        )));
    }

    let env = environment();
    let compiled = env
        .template_from_str(template)
        .map_err(|error| TemplateError::Parse(error.to_string()))?;

    let mut used: Vec<(NameUse, String)> = Vec::new();
    let mut bound = callable_scope_names();
    let instructions = &machinery::get_compiled_template(&compiled).instructions;
    let mut index = 0u32;
    while let Some(instruction) = instructions.get(index) {
        match instruction {
            Instruction::ApplyFilter(name, arguments, _) => {
                let name = engine_name(name);
                let argument = name_argument(instructions, index, &name, *arguments);
                used.push((NameUse::Filter, name));
                used.extend(argument);
            }
            Instruction::PerformTest(name, _, _) => {
                used.push((NameUse::Test, engine_name(name)));
            }
            Instruction::CallFunction(name, _) => {
                used.push((NameUse::Function, (*name).to_string()));
            }
            // Assignment targets, `{% with %}` bindings and loop targets all
            // compile to this. A call resolves through the frames before the
            // environment, and admission judges the bindings program-wide,
            // without regard to scope or order.
            Instruction::StoreLocal(name) => {
                bound.insert((*name).to_string());
            }
            // A recursive loop calls itself by this name.
            Instruction::PushLoop(_) => {
                bound.insert("loop".to_string());
            }
            // Judged by the filter that resolves a name from it, not here.
            Instruction::LoadConst(_) => {}
            // Named exhaustively: a MiniJinja instruction set that grows a new
            // name-carrying instruction, or changes the arity of one above,
            // must fail to compile rather than leave the walk silently partial.
            Instruction::EmitRaw(_)
            | Instruction::Lookup(_)
            | Instruction::GetAttr(_)
            | Instruction::SetAttr(_)
            | Instruction::GetItem
            | Instruction::Slice
            | Instruction::BuildMap(_)
            | Instruction::BuildKwargs(_)
            | Instruction::MergeKwargs(_)
            | Instruction::BuildList(_)
            | Instruction::UnpackList(_)
            | Instruction::UnpackLists(_)
            | Instruction::Add
            | Instruction::Sub
            | Instruction::Mul
            | Instruction::Div
            | Instruction::IntDiv
            | Instruction::Rem
            | Instruction::Pow
            | Instruction::Neg
            | Instruction::Eq
            | Instruction::Ne
            | Instruction::Gt
            | Instruction::Gte
            | Instruction::Lt
            | Instruction::Lte
            | Instruction::Not
            | Instruction::StringConcat
            | Instruction::In
            | Instruction::CompareAndPreserve(_)
            | Instruction::Emit
            | Instruction::PushWith
            | Instruction::Iterate(_)
            | Instruction::PushDidNotIterate
            | Instruction::PopFrame
            | Instruction::PopLoopFrame
            | Instruction::Jump(_)
            | Instruction::JumpIfFalse(_)
            | Instruction::JumpIfFalseOrPop(_)
            | Instruction::JumpIfTrueOrPop(_)
            | Instruction::PushAutoEscape
            | Instruction::PopAutoEscape
            | Instruction::BeginCapture(_)
            | Instruction::EndCapture
            | Instruction::CallMethod(_, _)
            | Instruction::CallObject(_)
            | Instruction::DupTop
            | Instruction::DiscardTop
            | Instruction::FastSuper
            | Instruction::FastRecurse
            | Instruction::Swap => {}
        }
        index += 1;
    }

    let mut judged = HashSet::new();
    for (use_site, name) in used {
        if !judged.insert((use_site, name.clone())) {
            continue;
        }
        let provided = match use_site {
            NameUse::Filter => engine_resolves(&env, &format!("{{{{ 0 | {name} }}}}"), None),
            NameUse::Test => engine_resolves(&env, &format!("{{{{ 0 is {name} }}}}"), None),
            NameUse::FilterArgument => engine_resolves(&env, "{{ [] | map(probe) }}", Some(&name)),
            NameUse::TestArgument => engine_resolves(&env, "{{ [] | select(probe) }}", Some(&name)),
            NameUse::Function => {
                bound.contains(&name) || env.globals().any(|(global, _)| global == name)
            }
        };
        if !provided {
            return Err(TemplateError::UnknownName {
                kind: use_site.label(),
                name,
            });
        }
    }
    Ok(())
}

/// `map` resolves a filter, and the `select`/`reject` family a test, from a
/// string argument while rendering: the only names the engine looks up from a
/// value rather than from the source. The lookup runs before the piped value is
/// iterated, so an undefined input does not spare it, and no other builtin
/// reads an argument as a name.
///
/// A keyword argument carries no name - `map(attribute=...)` looks an attribute
/// up instead - and an argument list too short to hold the name leaves the
/// builtin nothing to resolve.
///
/// The filter pops the piped value and then one value per argument, each of
/// which its expression pushes exactly once, so a run of that many constants
/// ending at the filter pushed the arguments themselves, in order. A conditional
/// argument compiles a constant per branch and its false branch ends the run, so
/// `map('a' if c else 'b')` is judged on `b` alone.
fn name_argument(
    instructions: &Instructions<'_>,
    filter_index: u32,
    filter: &str,
    arguments: Option<u16>,
) -> Option<(NameUse, String)> {
    let (use_site, position) = match filter {
        "map" => (NameUse::FilterArgument, 1u32),
        "select" | "reject" => (NameUse::TestArgument, 1),
        "selectattr" | "rejectattr" => (NameUse::TestArgument, 2),
        _ => return None,
    };
    let arguments = u32::from(arguments?).checked_sub(1)?;
    if arguments < position {
        return None;
    }
    let first = filter_index.checked_sub(arguments)?;
    let mut resolved = None;
    for offset in 0..arguments {
        match instructions.get(first + offset) {
            Some(Instruction::LoadConst(value)) => {
                if offset + 1 == position {
                    resolved = value.as_str().map(str::to_string);
                }
            }
            _ => return None,
        }
    }
    resolved.map(|name| (use_site, name))
}

/// Names a call resolves to without the environment providing them: the VM
/// looks a called name up through the context frames and the invocation scope
/// before it reaches the globals, and resolves `super` itself without any
/// lookup at all. The scope roots come from the sole owner of the render
/// context, so admission cannot drift from what a fire supplies.
fn callable_scope_names() -> HashSet<String> {
    let every_root = TemplateScope {
        event: serde_json::Value::Null,
        doc: Some(serde_json::Value::Null),
        args: Some(serde_json::Value::Null),
        group: Some(serde_json::Value::Null),
        node: serde_json::Value::Null,
        ctx: serde_json::Value::Null,
    };
    let mut names: HashSet<String> = template_context(&every_root)
        .as_object()
        .map(|roots| roots.keys().cloned().collect())
        .unwrap_or_default();
    names.insert("super".to_string());
    names
}

/// The virtual machine ignores whitespace inside a filter or test name.
fn engine_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_ascii_whitespace()).collect()
}

/// Only the engine's own refusal of the name counts: any other probe failure is
/// about the probe's operand, so the name exists.
///
/// A name the source carries is a lexed identifier, so it interpolates into a
/// probe that passes no argument; and with no argument no builtin reaches a name
/// lookup of its own, so `{{ 0 | select }}` reports its operand rather than
/// `select`'s absent test argument as an unknown name. A name a filter argument
/// carries is arbitrary text, so `argument` binds it through the context
/// instead, and that probe pipes an empty sequence so the builtin's own lookup
/// is all that can fail.
fn engine_resolves(env: &Environment<'static>, probe: &str, argument: Option<&str>) -> bool {
    let rendered = match argument {
        Some(name) => env.render_str(probe, serde_json::json!({ "probe": name })),
        None => env.render_str(probe, ()),
    };
    !matches!(
        rendered.err().map(|error| error.kind()),
        Some(ErrorKind::UnknownFilter | ErrorKind::UnknownTest)
    )
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
