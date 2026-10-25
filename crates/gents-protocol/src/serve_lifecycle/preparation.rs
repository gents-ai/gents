use serde::{Deserialize, Serialize};

pub const RUNTIME_PREPARATION_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum RuntimePreparationOutcome {
    Started,
    Replaced,
    Reused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum RuntimePreparationRefusal {
    NotInitialized,
    InvalidHome,
    Unsupported,
    Unavailable,
    Busy,
    Starting,
    StartupBusy,
    StoreBusy,
    Credentials,
    Failed,
}

/// A private helper-process receipt, not remote identity evidence. Reuse is
/// reported only after verifying the live node's signed control receipt;
/// started/replaced is emitted by the new server after publishing readiness.
/// Losing this output channel does not revoke native work or stop the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(tag = "kind", rename_all = "camelCase"))]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum RuntimePreparationResult {
    Ready {
        outcome: RuntimePreparationOutcome,
        home: String,
        node_did: String,
        graphql: String,
        build: String,
    },
    Refused {
        reason: RuntimePreparationRefusal,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct RuntimePreparationReport {
    pub version: u8,
    pub result: RuntimePreparationResult,
}
