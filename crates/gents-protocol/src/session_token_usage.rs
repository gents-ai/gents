/// Tokens charged against a session's budget. `input` and `output` are the
/// durable `InferenceCall` prompt and completion columns; `total` is their
/// charged sum (`crate::provider_usage::sum_charged_from_persisted_parts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTokenUsage {
    pub input: u64,
    pub output: u64,
    pub total: u64,
}
