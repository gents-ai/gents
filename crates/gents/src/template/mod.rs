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
/// iterated or any other argument is used, so an undefined input or a dynamic
/// later argument does not spare it, and no other builtin reads an argument as
/// a name.
///
/// A keyword argument carries no name - `map(attribute=...)` looks an attribute
/// up instead - and an argument list too short to hold the name leaves the
/// builtin nothing to resolve.
///
/// The filter pops the piped value and then one value per argument, each
/// pushed by its own complete expression in source order, so the arguments
/// after the name are the shortest run of instructions ending at the filter
/// that pushes exactly that many values. The name is the argument ending just
/// before that run, and it is a name only when it is a single string constant
/// that no jump bypasses: a conditional or short-circuit name compiles to a
/// branch, so which constant reaches the lookup depends on invocation values
/// and the name is left to fire time.
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
    let trailing = arguments.checked_sub(position)?;
    let landings = outside_landings(instructions, filter_index);
    // A jump from outside `region..filter_index` landing at or after `first`
    // lets control reach the filter without running the region in order.
    // A region the landings do not cover is one this walk cannot judge, so it
    // reads as entered and leaves the name to fire time.
    let entered_from_outside = |region: u32, first: u32| {
        landings.get(region as usize).copied().unwrap_or(i64::MAX) >= i64::from(first)
    };
    let heights = expression_heights(instructions, filter_index);
    let trailing_start = (0..=filter_index).rev().find(|&start| {
        !entered_from_outside(start, start + 1) && heights.values(start) == Some(trailing)
    })?;
    let name_index = trailing_start.checked_sub(1)?;
    let Some(Instruction::LoadConst(value)) = instructions.get(name_index) else {
        return None;
    };
    if entered_from_outside(name_index, name_index) {
        return None;
    }
    let name = value.as_str()?.to_string();
    Some((use_site, name))
}

/// The furthest index at or before `filter_index` that a jump from outside
/// `region..filter_index` lands on, for every `region` in `0..=filter_index`,
/// and `-1` for a region no jump from outside lands after.
fn outside_landings(instructions: &Instructions<'_>, filter_index: u32) -> Vec<i64> {
    let landing = |index: u32| match instructions.get(index).and_then(jump_target) {
        Some(target) if target <= filter_index => i64::from(target),
        _ => -1,
    };
    let mut furthest = -1i64;
    let mut after = filter_index;
    while instructions.get(after).is_some() {
        furthest = furthest.max(landing(after));
        after += 1;
    }
    let mut landings = vec![furthest; filter_index as usize + 1];
    for region in 0..filter_index {
        furthest = furthest.max(landing(region));
        landings[region as usize + 1] = furthest;
    }
    landings
}

/// Where a run of self-contained expression code can begin, and how deep the
/// stack is along it.
struct ExpressionHeights {
    /// The stack height at every index of the pass, counted from the start of
    /// the run the index belongs to.
    height: Vec<i64>,
    /// The lowest height any instruction in `index..end` leaves after its pops.
    floor: Vec<i64>,
    /// The first index a run reaching the end of the pass can begin at.
    first: u32,
}

impl ExpressionHeights {
    /// How many values `start..end` leaves on the stack when it runs as
    /// self-contained expression code: `None` when it consumes a value pushed
    /// before `start`, when an instruction in it has no fixed stack effect or
    /// jumps outside `start..=end`, or when branches in it join on disagreeing
    /// stack heights.
    ///
    /// These heights are one run's, so they answer for a `start` no jump from
    /// outside `start..end` lands after: such a jump carries into the run a join
    /// that `start..end` does not have. The caller rules one out itself.
    fn values(&self, start: u32) -> Option<u32> {
        if start < self.first {
            return None;
        }
        let index = usize::try_from(start).ok()?;
        let base = *self.height.get(index)?;
        if *self.floor.get(index)? < base {
            return None;
        }
        u32::try_from(self.height.last().copied()? - base).ok()
    }
}

/// Reads `0..=end` as expression code in one forward pass, so that judging a
/// candidate start costs a comparison instead of a rescan.
///
/// An instruction with no fixed stack effect and a jump leaving `index..=end`
/// both begin a new run after themselves, because control leaves the run there.
/// An index nothing reaches and a join on disagreeing heights begin one at
/// themselves: a run covering such an index is not reading a single expression,
/// while a run beginning at it carries neither the jump that skipped it nor the
/// joins that disagree.
fn expression_heights(instructions: &Instructions<'_>, end: u32) -> ExpressionHeights {
    let span = end as usize + 1;
    let mut height = Vec::with_capacity(span);
    let mut post_pop = Vec::with_capacity(span);
    let mut arriving = vec![Arriving::Nothing; span];
    let mut fall_through = Some(0i64);
    let mut first = 0u32;
    for index in 0..=end {
        let depth = match join_height(fall_through, arriving[index as usize]) {
            Some(Some(depth)) => depth,
            _ => {
                first = index;
                0
            }
        };
        height.push(depth);
        if index == end {
            break;
        }
        match instructions
            .get(index)
            .and_then(|instruction| expression_step(instruction, depth, index, end))
        {
            Some(step) => {
                post_pop.push(depth - step.pops);
                if let Some((target, kept)) = step.keeps {
                    record_arrival(&mut arriving[target as usize], kept);
                }
                fall_through = step
                    .falls_through
                    .then_some(depth - step.pops + step.pushes);
            }
            None => {
                first = index + 1;
                post_pop.push(i64::MAX);
                fall_through = Some(0);
            }
        }
    }
    let mut floor = vec![i64::MAX; span];
    for index in (0..post_pop.len()).rev() {
        floor[index] = floor[index + 1].min(post_pop[index]);
    }
    ExpressionHeights {
        height,
        floor,
        first,
    }
}

/// What one instruction does to the stack.
struct Step {
    pops: i64,
    pushes: i64,
    /// The target a jump in the instruction takes, and the height it keeps there.
    keeps: Option<(u32, i64)>,
    /// Whether the next instruction continues from this one.
    falls_through: bool,
}

/// The stack effect of one expression instruction; `None` for an instruction
/// whose effect is not fixed, and for a jump leaving `index..=end`, which takes
/// control out of the run rather than through it.
///
/// `JumpIfFalse` pops its condition however it goes, so its target runs one
/// value lower; `JumpIfFalseOrPop` and `JumpIfTrueOrPop` pop only where they
/// fall through, so the branch they take keeps the condition.
fn expression_step(
    instruction: &Instruction<'_>,
    depth: i64,
    index: u32,
    end: u32,
) -> Option<Step> {
    let (pops, pushes, keeps, falls_through) = match instruction {
        Instruction::LoadConst(_) | Instruction::Lookup(_) => (0, 1, None, true),
        Instruction::GetAttr(_) | Instruction::Not | Instruction::Neg => (1, 1, None, true),
        Instruction::GetItem
        | Instruction::Add
        | Instruction::Sub
        | Instruction::Mul
        | Instruction::Div
        | Instruction::IntDiv
        | Instruction::Rem
        | Instruction::Pow
        | Instruction::Eq
        | Instruction::Ne
        | Instruction::Gt
        | Instruction::Gte
        | Instruction::Lt
        | Instruction::Lte
        | Instruction::StringConcat
        | Instruction::In => (2, 1, None, true),
        Instruction::Slice => (4, 1, None, true),
        Instruction::CompareAndPreserve(_) | Instruction::Swap => (2, 2, None, true),
        Instruction::DupTop => (1, 2, None, true),
        Instruction::DiscardTop => (1, 0, None, true),
        Instruction::BuildMap(pairs) | Instruction::BuildKwargs(pairs) => (
            i64::from(u32::try_from(*pairs).ok()?.checked_mul(2)?),
            1,
            None,
            true,
        ),
        Instruction::MergeKwargs(count) | Instruction::BuildList(Some(count)) => {
            (i64::from(u32::try_from(*count).ok()?), 1, None, true)
        }
        Instruction::UnpackList(count) => (1, i64::from(u32::try_from(*count).ok()?), None, true),
        Instruction::ApplyFilter(_, Some(count), _)
        | Instruction::PerformTest(_, Some(count), _)
        | Instruction::CallFunction(_, Some(count))
        | Instruction::CallMethod(_, Some(count))
        | Instruction::CallObject(Some(count)) => (i64::from(*count), 1, None, true),
        Instruction::JumpIfFalse(target) => (1, 0, Some((*target, depth - 1)), true),
        Instruction::JumpIfFalseOrPop(target) | Instruction::JumpIfTrueOrPop(target) => {
            (1, 0, Some((*target, depth)), true)
        }
        Instruction::Jump(target) => (0, 0, Some((*target, depth)), false),
        _ => return None,
    };
    if let Some((target, _)) = keeps {
        if !(index + 1..=end).contains(&target) {
            return None;
        }
    }
    Some(Step {
        pops,
        pushes,
        keeps,
        falls_through,
    })
}

/// What the jumps into one index keep on the stack.
#[derive(Clone, Copy)]
enum Arriving {
    Nothing,
    Height(i64),
    /// Two jumps keep different heights there, so the index has no one height.
    Disagree,
}

fn record_arrival(arriving: &mut Arriving, kept: i64) {
    *arriving = match *arriving {
        Arriving::Nothing => Arriving::Height(kept),
        Arriving::Height(existing) if existing == kept => Arriving::Height(kept),
        _ => Arriving::Disagree,
    };
}

/// The stack height at an index, joining the fall-through height with the one
/// the jumps into it keep; `Some(None)` when nothing reaches it and `None` when
/// the arriving heights disagree.
fn join_height(fall_through: Option<i64>, arriving: Arriving) -> Option<Option<i64>> {
    match (fall_through, arriving) {
        (_, Arriving::Disagree) => None,
        (Some(height), Arriving::Height(kept)) if height != kept => None,
        (Some(height), _) => Some(Some(height)),
        (None, Arriving::Height(kept)) => Some(Some(kept)),
        (None, Arriving::Nothing) => Some(None),
    }
}

fn jump_target(instruction: &Instruction<'_>) -> Option<u32> {
    match instruction {
        Instruction::Jump(target)
        | Instruction::JumpIfFalse(target)
        | Instruction::JumpIfFalseOrPop(target)
        | Instruction::JumpIfTrueOrPop(target)
        | Instruction::Iterate(target) => Some(*target),
        _ => None,
    }
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
