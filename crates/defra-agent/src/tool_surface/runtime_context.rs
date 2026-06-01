use std::sync::Arc;

use defra_node::EmbeddedNode;

use crate::health_checker::ServiceHealthMap;
use crate::mcp_pool::McpPool;

#[derive(Clone)]
pub struct ToolRuntimeContext {
    pub(super) node: Arc<EmbeddedNode>,
    pub(super) mcp_pool: McpPool,
    pub(super) health_map: ServiceHealthMap,
    pub(super) local_hostname: String,
    pub(super) local_subnet: Option<String>,
    pub(super) agent_did: String,
}

impl ToolRuntimeContext {
    pub fn new(
        node: Arc<EmbeddedNode>,
        mcp_pool: McpPool,
        health_map: ServiceHealthMap,
        local_hostname: impl Into<String>,
        local_subnet: Option<String>,
    ) -> Self {
        Self::new_with_agent_did(node, mcp_pool, health_map, local_hostname, local_subnet, "")
    }

    pub fn new_with_agent_did(
        node: Arc<EmbeddedNode>,
        mcp_pool: McpPool,
        health_map: ServiceHealthMap,
        local_hostname: impl Into<String>,
        local_subnet: Option<String>,
        agent_did: impl Into<String>,
    ) -> Self {
        Self {
            node,
            mcp_pool,
            health_map,
            local_hostname: local_hostname.into(),
            local_subnet,
            agent_did: agent_did.into(),
        }
    }

    pub fn oneshot(node: Arc<EmbeddedNode>) -> Self {
        Self::oneshot_with_agent_did(node, "")
    }

    pub fn oneshot_with_agent_did(node: Arc<EmbeddedNode>, agent_did: impl Into<String>) -> Self {
        Self {
            node,
            mcp_pool: McpPool::default(),
            health_map: ServiceHealthMap::default(),
            local_hostname: "localhost".to_string(),
            local_subnet: None,
            agent_did: agent_did.into(),
        }
    }

    pub fn node(&self) -> &Arc<EmbeddedNode> {
        &self.node
    }

    pub fn local_hostname(&self) -> &str {
        &self.local_hostname
    }

    pub fn local_subnet(&self) -> Option<&str> {
        self.local_subnet.as_deref()
    }
}
