use serde_json::Value;

/// One installed plugin as the native document projector sees it. The host
/// builds these from its plugin store while it holds that store's lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedPlugin {
    /// `namespace/name`.
    pub coordinate: String,
    pub digest: String,
    /// Whether the plugin declares the `delete` data operation.
    pub deletes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectedOperationSource {
    pub purpose: &'static str,
    pub permission: acp::NodePermission,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedPluginRef {
    /// `namespace/name`.
    pub plugin: String,
    pub digest: Option<String>,
}

/// The host tool policy the native projector applies to a raw Tools document.
/// Store supplies no default: every node start names its policy, so a missing
/// ceiling cannot widen the projected permissions.
pub trait ToolSourcePolicy: Send + Sync {
    /// Native operations the configured tools require. An error withholds
    /// every operation and plugin source for this Tools selection.
    fn node_operation_sources(
        &self,
        tools: &Value,
        surfaces: &[Value],
    ) -> anyhow::Result<Vec<ProjectedOperationSource>>;

    /// Installed plugin tools the configured tools permit.
    fn plugin_refs(&self, tools: &Value) -> anyhow::Result<Vec<ProjectedPluginRef>>;
}
