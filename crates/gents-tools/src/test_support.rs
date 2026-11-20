pub fn tool_policy(ceiling: &crate::surface::ToolCeiling) -> &crate::surface::ToolPolicySurface {
    ceiling.policy()
}

#[cfg(feature = "vera")]
pub mod vera_test_home;
