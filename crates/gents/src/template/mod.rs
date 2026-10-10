//! Template rendering for event-driven tasks.

#[cfg(test)]
mod tests;

use minijinja::{
    machinery::{self, Instruction, Instructions},
    AutoEscape, Environment, ErrorKind, UndefinedBehavior,
};
use std::{cmp::Reverse, collections::BinaryHeap, collections::HashSet};

pub mod catalog;

pub struct TemplateScope {
    pub session: Option<serde_json::Value>,
    pub request: Option<serde_json::Value>,
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
    if let Some(session) = &scope.session {
        ctx.insert("session".into(), session.clone());
    }
    if let Some(request) = &scope.request {
        ctx.insert("request".into(), request.clone());
    }
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
    Variable,
}

impl NameUse {
    fn label(self) -> &'static str {
        match self {
            NameUse::Filter | NameUse::FilterArgument => "filter",
            NameUse::Test | NameUse::TestArgument => "test",
            NameUse::Function => "function",
            NameUse::Variable => "variable",
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
    let mut analysis = None;
    let mut index = 0u32;
    while let Some(instruction) = instructions.get(index) {
        match instruction {
            Instruction::ApplyFilter(name, arguments, _) => {
                let name = engine_name(name);
                let argument = name_argument(instructions, &mut analysis, index, &name, *arguments);
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
            // A bare variable resolves through the frames, then the render
            // context roots, then the environment's globals; strict undefined
            // fails every fire on anything else (#1970).
            Instruction::Lookup(name) => {
                used.push((NameUse::Variable, (*name).to_string()));
            }
            // Judged by the filter that resolves a name from it, not here.
            Instruction::LoadConst(_) => {}
            // Named exhaustively: a MiniJinja instruction set that grows a new
            // name-carrying instruction, or changes the arity of one above,
            // must fail to compile rather than leave the walk silently partial.
            Instruction::EmitRaw(_)
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
        // A default filter or definedness test renders an absent value, and
        // which lookup it guards is value flow this walk does not follow, so a
        // template that uses one anywhere leaves its variables to fire time.
        if use_site == NameUse::Variable && guards_undefined(&compiled) {
            continue;
        }
        let provided = match use_site {
            NameUse::Filter => engine_resolves(&env, &format!("{{{{ 0 | {name} }}}}"), None),
            NameUse::Test => engine_resolves(&env, &format!("{{{{ 0 is {name} }}}}"), None),
            NameUse::FilterArgument => engine_resolves(&env, "{{ [] | map(probe) }}", Some(&name)),
            NameUse::TestArgument => engine_resolves(&env, "{{ [] | select(probe) }}", Some(&name)),
            NameUse::Function | NameUse::Variable => {
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

/// Whether a compiled template uses a filter or test that renders an absent
/// value (`default`, `d`, `defined`, `undefined`, `none`).
fn guards_undefined(compiled: &minijinja::Template<'_, '_>) -> bool {
    let instructions = &machinery::get_compiled_template(compiled).instructions;
    (0..)
        .map_while(|index| instructions.get(index))
        .any(|instruction| match instruction {
            Instruction::ApplyFilter(name, _, _) => {
                matches!(engine_name(name).as_str(), "default" | "d")
            }
            Instruction::PerformTest(name, _, _) => {
                matches!(engine_name(name).as_str(), "defined" | "undefined" | "none")
            }
            _ => false,
        })
}

/// Whether a template renders an absent value somewhere, so configure-time
/// field checks cannot tell an optional field from a misspelled one.
pub fn template_guards_undefined(template: &str) -> bool {
    environment()
        .template_from_str(template)
        .is_ok_and(|compiled| guards_undefined(&compiled))
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
    analysis: &mut Option<ProgramAnalysis>,
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
    let analysis = analysis.get_or_insert_with(|| ProgramAnalysis::read(instructions));
    let run = analysis.trailing_run(instructions, filter_index, trailing)?;
    let name_index = run.start.checked_sub(1)?;
    let Some(Instruction::LoadConst(value)) = instructions.get(name_index) else {
        return None;
    };
    // A jump from outside `name_index..filter_index` landing on the name or
    // after it lets control reach the filter without reading the name.
    if run.re_entered >= i64::from(filter_index) || analysis.entered(name_index, filter_index) {
        return None;
    }
    let name = value.as_str()?.to_string();
    Some((use_site, name))
}

/// Counts what admission's analysis reads, so that a cost regression fails a
/// test instead of only lengthening the suite.
#[cfg(test)]
mod work {
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug)]
    pub(super) struct Work {
        pub(super) program_passes: u64,
        pub(super) indices: u64,
        pub(super) candidates: u64,
    }

    const NOTHING: Work = Work {
        program_passes: 0,
        indices: 0,
        candidates: 0,
    };

    thread_local! {
        static WORK: Cell<Work> = const { Cell::new(NOTHING) };
    }

    pub(super) fn record_program_pass(indices: u64) {
        WORK.with(|work| {
            let mut counted = work.get();
            counted.program_passes += 1;
            counted.indices += indices;
            work.set(counted);
        });
    }

    pub(super) fn record_candidate() {
        WORK.with(|work| {
            let mut counted = work.get();
            counted.candidates += 1;
            work.set(counted);
        });
    }

    /// The counts are per thread, so a caller must hold the thread for the whole
    /// admission it measures.
    pub(super) fn measure(admit: impl FnOnce()) -> Work {
        WORK.with(|work| work.set(NOTHING));
        admit();
        WORK.with(|work| work.get())
    }
}

/// Where the arguments after the name begin, and the furthest index at or after
/// the filter that jumps back into that run or onto the name before it.
struct TrailingRun {
    start: u32,
    re_entered: i64,
}

/// The whole program read once, as expression code and as a jump graph, so that
/// every filter judges its candidate runs by comparison instead of by a pass of
/// its own. A template spells a filter out in a few instructions, so the number
/// of filters grows with the program and a pass per filter costs the square of
/// its length.
struct ProgramAnalysis {
    /// The stack height at every index, counted from the start of the run the
    /// index belongs to.
    height: Vec<i64>,
    /// The height every index leaves after its pops, and `i64::MAX` where the
    /// instruction has no fixed stack effect.
    post_pop: Vec<i64>,
    /// The first index the run reaching every index can begin at.
    run_start: Vec<u32>,
    /// The nearest index after every index that a jump from before it lands on,
    /// and `u32::MAX` where no jump passes over it.
    crossing: Vec<u32>,
    /// Whether a jump from before every index lands on it.
    landed_on: Vec<bool>,
    /// The furthest index at or after every index that jumps back to it, and
    /// `-1` where none does.
    jumps_back_from: Vec<i64>,
}

impl ProgramAnalysis {
    /// An instruction with no fixed stack effect and a jump leaving the program
    /// both begin a new run after themselves, because control leaves the run
    /// there. An index nothing reaches and a join on disagreeing heights begin
    /// one at themselves: a run covering such an index is not reading a single
    /// expression, while a run beginning at it carries neither the jump that
    /// skipped it nor the joins that disagree.
    ///
    /// A jump landing past a filter would have ended the run in a pass reading
    /// only the indices up to that filter. This pass keeps such a run going, so
    /// `trailing_run` rules that jump out of the run it judges instead.
    fn read(instructions: &Instructions<'_>) -> Self {
        let span = u32::try_from(instructions.len()).unwrap_or(u32::MAX);
        let capacity = span as usize;
        let end = span.saturating_sub(1);
        let mut height = Vec::with_capacity(capacity);
        let mut post_pop = Vec::with_capacity(capacity);
        let mut run_start = Vec::with_capacity(capacity);
        let mut crossing = Vec::with_capacity(capacity);
        let mut landed_on = vec![false; capacity];
        let mut jumps_back_from = vec![-1i64; capacity];
        let mut arriving = vec![Arriving::Nothing; capacity];
        // Every target of a jump from an earlier index that no index so far has
        // reached, nearest first.
        let mut pending: BinaryHeap<Reverse<u32>> = BinaryHeap::new();
        let mut fall_through = Some(0i64);
        let mut first = 0u32;
        for index in 0..span {
            while let Some(Reverse(target)) = pending.peek().copied() {
                if target > index {
                    break;
                }
                pending.pop();
                landed_on[index as usize] = true;
            }
            crossing.push(pending.peek().map_or(u32::MAX, |Reverse(target)| *target));
            let depth = match join_height(fall_through, arriving[index as usize]) {
                Some(Some(depth)) => depth,
                _ => {
                    first = index;
                    0
                }
            };
            height.push(depth);
            run_start.push(first);
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
            if let Some(target) = instructions.get(index).and_then(jump_target) {
                if target > index {
                    pending.push(Reverse(target));
                } else {
                    jumps_back_from[target as usize] = i64::from(index);
                }
            }
        }
        #[cfg(test)]
        work::record_program_pass(u64::from(span));
        Self {
            height,
            post_pop,
            run_start,
            crossing,
            landed_on,
            jumps_back_from,
        }
    }

    /// The last index at or before `filter_index` from which the instructions up
    /// to the filter run as self-contained expression code leaving exactly
    /// `trailing` values on the stack: `None` when every candidate consumes a
    /// value pushed before it, holds an instruction with no fixed stack effect,
    /// joins branches on disagreeing heights or leaves a different count.
    ///
    /// The heights are the whole program's one running count, so they answer for
    /// a run only where no jump enters `start..filter_index` from outside it and
    /// none leaves it: an arriving jump carries into the run a join the run does
    /// not have, and a jump out of it leaves the run altogether where a pass
    /// reading only up to the filter would have restarted the count. Both are
    /// ruled out here rather than read off the heights.
    fn trailing_run(
        &self,
        instructions: &Instructions<'_>,
        filter_index: u32,
        trailing: u32,
    ) -> Option<TrailingRun> {
        let filter = usize::try_from(filter_index).ok()?;
        let filter_height = *self.height.get(filter)?;
        let mut floor = i64::MAX;
        let mut leaves = 0u32;
        let mut re_entered = -1i64;
        let mut start = filter_index;
        loop {
            #[cfg(test)]
            work::record_candidate();
            let base = self.height[start as usize];
            if floor >= base
                && leaves <= filter_index
                && re_entered < i64::from(filter_index)
                && !self.crossed(start, filter_index)
                && u32::try_from(filter_height - base).is_ok_and(|values| values == trailing)
            {
                re_entered = re_entered.max(self.jumps_back_from[start as usize]);
                if let Some(name) = start.checked_sub(1) {
                    re_entered = re_entered.max(self.jumps_back_from[name as usize]);
                }
                return Some(TrailingRun { start, re_entered });
            }
            if start == 0 || start <= self.run_start[filter] {
                return None;
            }
            start -= 1;
            floor = floor.min(self.post_pop[start as usize]);
            if let Some(target) = instructions.get(start).and_then(jump_target) {
                leaves = leaves.max(target);
            }
            re_entered = re_entered.max(self.jumps_back_from[start as usize + 1]);
        }
    }

    /// Whether a jump from before `index` lands after it, at or before
    /// `filter_index`. An index the pass did not cover reads as crossed, because
    /// this walk cannot judge it.
    fn crossed(&self, index: u32, filter_index: u32) -> bool {
        self.crossing
            .get(index as usize)
            .is_none_or(|&crossing| crossing <= filter_index)
    }

    /// Whether a jump from before `index` lands on it or after it, at or before
    /// `filter_index`.
    fn entered(&self, index: u32, filter_index: u32) -> bool {
        self.landed_on.get(index as usize).copied().unwrap_or(true)
            || self.crossed(index, filter_index)
    }
}

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
        session: Some(serde_json::Value::Null),
        request: Some(serde_json::Value::Null),
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
    agent_id: &str,
    now: &str,
) -> (serde_json::Value, serde_json::Value) {
    (
        serde_json::json!({ "node_did": node_did, "agent_id": agent_id }),
        serde_json::json!({ "now": now }),
    )
}

/// Render a Task's prompt and its Goal declaration, if any, in `scope`.
pub(crate) fn render_task(
    prompt_template: &str,
    goal_objective_template: Option<&str>,
    goal_token_budget: Option<i64>,
    scope: &TemplateScope,
) -> Result<(String, Option<String>), String> {
    let prompt =
        render_template(prompt_template, scope).map_err(|error| format!("template: {error}"))?;
    let objective = goal_objective_template
        .map(|template| render_template(template, scope))
        .transpose()
        .map_err(|error| format!("goal template: {error}"))?;
    crate::goal::validate_task_goal_declaration(objective.as_deref(), goal_token_budget)
        .map_err(|error| format!("goal declaration: {error}"))?;
    Ok((prompt, objective))
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
    matches!(
        ident,
        "event" | "doc" | "args" | "group" | "node" | "ctx" | "session" | "request"
    )
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
