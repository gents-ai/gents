#[derive(Debug, thiserror::Error)]
pub enum ExecutionOriginError {
    #[error("unknown execution_origin {origin:?}")]
    Unknown { origin: String },
    #[error("execution_origin is required")]
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOrigin {
    Interactive,
    Scheduled,
}

impl ExecutionOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Scheduled => "scheduled",
        }
    }

    pub fn from_persisted(value: Option<&str>) -> Result<Self, ExecutionOriginError> {
        match value {
            Some("interactive") => Ok(Self::Interactive),
            Some("scheduled") => Ok(Self::Scheduled),
            Some(other) => Err(ExecutionOriginError::Unknown {
                origin: other.to_owned(),
            }),
            None => Err(ExecutionOriginError::Required),
        }
    }
}

#[cfg(test)]
mod conformance {
    use super::ExecutionOrigin;

    #[test]
    fn execution_origins_are_the_lean_execution_origins() {
        gents_lean_vocab::families::execution_origin_vocabulary::replay(&[
            ExecutionOrigin::Interactive,
            ExecutionOrigin::Scheduled,
        ]);
    }
}
