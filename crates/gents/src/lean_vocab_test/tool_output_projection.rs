use serde::Deserialize;

/// Generated witness for the exact full-output/model-projection authority cut.
#[derive(Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanToolOutputProjectionCase {
    pub(crate) name: String,
    pub(crate) observation: String,
    pub(crate) observed_hash: u64,
    pub(crate) accepted: bool,
    pub(crate) full_output_preserved: bool,
}
