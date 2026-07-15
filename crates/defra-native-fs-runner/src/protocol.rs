use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NativeFsRunnerRequest {
    ListFiles(ListFilesArgs),
    Glob(GlobArgs),
    Grep(GrepArgs),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListFilesArgs {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub recursive: bool,
    pub max_entries: usize,
    #[serde(default)]
    pub raw_json: bool,
    /// Walk budget: entries admitted before the walk stops with partial
    /// results. `None` uses the runner default.
    #[serde(default)]
    pub max_entries_visited: Option<usize>,
    /// Walk budget: wall-clock milliseconds before the walk stops with
    /// partial results. `None` uses the runner default.
    #[serde(default)]
    pub max_wall_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobArgs {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    pub max_matches: usize,
    #[serde(default)]
    pub raw_json: bool,
    /// Walk budget: entries admitted before the walk stops with partial
    /// results. `None` uses the runner default.
    #[serde(default)]
    pub max_entries_visited: Option<usize>,
    /// Walk budget: wall-clock milliseconds before the walk stops with
    /// partial results. `None` uses the runner default.
    #[serde(default)]
    pub max_wall_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepArgs {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub case_sensitive: bool,
    pub max_matches: usize,
    #[serde(default)]
    pub raw_json: bool,
    /// Walk budget: entries admitted before the walk stops with partial
    /// results. `None` uses the runner default.
    #[serde(default)]
    pub max_entries_visited: Option<usize>,
    /// Walk budget: total file bytes read before grep stops with partial
    /// results. `None` uses the runner default.
    #[serde(default)]
    pub max_bytes_read: Option<u64>,
    /// Walk budget: wall-clock milliseconds before the walk stops with
    /// partial results. `None` uses the runner default.
    #[serde(default)]
    pub max_wall_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeFsRunnerResponse {
    pub ok: bool,
    pub output: Option<String>,
    pub error: Option<String>,
}
