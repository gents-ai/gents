use crate::tool::{ToolCallEnvelope, ToolDyn, ToolFootprint};

use super::ToolEffectCall;

/// Lean `ToolExecution.Parallel.maxParallelReads`.
pub const MAX_PARALLEL_READ_TOOLS: usize = 4;

/// Lean `ToolExecution.Parallel.joinsGroup`.
pub fn joins_group(read: bool, duplicate: bool, full: bool) -> bool {
    read && !duplicate && !full
}

/// Lean `ToolExecution.Parallel.takeReads`: the length of the read prefix.
pub fn read_prefix<'a>(
    calls: impl IntoIterator<Item = (&'a str, &'a str, bool)>,
    limit: usize,
) -> usize {
    let mut seen = Vec::new();
    for (name, args, read) in calls {
        if !joins_group(read, seen.contains(&(name, args)), seen.len() >= limit) {
            break;
        }
        seen.push((name, args));
    }
    seen.len()
}

/// A call its tool's owner declares free of effects and whose arguments it admits.
pub fn is_read(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    arguments: &str,
    context: &ToolCallEnvelope,
) -> bool {
    tools
        .iter()
        .find(|tool| tool.name() == name)
        .is_some_and(|tool| {
            tool.footprint(arguments, context) == ToolFootprint::Read
                && tool.admit(arguments, context).is_ok()
        })
}

/// Lean `ToolExecution.Parallel.effectWidth` at `maxParallelReads`.
pub fn effect_width(
    calls: &[ToolEffectCall],
    tools: &[Box<dyn ToolDyn>],
    context: &ToolCallEnvelope,
) -> usize {
    if calls.is_empty() {
        return 0;
    }
    read_prefix(
        calls.iter().map(|call| {
            (
                call.name.as_str(),
                call.arguments.as_str(),
                is_read(tools, &call.name, &call.arguments, context),
            )
        }),
        MAX_PARALLEL_READ_TOOLS,
    )
    .max(1)
}

#[cfg(test)]
mod tests {
    #[test]
    fn joins_group_conforms_to_lean() {
        #[derive(serde::Deserialize, PartialEq)]
        #[serde(rename_all = "snake_case")]
        enum Footprint {
            Read,
            Barrier,
        }
        gents_lean_vocab::families::tool_execution_parallel_joins_group::replay::<Footprint>(
            |case| super::joins_group(case.footprint == Footprint::Read, case.duplicate, case.full),
        );
    }
}
