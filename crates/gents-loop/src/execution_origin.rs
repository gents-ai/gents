//! Why a request executed: interactive (a human waiting) or scheduled (a
//! trigger, cron, or background wakeup). Moved out of `gents::lifecycle`
//! (which stays native) because `CompletionRetryPolicy::default_for_origin`
//! needs it and lives in the loop's closure; every other lifecycle concept
//! (claims, generations, the request document itself) stays native.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    pub fn from_persisted(value: Option<&str>) -> anyhow::Result<Self> {
        match value {
            Some("interactive") => Ok(Self::Interactive),
            Some("scheduled") => Ok(Self::Scheduled),
            Some(other) => anyhow::bail!("unknown execution_origin {other:?}"),
            None => anyhow::bail!("execution_origin is required"),
        }
    }
}
