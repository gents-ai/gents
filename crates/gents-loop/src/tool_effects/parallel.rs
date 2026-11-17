/// Reserve invalid-call budget before dispatch and stop at any duplicate,
/// because repetition decisions depend on preceding results.
pub fn read_prefix<'a>(
    calls: impl IntoIterator<Item = (&'a str, &'a str, bool)>,
    limit: usize,
) -> usize {
    let mut seen = Vec::new();
    for (name, args, read) in calls.into_iter().take(limit) {
        if !read || seen.contains(&(name, args)) {
            break;
        }
        seen.push((name, args));
    }
    seen.len()
}

pub fn is_read(tools: &[Box<dyn crate::tool::ToolDyn>], name: &str, arguments: &str) -> bool {
    tools
        .iter()
        .find(|tool| tool.name() == name)
        .is_some_and(|tool| {
            tool.footprint(arguments) == crate::tool::ToolFootprint::Read
                && tool.admit(arguments).is_ok()
        })
}

pub fn effect_width(
    calls: &[super::ToolEffectCall],
    tools: &[Box<dyn crate::tool::ToolDyn>],
) -> usize {
    if calls.is_empty() {
        return 0;
    }
    read_prefix(
        calls.iter().map(|call| {
            (
                call.name.as_str(),
                call.arguments.as_str(),
                is_read(tools, &call.name, &call.arguments),
            )
        }),
        4,
    )
    .max(1)
}
