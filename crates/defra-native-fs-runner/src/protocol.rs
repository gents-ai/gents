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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobArgs {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    pub max_matches: usize,
    #[serde(default)]
    pub raw_json: bool,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeFsRunnerResponse {
    pub ok: bool,
    pub output: Option<String>,
    pub error: Option<String>,
}
