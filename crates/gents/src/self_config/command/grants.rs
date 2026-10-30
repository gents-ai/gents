use super::*;

pub(super) fn model_resources(categories: &BTreeSet<String>, pack: bool) -> Vec<&'static str> {
    let mut resources = Vec::new();
    if categories.contains("node") {
        resources.push("agent");
    } else if categories.contains("agent") {
        resources.push("agent (current only)");
    }
    for (category, resource) in [
        ("node", "context"),
        ("tools", "tools"),
        ("tools", "datastore"),
        ("tools", "skill"),
        ("tools", "agent-target"),
        ("profile", "profile"),
        ("profile", "execution"),
        ("profile", "sampling"),
        ("profile", "retry-policy"),
        ("profile", "compaction"),
        ("backend", "backend"),
        ("mcp_service", "mcp-service"),
        ("automation", "automation"),
        ("automation", "task"),
        ("automation", "trigger"),
        ("automation", "schedule"),
        ("automation", "event-source"),
    ] {
        if categories.contains(category) {
            resources.push(resource);
        }
    }
    if pack {
        resources.push("pack");
    }
    if pack || categories.contains("tools") {
        resources.push("plugin");
    }
    if !categories.is_empty() {
        resources.push("cleanup");
        resources.push("batch");
    }
    if categories.contains("tools") {
        resources.push("discovery");
    }
    if categories.contains("node") {
        resources.push("validate");
    }
    resources
}

impl ConfigCommandTool {
    pub(super) fn ensure_resource(&self, resource: &str) -> Result<()> {
        let category = match resource {
            "mcp-service" => "mcp_service",
            other => other,
        };
        anyhow::ensure!(
            self.categories.contains(category),
            "{resource} configuration is not granted; enabled resources: {}",
            model_resources(&self.categories, self.core.held_grants().pack_install).join(", ")
        );
        Ok(())
    }

    pub(super) fn ensure_agent_catalog(
        &self,
        operation: &str,
        agent_id: Option<&str>,
    ) -> Result<()> {
        let current_only = agent_id.is_some_and(|id| id == self.core.agent_id());
        anyhow::ensure!(
            self.categories.contains("node") || current_only,
            "agent {operation} outside the current agent is not granted; the agent catalog grant (category node) is required"
        );
        Ok(())
    }

    pub(super) fn ensure_agent_operation(
        &self,
        operation: &str,
        agent_id: Option<&str>,
    ) -> Result<()> {
        let current_edit =
            operation == "edit" && agent_id.is_some_and(|id| id == self.core.agent_id());
        anyhow::ensure!(
            self.categories.contains("node") || current_edit,
            "agent {operation} is not granted; only editing the current agent is allowed without the agent catalog grant (category node)"
        );
        Ok(())
    }

    pub(super) fn ensure_agent_stays_enabled(
        &self,
        operation: &str,
        agent_id: Option<&str>,
    ) -> Result<()> {
        anyhow::ensure!(
            !(self.no_lockout && operation == "disable" && agent_id == Some(self.core.agent_id())),
            "no-lockout guard: agent must remain enabled"
        );
        Ok(())
    }

    pub(super) fn ensure_node_authority(&self, operation: &str) -> Result<()> {
        anyhow::ensure!(
            self.categories.contains("node"),
            "agent {operation} requires the agent catalog grant (category node)"
        );
        Ok(())
    }

    pub(super) fn ensure_default_selection(&self, params: &ConfigureAgentParams) -> Result<()> {
        anyhow::ensure!(
            !params.make_default || self.categories.contains("node"),
            "--default changes the node's agent selection and requires the agent catalog grant (category node)"
        );
        Ok(())
    }
}
