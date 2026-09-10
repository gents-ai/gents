use std::collections::BTreeMap;
use anyhow::{Context, Result};
use super::ProjectionAcpBinding;

const PROJECTION_ACP_RUNTIME_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentMessage",
    "AgentToolCall",
    "Goal",
    "AgentResponse",
    "InferenceCall",
    "CompactionEntry",
    "AgentSession",
    "RenderedRequest",
];

impl ProjectionAcpBinding {
    /// Validate authored policy references. Publication status remains an
    /// observation checked by the existing projection enforcement owner.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(!self.policy_id.trim().is_empty(), "projection ACP binding {} must contain a non-empty policy_id", self.binding_id);
        let optional = |value: &Option<String>| value.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned);
        let active = self.policy_id.trim();
        let staged = optional(&self.staged_policy_id);
        let previous = optional(&self.previous_policy_id);
        anyhow::ensure!(staged.as_deref() != Some(active), "projection ACP binding {} staged_policy_id must differ from active policy_id", self.binding_id);
        anyhow::ensure!(previous.as_deref() != Some(active), "projection ACP binding {} previous_policy_id must differ from active policy_id", self.binding_id);
        anyhow::ensure!(staged.is_none() || staged != previous, "projection ACP binding {} staged_policy_id must differ from previous_policy_id", self.binding_id);
        if let Some(projection) = optional(&self.projection_id) {
            serde_json::from_value::<crate::adapter_projection::AdapterProjectionKind>(projection.into())
                .with_context(|| format!("projection ACP binding {} has invalid projection_id", self.binding_id))?;
        }
        parse_projection_resource_map(self.resource_map_json.as_deref())?;
        Ok(())
    }
}

pub fn parse_projection_resource_map(
    resource_map_json: Option<&str>,
) -> Result<BTreeMap<String, String>> {
    let Some(raw) = resource_map_json
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(BTreeMap::new());
    };
    let raw_map = serde_json::from_str::<BTreeMap<String, String>>(raw)
        .context("parsing ProjectionAcpBinding.resource_map_json")?;
    let mut map = BTreeMap::new();
    for (collection, resource_name) in raw_map {
        let collection = collection.trim();
        let resource_name = resource_name.trim();
        if collection.is_empty() || resource_name.is_empty() {
            anyhow::bail!(
                "ProjectionAcpBinding.resource_map_json must map non-empty collection names to non-empty ACP resource names"
            );
        }
        if !PROJECTION_ACP_RUNTIME_COLLECTIONS.contains(&collection) {
            anyhow::bail!(
                "ProjectionAcpBinding.resource_map_json contains unknown runtime collection {collection}; expected one of {}",
                PROJECTION_ACP_RUNTIME_COLLECTIONS.join(", ")
            );
        }
        map.insert(collection.to_string(), resource_name.to_string());
    }
    Ok(map)
}


#[cfg(test)]
mod tests;
