use serde::Serialize;

pub(crate) const DEFAULT_IGNORED_NAMES: &[&str] = &[
    ".cache",
    ".direnv",
    ".git",
    ".next",
    ".turbo",
    ".venv",
    "dist",
    "node_modules",
    "target",
    "venv",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FilesystemEntry {
    pub(crate) path: String,
    pub(crate) entry_type: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrepMatch {
    pub(crate) path: String,
    pub(crate) line_number: usize,
    pub(crate) line: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Collected<T> {
    pub(crate) items: Vec<T>,
    pub(crate) truncated: bool,
}
