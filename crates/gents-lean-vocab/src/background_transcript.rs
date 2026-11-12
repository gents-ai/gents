use super::*;

impl LeanR4cBackgroundWorkCase {
    pub fn witness(&self) -> &'static str {
        match self {
            Self::ReadToolOutputCanonicalSourceReconstruction { .. } => {
                "r4c.read_tool_output.canonical_source_reconstruction"
            }
        }
    }
}

impl LeanBackgroundTheoremWitness {
    pub fn kind_field(&self, key: &str) -> &str {
        self.kind_fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| field.value.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "Lean Background theorem witness {:?} omitted kind field {:?}",
                    self.theorem_name, key
                )
            })
    }
}
