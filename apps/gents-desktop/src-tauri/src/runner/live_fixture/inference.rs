use gents::config::ReasoningEffort;
use gents::document_config::{
    DocumentConfigError, InferenceExecution, InferenceProfile, InferenceSampling,
};
use serde::Deserialize;
use serde_json::{json, Value};

type Result<T, E = InferenceError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub(super) enum InferenceError {
    #[error("reading GENTS_DESKTOP_LIVE_INFERENCE")]
    ReadEnv(#[source] std::env::VarError),
    #[error("parsing GENTS_DESKTOP_LIVE_INFERENCE")]
    Parse(#[source] serde_json::Error),
    #[error("GENTS_DESKTOP_LIVE_INFERENCE must be a JSON object")]
    NotObject,
    #[error("decoding GENTS_DESKTOP_LIVE_INFERENCE")]
    Decode(#[source] serde_json::Error),
    #[error("GENTS_DESKTOP_LIVE_INFERENCE sampling")]
    Sampling(#[source] Box<DocumentConfigError>),
    #[error("GENTS_DESKTOP_LIVE_INFERENCE profile")]
    Profile(#[source] Box<DocumentConfigError>),
    #[error("GENTS_DESKTOP_LIVE_INFERENCE execution")]
    Execution(#[source] Box<DocumentConfigError>),
}

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
            Err(error) => Err(InferenceError::ReadEnv(error)),
        }
    }

    fn parse(raw: Option<&str>) -> Result<Self> {
        let settings: Self = match raw {
            Some(raw) => {
                let value: Value = serde_json::from_str(raw).map_err(InferenceError::Parse)?;
                if !value.is_object() {
                    return Err(InferenceError::NotObject);
                }
                serde_json::from_value(value).map_err(InferenceError::Decode)?
            }
            None => Self::default(),
        };
        InferenceSampling {
            temperature: Some(settings.temperature),
            top_p: settings.top_p,
            ..Default::default()
        }
        .validate()
        .map_err(|error| InferenceError::Sampling(Box::new(error)))?;
        InferenceProfile {
            reasoning_effort: settings.reasoning_effort,
            max_output_tokens: Some(settings.max_output_tokens),
            ..Default::default()
        }
        .validate()
        .map_err(|error| InferenceError::Profile(Box::new(error)))?;
        InferenceExecution {
            max_turns: Some(settings.max_turns),
            ..Default::default()
        }
        .validate()
        .map_err(|error| InferenceError::Execution(Box::new(error)))?;
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
