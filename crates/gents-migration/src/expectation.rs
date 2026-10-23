use defra_node::CollectionVersion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionExpectation {
    pub required_field_names: Option<&'static [&'static str]>,
}

impl CollectionExpectation {
    pub const fn dag_only() -> Self {
        Self {
            required_field_names: None,
        }
    }

    pub const fn fields(required_field_names: &'static [&'static str]) -> Self {
        Self {
            required_field_names: Some(required_field_names),
        }
    }

    pub fn verify(&self, version: &CollectionVersion) -> Result<(), String> {
        if let Some(required) = self.required_field_names {
            let present: rapidhash::RapidHashSet<&str> =
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
