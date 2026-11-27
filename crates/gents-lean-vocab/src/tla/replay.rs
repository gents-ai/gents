use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
pub struct Step {
    pub action: String,
    pub actor: Option<Value>,
    pub before: Value,
    pub after: Value,
}

#[derive(Deserialize)]
pub struct Trace {
    spec: String,
    pub config: String,
    expect: String,
    pub constants: Value,
    pub steps: Vec<Step>,
}

pub enum Red {
    Defect,
    Premise,
    Witness,
}

impl Trace {
    pub fn load(json: &str, spec: &str, constants: &[(&str, u32)]) -> Self {
        let trace: Trace = serde_json::from_str(json).expect("TLC trace");
        assert_eq!(trace.spec, spec);
        for &(name, expected) in constants {
            assert_eq!(
                trace.constants[name], expected,
                "{} constant {name}",
                trace.config
            );
        }
        trace
    }

    pub fn check(&self, red: Red, properties: &[(&str, bool)]) {
        let violated = self
            .expect
            .strip_prefix("violates ")
            .expect("counterexample");
        assert!(
            properties.iter().any(|(name, _)| *name == violated),
            "{} has no observation for {violated}",
            self.config
        );
        for &(property, holds) in properties {
            let expected = property != violated || matches!(red, Red::Defect);
            assert_eq!(holds, expected, "{}: {property}", self.config);
        }
    }
}
