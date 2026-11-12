use super::*;

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
