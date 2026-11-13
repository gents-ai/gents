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
        ("tools", "page"),
        ("tools", "plugin-source"),
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
        if !resources.contains(&"page") {
            resources.push("page");
        }
        resources.push("ux");
    }
    if categories.contains("policy") {
        resources.push("policy");
    }
    if pack || categories.contains("tools") {
        resources.push("plugin");
        resources.push("ux");
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
#[cfg(test)]
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
}

#[cfg(test)]
mod creation_exposure_tests {
    use super::*;

    #[test]
    fn modeled_agent_creation_exposure_controls_the_command() {
        gents_lean_vocab::families::node_provisioning_agent_creation_exposure::replay(|i| {
            let mut categories = BTreeSet::new();
            if i.node_category {
                categories.insert("node".to_owned());
            }
            if i.agent_category {
                categories.insert("agent".to_owned());
            }
            agent_command_exposed(&categories)
        });
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    use crate::surface::{ResolvedToolSelection, ToolPolicySurface};
    use gents_lean_vocab::families::contracts_self_config_scope::{
        GroupOutput, PresetOutput, ScopeOutput,
    };

    fn categories(names: &[String]) -> BTreeSet<String> {
        names.iter().cloned().collect()
    }

    fn sorted(names: &[&str]) -> Vec<String> {
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn effective(enabled: bool, categories: Option<Vec<String>>) -> BTreeSet<String> {
        ToolPolicySurface::from_selection(
            &ResolvedToolSelection {
                enable_self_config: enabled,
                self_config_categories: categories,
                ..Default::default()
            },
            &Default::default(),
        )
        .self_config_category_set()
    }

    fn preset() -> PresetOutput {
        let preset = crate::builtin::self_config::engineer_self_config();
        let enabled = preset.enable_self_config == Some(true);
        let granted = effective(enabled, preset.self_config_categories);
        PresetOutput {
            enabled,
            edits_own_tools: may_edit(&granted, "tools", ConfigScope::Own),
            creates_agents: may_create_agent(&granted),
            effective: granted.into_iter().collect(),
        }
    }

    #[test]
    fn contracts_self_config_scope_matches_lean() {
        gents_lean_vocab::families::contracts_self_config_scope::replay(|i| ScopeOutput {
            categories: sorted(&gents_documents::patch::SELF_CONFIG_CATEGORIES),
            default_categories: sorted(&gents_documents::patch::DEFAULT_SELF_CONFIG_CATEGORIES),
            allowed: i
                .edit
                .iter()
                .map(|case| {
                    let scope = match case.scope.as_str() {
                        "own" => ConfigScope::Own,
                        "foreign" => ConfigScope::Foreign,
                        other => panic!("unknown scope {other}"),
                    };
                    may_edit(&categories(&case.categories), &case.category, scope)
                })
                .collect(),
            groups: i
                .groups
                .iter()
                .map(|group| {
                    let granted = effective(group.enabled, group.categories.clone());
                    GroupOutput {
                        may_create_agent: may_create_agent(&granted),
                        effective: granted.into_iter().collect(),
                    }
                })
                .collect(),
            scope: i
                .scope_of
                .iter()
                .map(|case| {
                    match config_scope(&case.invoking_agent, &case.target_agent, case.shared) {
                        ConfigScope::Own => "own".to_owned(),
                        ConfigScope::Foreign => "foreign".to_owned(),
                    }
                })
                .collect(),
            engineer: preset(),
            yolo: preset(),
        });
    }
}
