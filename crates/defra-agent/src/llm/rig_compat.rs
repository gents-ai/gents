//! Converters between Defra-native [`crate::llm`] types and rig's, used only at
//! the provider/parsing boundary (Layer A). Deleted once Layer A is owned.
//!
//! These are free functions rather than `From` impls: rig's types are foreign,
//! so `impl From<Native> for RigType` would violate the orphan rule.

use super::tool::ToolDefinition;
use super::ToolChoice;

/// Convert a native [`ToolDefinition`] into rig's, for the outgoing completion
/// request's tool list.
pub(crate) fn to_rig_tool_definition(def: &ToolDefinition) -> rig::completion::ToolDefinition {
    rig::completion::ToolDefinition {
        name: def.name.clone(),
        description: def.description.clone(),
        parameters: def.parameters.clone(),
    }
}

/// Convert a native [`ToolChoice`] into rig's, for the outgoing completion request.
pub(crate) fn to_rig_tool_choice(choice: &ToolChoice) -> rig::message::ToolChoice {
    match choice {
        ToolChoice::Auto => rig::message::ToolChoice::Auto,
        ToolChoice::None => rig::message::ToolChoice::None,
        ToolChoice::Required => rig::message::ToolChoice::Required,
        ToolChoice::Specific { function_names } => rig::message::ToolChoice::Specific {
            function_names: function_names.clone(),
        },
    }
}
