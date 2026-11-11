use anyhow::Result;

use super::AgentTargetDocument;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentToolConfig {
    pub targets: Vec<AgentTargetDocument>,
    pub enabled: bool,
}

impl AgentToolConfig {
    pub fn from_document(tools: &super::Tools) -> Result<Self> {
        tools.validate()?;
        Ok(Self {
            targets: Vec::new(),
            enabled: tools
                .agents
                .as_ref()
                .and_then(|group| group.enabled)
                .unwrap_or(false),
        })
    }

    pub fn from_document_with_targets<'a>(
        tools: &super::Tools,
        targets: impl IntoIterator<Item = &'a AgentTargetDocument>,
    ) -> Result<Self> {
        let mut resolved = Self::from_document(tools)?;
        let mut by_id = rapidhash::RapidHashMap::default();
        for target in targets {
            anyhow::ensure!(
                by_id
                    .insert(
                        (target.node_did.as_str(), target.target_id.as_str()),
                        target
                    )
                    .is_none(),
                "duplicate scoped AgentTarget {}",
                target.target_id
            );
        }
        let mut names = rapidhash::RapidHashSet::default();
        for id in tools
            .agents
            .as_ref()
            .into_iter()
            .flat_map(|group| &group.target_ids)
        {
            let target = by_id
                .get(&(tools.node_did.as_str(), id.as_str()))
                .ok_or_else(|| anyhow::anyhow!("missing same-owner AgentTarget {id}"))?;
            anyhow::ensure!(
                !target.name.trim().is_empty()
                    && !target.target_node_did.trim().is_empty()
                    && !target.agent_did.trim().is_empty(),
                "invalid AgentTarget {id}"
            );
            anyhow::ensure!(
                names.insert(target.name.as_str()),
                "duplicate agent target name {}",
                target.name
            );
            resolved.targets.push((*target).clone());
        }
        Ok(resolved)
    }

    /// The agents group follows `AgentTools.enabled` alone (Lean
    /// `ToolPolicy.Surface.sessionMessages`): `agent_message`, `agent_list`, `agent_session_list`, `agent_session_read` and
    /// `agent_interrupt` address sessions on this node and need no target.
    pub fn tools_enabled(&self) -> bool {
        self.enabled
    }

    pub fn agent_new_enabled(&self) -> bool {
        self.enabled && !self.targets.is_empty()
    }

    pub fn target(&self, name: &str) -> Option<&AgentTargetDocument> {
        self.targets.iter().find(|target| target.name == name)
    }

    pub(crate) fn target_names(&self) -> Vec<String> {
        self.targets
            .iter()
            .map(|target| target.name.clone())
            .collect()
    }

    pub fn allows(&self, target_node_did: &str, agent_did: &str) -> bool {
        self.targets.iter().any(|target| {
            target.target_node_did == target_node_did && target.agent_did == agent_did
        })
    }
}
