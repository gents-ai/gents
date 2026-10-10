use anyhow::{Context, Result};
use gents::config::ReasoningEffort;
use gents::document_config::{InferenceExecution, InferenceProfile, InferenceSampling};
use serde::Deserialize;
use serde_json::{json, Value};

/// `GENTS_DESKTOP_LIVE_INFERENCE` is a JSON object applied to both live fixture
/// agents, e.g. `{"reasoning_effort":"high","temperature":1,"top_p":0.95,
/// "max_output_tokens":16384,"max_turns":1000}`. Unspecified fields retain
/// fixture defaults; deadlines and tool authority are not configurable here.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct LiveInferenceSettings {
    reasoning_effort: Option<ReasoningEffort>,
    temperature: f64,
    top_p: Option<f64>,
    max_output_tokens: i64,
    max_turns: i64,
}

impl Default for LiveInferenceSettings {
    fn default() -> Self {
        Self {
            reasoning_effort: None,
            temperature: 0.0,
            top_p: None,
            max_output_tokens: 1024,
            max_turns: 20,
        }
    }
}

impl LiveInferenceSettings {
    pub(super) fn from_env() -> Result<Self> {
        match std::env::var("GENTS_DESKTOP_LIVE_INFERENCE") {
            Ok(raw) => Self::parse(Some(&raw)),
            Err(std::env::VarError::NotPresent) => Self::parse(None),
            Err(error) => Err(error).context("reading GENTS_DESKTOP_LIVE_INFERENCE"),
        }
    }

    fn parse(raw: Option<&str>) -> Result<Self> {
        let settings: Self = match raw {
            Some(raw) => {
                let value: Value =
                    serde_json::from_str(raw).context("parsing GENTS_DESKTOP_LIVE_INFERENCE")?;
                anyhow::ensure!(
                    value.is_object(),
                    "GENTS_DESKTOP_LIVE_INFERENCE must be a JSON object"
                );
                serde_json::from_value(value).context("decoding GENTS_DESKTOP_LIVE_INFERENCE")?
            }
            None => Self::default(),
        };
        InferenceSampling {
            temperature: Some(settings.temperature),
            top_p: settings.top_p,
            ..Default::default()
        }
        .validate()
        .context("GENTS_DESKTOP_LIVE_INFERENCE sampling")?;
        InferenceProfile {
            reasoning_effort: settings.reasoning_effort,
            max_output_tokens: Some(settings.max_output_tokens),
            ..Default::default()
        }
        .validate()
        .context("GENTS_DESKTOP_LIVE_INFERENCE profile")?;
        InferenceExecution {
            max_turns: Some(settings.max_turns),
            ..Default::default()
        }
        .validate()
        .context("GENTS_DESKTOP_LIVE_INFERENCE execution")?;
        Ok(settings)
    }

    pub(super) fn apply(&self, config: &mut Value) {
        for profile in config["inference_profiles"]
            .as_array_mut()
            .expect("fixture profiles")
        {
            profile["max_output_tokens"] = json!(self.max_output_tokens);
            if let Some(effort) = self.reasoning_effort {
                profile["reasoning_effort"] = json!(effort);
            }
        }
        let sampling = &mut config["inference_sampling"][0];
        sampling["temperature"] = json!(self.temperature);
        if let Some(top_p) = self.top_p {
            sampling["top_p"] = json!(top_p);
        }
        config["inference_execution"][0]["max_turns"] = json!(self.max_turns);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        json!({
            "inference_profiles": [{"profile_id":"main"}, {"profile_id":"target"}],
            "inference_sampling": [{}],
            "inference_execution": [{"deadline_duration_secs":300,"stream_batch_ms":250,"stream_liveness_timeout_secs":60}]
        })
    }

    #[test]
    fn absent_settings_preserve_fixture_defaults() {
        let mut config = fixture();
        LiveInferenceSettings::parse(None)
            .unwrap()
            .apply(&mut config);
        for profile in config["inference_profiles"].as_array().unwrap() {
            assert_eq!(profile["max_output_tokens"], 1024);
            assert!(profile.get("reasoning_effort").is_none());
        }
        assert_eq!(config["inference_sampling"][0], json!({"temperature":0.0}));
        assert_eq!(config["inference_execution"][0]["max_turns"], 20);
    }

    #[test]
    fn explicit_settings_apply_to_both_profiles_without_changing_deadlines() {
        let mut config = fixture();
        LiveInferenceSettings::parse(Some(r#"{"reasoning_effort":"high","temperature":1,"top_p":0.95,"max_output_tokens":16384,"max_turns":1000}"#))
            .unwrap().apply(&mut config);
        for profile in config["inference_profiles"].as_array().unwrap() {
            assert_eq!(profile["reasoning_effort"], "high");
            assert_eq!(profile["max_output_tokens"], 16384);
        }
        assert_eq!(
            config["inference_sampling"][0],
            json!({"temperature":1.0,"top_p":0.95})
        );
        assert_eq!(
            config["inference_execution"][0],
            json!({"max_turns":1000,"deadline_duration_secs":300,"stream_batch_ms":250,"stream_liveness_timeout_secs":60})
        );
    }

    #[test]
    fn malformed_unknown_and_invalid_explicit_values_are_errors() {
        for raw in [
            "",
            "null",
            "[]",
            "{",
            r#"{"unknown":1}"#,
            r#"{"reasoning_effort":"highest"}"#,
            r#"{"temperature":-1}"#,
            r#"{"temperature":null}"#,
            r#"{"top_p":1.01}"#,
            r#"{"max_output_tokens":0}"#,
            r#"{"max_turns":0}"#,
            r#"{"max_turns":1.5}"#,
        ] {
            assert!(LiveInferenceSettings::parse(Some(raw)).is_err(), "{raw}");
        }
    }
}
