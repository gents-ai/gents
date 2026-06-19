use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::Serialize;

use crate::defra_query::DEFRA_QUERY_TOOL_NAME;
use crate::meta_tools::META_TOOL_NAMES;
use crate::toolset::{
    background_tool_names, orchestration_tool_names, subagent_tool_names, CONTEXT_BUDGET_TOOL_NAME,
    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME, SESSION_HISTORY_TOOL_NAME,
};

use super::{BehaviorToolConfig, ToolSurface};

const MEMORY_TOOL_NAME: &str = "memory";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolSurfaceExplanation {
    pub tool_names: Vec<String>,
    pub included: BTreeMap<String, Vec<String>>,
    pub excluded: BTreeMap<String, Vec<String>>,
    pub unavailable: BTreeMap<String, Vec<String>>,
    pub warnings: Vec<ToolSurfaceWarning>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolSurfaceWarning {
    pub code: String,
    pub message: String,
}

impl ToolSurfaceExplanation {
    pub(crate) fn from_resolved(
        config: &BehaviorToolConfig,
        surface: &ToolSurface,
    ) -> ToolSurfaceExplanation {
        let mut builder = ExplanationBuilder::default();

        builder.include_many("host", surface.host_tools.tool_names());
        explain_meta(config, surface, &mut builder);
        explain_subagents(config, surface, &mut builder);
        explain_orchestration(config, surface, &mut builder);
        explain_background(config, surface, &mut builder);
        builder.include_many(
            "custom",
            surface
                .custom_tools
                .iter()
                .map(|tool| tool.name().to_string()),
        );
        explain_memory(config, &mut builder);
        explain_builtin_reads(config, surface, &mut builder);

        let tool_names = surface.tool_names();
        if surface.host_tools.tool_names().is_empty()
            && tool_names.iter().any(|name| {
                name == CONTEXT_BUDGET_TOOL_NAME
                    || name == SESSION_HISTORY_TOOL_NAME
                    || name == DEFRA_QUERY_TOOL_NAME
            })
        {
            builder.warn(
                "host_ceiling_not_global",
                "ToolCeiling currently clamps host-native file/bash/CLI tools only; built-in read tools can still be model-callable.",
            );
        }

        builder.finish(tool_names)
    }
}

impl BehaviorToolConfig {
    pub fn explain_with_runtime(
        &self,
        mcp_services_online: bool,
        own_agent_did: &str,
        active_behavior_ids: &HashSet<String>,
    ) -> ToolSurfaceExplanation {
        let surface = self.resolve_with_available_subagent_targets_for_mcp_presence(
            mcp_services_online,
            own_agent_did,
            active_behavior_ids,
        );
        ToolSurfaceExplanation::from_resolved(self, &surface)
    }
}

#[derive(Default)]
struct ExplanationBuilder {
    included: BTreeMap<String, BTreeSet<String>>,
    excluded: BTreeMap<String, BTreeSet<String>>,
    unavailable: BTreeMap<String, BTreeSet<String>>,
    warnings: Vec<ToolSurfaceWarning>,
}

impl ExplanationBuilder {
    fn include_many<I>(&mut self, category: &str, names: I)
    where
        I: IntoIterator<Item = String>,
    {
        for name in names {
            self.insert(category, name, SurfaceStatus::Included);
        }
    }

    fn exclude(&mut self, category: &str, name: impl Into<String>) {
        self.insert(category, name.into(), SurfaceStatus::Excluded);
    }

    fn unavailable(&mut self, category: &str, name: impl Into<String>) {
        self.insert(category, name.into(), SurfaceStatus::Unavailable);
    }

    fn warn(&mut self, code: impl Into<String>, message: impl Into<String>) {
        let code = code.into();
        if self.warnings.iter().any(|warning| warning.code == code) {
            return;
        }
        self.warnings.push(ToolSurfaceWarning {
            code,
            message: message.into(),
        });
    }

    fn finish(self, tool_names: Vec<String>) -> ToolSurfaceExplanation {
        ToolSurfaceExplanation {
            tool_names,
            included: into_vec_map(self.included),
            excluded: into_vec_map(self.excluded),
            unavailable: into_vec_map(self.unavailable),
            warnings: self.warnings,
        }
    }

    fn insert(&mut self, category: &str, name: String, status: SurfaceStatus) {
        let map = match status {
            SurfaceStatus::Included => &mut self.included,
            SurfaceStatus::Excluded => &mut self.excluded,
            SurfaceStatus::Unavailable => &mut self.unavailable,
        };
        map.entry(category.to_string()).or_default().insert(name);
    }
}

enum SurfaceStatus {
    Included,
    Excluded,
    Unavailable,
}

fn into_vec_map(map: BTreeMap<String, BTreeSet<String>>) -> BTreeMap<String, Vec<String>> {
    map.into_iter()
        .map(|(category, names)| (category, names.into_iter().collect()))
        .collect()
}

fn explain_meta(
    config: &BehaviorToolConfig,
    surface: &ToolSurface,
    builder: &mut ExplanationBuilder,
) {
    if surface.include_meta_tools {
        builder.include_many(
            "meta_mcp",
            META_TOOL_NAMES.iter().map(|name| (*name).to_string()),
        );
        if surface.allowed_mcp_service_ids.is_empty() {
            builder.warn(
                "mcp_empty_allowlist_all",
                "allowed_mcp_service_ids is empty, which currently means all online MCP services.",
            );
        }
        return;
    }

    for name in META_TOOL_NAMES {
        if config.meta_tools_requested() {
            builder.unavailable("meta_mcp", name.to_string());
        } else {
            builder.exclude("meta_mcp", name.to_string());
        }
    }
    if config.meta_tools_requested() {
        builder.warn(
            "meta_requested_no_online_mcp",
            "Meta tools are configured on, but no ToolServiceRegistry row is currently online.",
        );
    }
}

fn explain_subagents(
    config: &BehaviorToolConfig,
    surface: &ToolSurface,
    builder: &mut ExplanationBuilder,
) {
    let included = subagent_tool_names(&surface.subagent_tools);
    if !included.is_empty() {
        builder.include_many("subagent", included);
    } else if config.subagent_tools().tools_enabled() {
        builder.unavailable("subagent", "spawn_subagent");
        builder.warn(
            "subagent_targets_unavailable",
            "Subagent spawning is configured, but all targets were filtered out by active-behavior or cross-deployment availability.",
        );
    } else {
        builder.exclude("subagent", "spawn_subagent");
    }
}

fn explain_orchestration(
    config: &BehaviorToolConfig,
    surface: &ToolSurface,
    builder: &mut ExplanationBuilder,
) {
    let included = orchestration_tool_names(&surface.orchestration_tools, &surface.subagent_tools);
    if !included.is_empty() {
        builder.include_many("workflow_orchestration", included);
    } else if config.orchestration_tools().enabled {
        builder.unavailable(
            "workflow_orchestration",
            FAN_OUT_AND_SYNTHESIZE_TOOL_NAME.to_string(),
        );
        builder.warn(
            "workflow_orchestration_unavailable",
            "Workflow orchestration is configured, but subagent spawning, background subagents, or available subagent targets are missing.",
        );
    } else {
        builder.exclude(
            "workflow_orchestration",
            FAN_OUT_AND_SYNTHESIZE_TOOL_NAME.to_string(),
        );
    }
}

fn explain_background(
    config: &BehaviorToolConfig,
    surface: &ToolSurface,
    builder: &mut ExplanationBuilder,
) {
    let included = background_tool_names(&surface.background_tools);
    if !included.is_empty() {
        builder.include_many("background_process", included);
    } else if config.background_tools().tools_enabled() {
        builder.unavailable("background_process", "spawn_process");
    } else {
        builder.exclude("background_process", "spawn_process");
    }
}

fn explain_memory(config: &BehaviorToolConfig, builder: &mut ExplanationBuilder) {
    if !config.memory_requested() {
        builder.exclude("built_in_memory", MEMORY_TOOL_NAME);
        return;
    }

    #[cfg(feature = "agent-memory")]
    {
        builder.include_many(
            "built_in_memory",
            [crate::toolset::MEMORY_TOOL_NAME.to_string()],
        );
    }

    #[cfg(not(feature = "agent-memory"))]
    {
        builder.unavailable("built_in_memory", MEMORY_TOOL_NAME);
        builder.warn(
            "memory_requested_compiled_out",
            "enable_memory is true, but this binary was built without the agent-memory feature.",
        );
    }
}

fn explain_builtin_reads(
    config: &BehaviorToolConfig,
    surface: &ToolSurface,
    builder: &mut ExplanationBuilder,
) {
    if surface.enable_context_budget {
        builder.include_many("built_in_read", [CONTEXT_BUDGET_TOOL_NAME.to_string()]);
    } else if config.context_budget_requested() {
        builder.unavailable("built_in_read", CONTEXT_BUDGET_TOOL_NAME);
    } else {
        builder.exclude("built_in_read", CONTEXT_BUDGET_TOOL_NAME);
    }

    // The `sessions` history tool is opt-in via the ToolSelection
    // `enable_session_history_tool` field (default off), so report it as
    // included only when the operator enabled it; otherwise it is excluded.
    if surface.enable_session_history_tool {
        builder.include_many("built_in_read", [SESSION_HISTORY_TOOL_NAME.to_string()]);
    } else {
        builder.exclude("built_in_read", SESSION_HISTORY_TOOL_NAME);
    }

    if surface.enable_defra_query {
        builder.include_many("built_in_read", [DEFRA_QUERY_TOOL_NAME.to_string()]);
        if surface.defra_query_collections.is_empty() {
            builder.warn(
                "defra_query_empty_scope_all",
                "defra_query_collections is empty, which currently means all collections except hard-blocked sensitive fields.",
            );
        }
    } else if config.defra_query_requested() {
        builder.unavailable("built_in_read", DEFRA_QUERY_TOOL_NAME);
    } else {
        builder.exclude("built_in_read", DEFRA_QUERY_TOOL_NAME);
    }
}
