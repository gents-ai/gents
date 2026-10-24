use super::error::DocumentConfigError;
use super::ProjectionAcpBinding;
use std::collections::BTreeMap;

const PROJECTION_ACP_RUNTIME_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentMessage",
    "AgentToolCall",
    "Goal",
    "AgentOutputSegment",
    "InferenceCall",
    "CompactionEntry",
    "AgentSession",
    "RenderedRequest",
];

impl ProjectionAcpBinding {
    pub fn validate(&self) -> Result<(), DocumentConfigError> {
        if self.policy_id.trim().is_empty() {
            return Err(DocumentConfigError::ProjectionAcpPolicyBlank {
                binding_id: self.binding_id.clone(),
            });
        }
        let optional = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let active = self.policy_id.trim();
        let staged = optional(&self.staged_policy_id);
        let previous = optional(&self.previous_policy_id);
        if staged.as_deref() == Some(active) {
            return Err(DocumentConfigError::ProjectionAcpStagedIsActive {
                binding_id: self.binding_id.clone(),
            });
        }
        if previous.as_deref() == Some(active) {
            return Err(DocumentConfigError::ProjectionAcpPreviousIsActive {
                binding_id: self.binding_id.clone(),
            });
        }
        if staged.is_some() && staged == previous {
            return Err(DocumentConfigError::ProjectionAcpStagedIsPrevious {
                binding_id: self.binding_id.clone(),
            });
        }
        if let Some(projection) = optional(&self.projection_id) {
            serde_json::from_value::<crate::adapter_projection::AdapterProjectionKind>(
                projection.into(),
            )
            .map_err(
                |source| DocumentConfigError::ProjectionAcpInvalidProjection {
                    binding_id: self.binding_id.clone(),
                    source,
                },
            )?;
        }
        parse_projection_resource_map(self.resource_map_json.as_deref())?;
        Ok(())
    }
}

pub fn parse_projection_resource_map(
    resource_map_json: Option<&str>,
) -> Result<BTreeMap<String, String>, DocumentConfigError> {
    let Some(raw) = resource_map_json
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(BTreeMap::new());
    };
    let raw_map = serde_json::from_str::<BTreeMap<String, String>>(raw)
        .map_err(|source| DocumentConfigError::ProjectionResourceMapParse { source })?;
    let mut map = BTreeMap::new();
    for (collection, resource_name) in raw_map {
        let collection = collection.trim();
        let resource_name = resource_name.trim();
        if collection.is_empty() || resource_name.is_empty() {
            return Err(DocumentConfigError::ProjectionResourceMapBlankEntry);
        }
        if !PROJECTION_ACP_RUNTIME_COLLECTIONS.contains(&collection) {
            return Err(
                DocumentConfigError::ProjectionResourceMapUnknownCollection {
                    collection: collection.to_owned(),
                    expected: PROJECTION_ACP_RUNTIME_COLLECTIONS.join(", "),
                },
            );
        }
        map.insert(collection.to_string(), resource_name.to_string());
    }
    Ok(map)
}
