//! Collection state expectations.
//!
//! Contract: a baseline entry or step may require field names that must be
//! present on the active version; the version DAG shape itself is verified
//! against the registry pins by the engine.

use defra_node::CollectionVersion;

/// Expected post-state for a baseline entry or step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionExpectation {
    /// Field names that must be present on the active version.
    /// `None` means "do not check field names" (pin / single-version only).
    pub required_field_names: Option<&'static [&'static str]>,
}

impl CollectionExpectation {
    /// Expectation that only checks the version DAG shape (no field list).
    pub const fn dag_only() -> Self {
        Self {
            required_field_names: None,
        }
    }

    /// Expectation that requires the given field names on the active version.
    pub const fn fields(required_field_names: &'static [&'static str]) -> Self {
        Self {
            required_field_names: Some(required_field_names),
        }
    }

    /// Verify `version` against this expectation. Returns `Ok(())` or a
    /// human-readable detail string.
    pub fn verify(&self, version: &CollectionVersion) -> Result<(), String> {
        if let Some(required) = self.required_field_names {
            let present: std::collections::HashSet<&str> =
                version.fields.iter().map(|f| f.name.as_str()).collect();
            let mut missing = Vec::new();
            for name in required {
                if !present.contains(name) {
                    missing.push(*name);
                }
            }
            if !missing.is_empty() {
                return Err(format!("missing required fields: {missing:?}"));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_required_field_is_reported() {
        let exp = CollectionExpectation::fields(&["lifecycle_state"]);
        // Empty version has no fields — verification must fail.
        let v = CollectionVersion {
            name: "AgentToolCall".into(),
            version_id: "v1".into(),
            collection_id: "c1".into(),
            ..CollectionVersion::new("", "", "", vec![])
        };
        let err = exp.verify(&v).unwrap_err();
        assert!(err.contains("lifecycle_state"), "{err}");
    }
}
