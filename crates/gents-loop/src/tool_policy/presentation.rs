/// Lean `ToolPresentation.isGateway`.
pub fn is_gateway(name: &str) -> bool {
    matches!(name, "script" | "config" | "load_skill" | "spawn_process")
}

/// Lean `ToolPresentation.configurationValid`.
pub fn configuration_valid(hides_others: bool, script_selected: bool, hides_gateway: bool) -> bool {
    !hides_gateway && (!hides_others || script_selected)
}

#[cfg(test)]
mod tests;
