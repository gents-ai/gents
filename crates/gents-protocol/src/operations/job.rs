use crate::job::{JobReadError, JobState};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobListInput {
    pub node_did: String,
    pub states: Option<Vec<JobState>>,
    pub plugin: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobViewInput {
    pub node_did: String,
    pub address: Option<String>,
    pub row_doc_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum JobQueryError {
    #[error("A run's address is its session id and tool call id, joined by `:`.")]
    InvalidAddress,
    #[error("Name the run by exactly one of its address and its row id.")]
    InvalidSelector,
    #[error(transparent)]
    Read(#[from] JobReadError),
}
