use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Optional sampling overrides shared by inference profiles. Unsupported explicit
/// settings must be rejected for the selected provider/model rather than ignored.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceSampling {
    pub agent_did: String,
    pub sampling_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub top_k: Option<i64>,
    /// Requested provider seed; does not guarantee deterministic provider output.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub min_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub frequency_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub repetition_penalty: Option<f64>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

impl InferenceSampling {
    /// Validate the sampling bounds owned by this document and report every
    /// violated rule at once.
    pub fn validation_violations(&self) -> Vec<String> {
        let sampling_id = self.sampling_id.trim();
        let mut violations: Vec<String> = Vec::new();

        if self
            .temperature
            .is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            violations.push(format!(
                "InferenceSampling {sampling_id} temperature must be finite and non-negative"
            ));
        }
        if self.seed.is_some_and(|value| value < 0) {
            violations.push(format!(
                "InferenceSampling {sampling_id} seed must be non-negative"
            ));
        }
        if self
            .top_p
            .is_some_and(|value| !(0.0..=1.0).contains(&value))
        {
            violations.push(format!(
                "InferenceSampling {sampling_id} top_p must be within [0, 1]"
            ));
        }
        if self
            .min_p
            .is_some_and(|value| !(0.0..=1.0).contains(&value))
        {
            violations.push(format!(
                "InferenceSampling {sampling_id} min_p must be within [0, 1]"
            ));
        }
        if self.top_k.is_some_and(|value| value <= 0) {
            violations.push(format!(
                "InferenceSampling {sampling_id} top_k must be positive"
            ));
        }
        if self
            .repetition_penalty
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            violations.push(format!(
                "InferenceSampling {sampling_id} repetition_penalty must be positive"
            ));
        }
        for (name, value) in [
            ("frequency_penalty", self.frequency_penalty),
            ("presence_penalty", self.presence_penalty),
        ] {
            if value.is_some_and(|value| !(-2.0..=2.0).contains(&value)) {
                violations.push(format!(
                    "InferenceSampling {sampling_id} {name} must be within [-2, 2]"
                ));
            }
        }

        violations
    }

    pub fn validate(&self) -> Result<()> {
        let violations = self.validation_violations();
        if violations.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(violations.join("; "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonfinite_sampling_cannot_become_silent_null_provider_defaults() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let sampling = InferenceSampling {
                temperature: Some(value),
                top_p: Some(value),
                min_p: Some(value),
                frequency_penalty: Some(value),
                presence_penalty: Some(value),
                repetition_penalty: Some(value),
                ..Default::default()
            };
            assert_eq!(sampling.validation_violations().len(), 6, "{value}");
        }
    }

    #[test]
    fn finite_sampling_boundaries_and_zero_seed_remain_valid() {
        let mut sampling = InferenceSampling {
            temperature: Some(0.0),
            top_p: Some(0.0),
            min_p: Some(1.0),
            top_k: Some(1),
            seed: Some(0),
            frequency_penalty: Some(-2.0),
            presence_penalty: Some(2.0),
            repetition_penalty: Some(f64::MIN_POSITIVE),
            ..Default::default()
        };
        assert!(sampling.validate().is_ok());
        sampling.temperature = Some(-0.01);
        sampling.top_p = Some(1.01);
        sampling.min_p = Some(-0.01);
        sampling.top_k = Some(0);
        sampling.seed = Some(-1);
        sampling.frequency_penalty = Some(-2.01);
        sampling.presence_penalty = Some(2.01);
        sampling.repetition_penalty = Some(0.0);
        assert_eq!(sampling.validation_violations().len(), 8);
    }
}
