use super::*;

pub(super) fn model_resources(categories: &BTreeSet<String>, pack: bool) -> Vec<&'static str> {
    let mut resources = Vec::new();
    if agent_command_exposed(categories) {
        resources.push("agent");
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

pub(super) fn agent_command_exposed(categories: &BTreeSet<String>) -> bool {
    categories.contains("node") || categories.contains("agent")
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
        _agent_did: Option<&str>,
    ) -> Result<()> {
        anyhow::ensure!(
            agent_command_exposed(&self.categories),
            "agent {operation} is not exposed"
        );
        Ok(())
    }

    pub(super) fn ensure_agent_operation(
        &self,
        operation: &str,
        _agent_did: Option<&str>,
    ) -> Result<()> {
        anyhow::ensure!(
            agent_command_exposed(&self.categories),
            "agent {operation} is not exposed"
        );
        Ok(())
    }

    pub(super) fn ensure_agent_stays_enabled(
        &self,
        operation: &str,
        agent_did: Option<&str>,
    ) -> Result<()> {
        anyhow::ensure!(
            !(self.no_lockout
                && operation == "disable"
                && agent_did == Some(self.core.agent_did())),
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

#[cfg(test)]
mod creation_exposure_tests {
    use super::*;

    #[test]
    fn modeled_agent_creation_exposure_controls_the_command() {
        for case in
            &crate::lean_vocab_test::lean_contract_snapshot().identity_agent_creation_exposure_cases
        {
            let mut categories = BTreeSet::new();
            if case["node_category"].as_bool().unwrap() {
                categories.insert("node".to_owned());
            }
            if case["agent_category"].as_bool().unwrap() {
                categories.insert("agent".to_owned());
            }
            for operation in ["create", "clone", "edit", "disable"] {
                assert_eq!(
                    agent_command_exposed(&categories),
                    case["exposed"].as_bool().unwrap(),
                    "{} {operation}",
                    case["name"]
                );
            }
        }
    }
}
