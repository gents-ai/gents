pub fn tool_policy(
    ceiling: &crate::tools::surface::ToolCeiling,
) -> &crate::tools::surface::ToolPolicySurface {
    ceiling.policy()
}
