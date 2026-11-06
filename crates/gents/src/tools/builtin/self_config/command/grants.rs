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
    if categories.contains("policy") {
        resources.push("policy");
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

/// The agent catalog grant: manages Agents on the node and reaches foreign configuration.
const CATALOG_CATEGORY: &str = "node";
/// The self-config category of an Agent document and its Contexts; foreign edits of it need only the catalog.
const AGENT_CATEGORY: &str = "agent";

/// Whose configuration an edit touches (`SelfConfig.Scope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfigScope {
    Own,
    Foreign,
}

/// `SelfConfig.scopeOf`: a shared document is foreign even when the invoking Agent selects it.
pub(super) fn config_scope(invoking_agent: &str, target_agent: &str, shared: bool) -> ConfigScope {
    if target_agent == invoking_agent && !shared {
        ConfigScope::Own
    } else {
        ConfigScope::Foreign
    }
}

/// `SelfConfig.requiredCategories`.
fn required_categories(category: &str, scope: ConfigScope) -> Vec<&str> {
    match scope {
        ConfigScope::Own => vec![category],
        ConfigScope::Foreign if category == AGENT_CATEGORY => vec![CATALOG_CATEGORY],
        ConfigScope::Foreign => vec![CATALOG_CATEGORY, category],
    }
}

/// `SelfConfig.mayEdit`.
pub(super) fn may_edit(categories: &BTreeSet<String>, category: &str, scope: ConfigScope) -> bool {
    required_categories(category, scope)
        .iter()
        .all(|required| categories.contains(*required))
}

/// `SelfConfig.mayCreateAgent`.
pub(super) fn may_create_agent(categories: &BTreeSet<String>) -> bool {
    categories.contains(CATALOG_CATEGORY)
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

    /// Gates configuration access to `agent_did` (already resolved). An own
    /// document needs its category; a foreign one needs the catalog, plus the
    /// category unless the document is an Agent. `category` None is an
    /// effective read, which needs only the catalog for a foreign Agent. A
    /// Context or Tools selected by several Agents is gated separately by
    /// `ops::guard_shared_authority_in_txn`, which also requires the catalog.
    pub(super) fn ensure_config_scope(
        &self,
        category: Option<&str>,
        agent_did: &str,
    ) -> Result<()> {
        let scope = config_scope(self.core.agent_did(), agent_did, false);
        if scope == ConfigScope::Foreign {
            anyhow::ensure!(
                self.categories.contains(CATALOG_CATEGORY),
                "configuration of another agent requires the agent catalog grant (category node)"
            );
        }
        let Some(category) = category else {
            return Ok(());
        };
        anyhow::ensure!(
            may_edit(&self.categories, category, scope),
            "{category} configuration is not granted; enabled resources: {}",
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

    /// Create and clone add an Agent to the node (catalog). Disable is an
    /// Agent edit of its target, so it follows that Agent's scope.
    pub(super) fn ensure_agent_mutation(
        &self,
        operation: &str,
        agent_did: Option<&str>,
    ) -> Result<()> {
        match (operation, agent_did) {
            ("create" | "clone", _) => self.ensure_node_authority(operation),
            (_, Some(agent_did)) => self.ensure_config_scope(Some(AGENT_CATEGORY), agent_did),
            (_, None) => Ok(()),
        }
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
            !params.make_default || may_create_agent(&self.categories),
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

#[cfg(test)]
mod scope_tests {
    use super::*;
    use crate::lean_vocab_test::LeanSelfConfigScopeCases;

    fn categories(names: &[String]) -> BTreeSet<String> {
        names.iter().cloned().collect()
    }

    #[test]
    fn modeled_edit_scope_cases_drive_production_gate() {
        let LeanSelfConfigScopeCases { edit, .. } =
            &crate::lean_vocab_test::lean_contract_snapshot().self_config_scope_cases;
        assert!(!edit.is_empty());
        for case in edit {
            let scope = match case.scope.as_str() {
                "own" => ConfigScope::Own,
                "foreign" => ConfigScope::Foreign,
                other => panic!("unknown scope {other}"),
            };
            assert_eq!(
                may_edit(&categories(&case.categories), &case.category, scope),
                case.allowed,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn engineer_preset_is_the_modeled_engineer_group() {
        let LeanSelfConfigScopeCases { groups, .. } =
            &crate::lean_vocab_test::lean_contract_snapshot().self_config_scope_cases;
        let modeled = groups
            .iter()
            .find(|case| case.name == "engineer")
            .expect("engineer row");
        let preset = crate::tools::builtin::self_config::engineer_self_config();
        assert_eq!(preset.enable_self_config, Some(true));
        let mut granted = preset.self_config_categories.unwrap_or_default();
        granted.sort();
        let mut effective = modeled.effective.clone();
        effective.sort();
        assert_eq!(granted, effective);
        assert!(may_edit(&categories(&granted), "tools", ConfigScope::Own));
        assert!(may_create_agent(&categories(&granted)));
    }

    #[test]
    fn modeled_effective_groups_and_scope_of_drive_production_helpers() {
        let LeanSelfConfigScopeCases {
            groups, scope_of, ..
        } = &crate::lean_vocab_test::lean_contract_snapshot().self_config_scope_cases;
        for case in groups {
            assert_eq!(
                may_create_agent(&categories(&case.effective)),
                case.may_create_agent,
                "{}",
                case.name
            );
        }
        for case in scope_of {
            let expected = match case.scope.as_str() {
                "own" => ConfigScope::Own,
                _ => ConfigScope::Foreign,
            };
            assert_eq!(
                config_scope(&case.invoking_agent, &case.target_agent, case.shared),
                expected,
                "{}",
                case.name
            );
        }
    }
}
