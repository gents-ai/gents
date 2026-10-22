#[cfg(test)]
mod tests;

use minijinja::{
    machinery::{self, Instruction, Instructions},
    AutoEscape, Environment, ErrorKind, UndefinedBehavior,
};
use rapidhash::{HashSetExt, RapidHashSet};
use std::{cmp::Reverse, collections::BinaryHeap};

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

/// MiniJinja's `tojson` filter escapes HTML-sensitive characters even with auto-escaping disabled.
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
            Instruction::StoreLocal(name) => {
                bound.insert((*name).to_string());
            }
            Instruction::PushLoop(_) => {
                bound.insert("loop".to_string());
            }
            Instruction::Lookup(name) => {
                used.push((NameUse::Variable, (*name).to_string()));
            }
            Instruction::LoadConst(_) => {}
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

    let mut judged = RapidHashSet::new();
    for (use_site, name) in used {
        if !judged.insert((use_site, name.clone())) {
            continue;
        }
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

pub fn template_guards_undefined(template: &str) -> bool {
    environment()
        .template_from_str(template)
        .is_ok_and(|compiled| guards_undefined(&compiled))
}

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
    if run.re_entered >= i64::from(filter_index) || analysis.entered(name_index, filter_index) {
        return None;
    }
    let name = value.as_str()?.to_string();
    Some((use_site, name))
}

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

    pub(super) fn measure(admit: impl FnOnce()) -> Work {
        WORK.with(|work| work.set(NOTHING));
        admit();
        WORK.with(|work| work.get())
    }
}

struct TrailingRun {
    start: u32,
    re_entered: i64,
}

struct ProgramAnalysis {
    height: Vec<i64>,
    post_pop: Vec<i64>,
    run_start: Vec<u32>,
    crossing: Vec<u32>,
    landed_on: Vec<bool>,
    jumps_back_from: Vec<i64>,
}

impl ProgramAnalysis {
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

    fn crossed(&self, index: u32, filter_index: u32) -> bool {
        self.crossing
            .get(index as usize)
            .is_none_or(|&crossing| crossing <= filter_index)
    }

    fn entered(&self, index: u32, filter_index: u32) -> bool {
        self.landed_on.get(index as usize).copied().unwrap_or(true)
            || self.crossed(index, filter_index)
    }
}

struct Step {
    pops: i64,
    pushes: i64,
    keeps: Option<(u32, i64)>,
    falls_through: bool,
}

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
    Disagree,
}

fn record_arrival(arriving: &mut Arriving, kept: i64) {
    *arriving = match *arriving {
        Arriving::Nothing => Arriving::Height(kept),
        Arriving::Height(existing) if existing == kept => Arriving::Height(kept),
        _ => Arriving::Disagree,
    };
}

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

fn callable_scope_names() -> RapidHashSet<String> {
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
    let mut names: RapidHashSet<String> = template_context(&every_root)
        .as_object()
        .map(|roots| roots.keys().cloned().collect())
        .unwrap_or_default();
    names.insert("super".to_string());
    names
}

fn engine_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_ascii_whitespace()).collect()
}

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
