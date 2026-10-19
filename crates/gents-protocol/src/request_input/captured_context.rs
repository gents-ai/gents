use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CapturedContextKind {
    Selection,
    File,
    Diagnostics,
    Terminal,
}

impl CapturedContextKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Selection => "selection",
            Self::File => "file",
            Self::Diagnostics => "diagnostics",
            Self::Terminal => "terminal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CapturedContextSource {
    Editor,
    Clipboard,
}

impl CapturedContextSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Editor => "editor",
            Self::Clipboard => "clipboard",
        }
    }
}

/// Zero-based UTF-16 code-unit coordinates, never byte offsets into captured text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct CapturedPosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct CapturedRange {
    pub start: CapturedPosition,
    pub end: CapturedPosition,
}

/// Immutable caller observations authenticated with the request. Source URIs,
/// versions and unsaved state grant no access and must not be used to reopen a
/// file in place of the captured text. Admission verifies structure and signer,
/// not whether the caller's observations match a live editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapturedContext {
    pub kind: CapturedContextKind,
    pub source: CapturedContextSource,
    pub captured_at: String,
    pub uri: Option<String>,
    pub terminal_name: Option<String>,
    pub workspace_uri: Option<String>,
    pub version: Option<u32>,
    pub unsaved: bool,
    pub range: Option<CapturedRange>,
    pub content: String,
}

impl CapturedContext {
    pub fn is_well_formed(&self) -> bool {
        self.version.is_none_or(|version| version > 0)
            && self.range.is_none_or(|range| {
                (range.start.line, range.start.character) <= (range.end.line, range.end.character)
            })
            && (self.kind != CapturedContextKind::Selection || self.range.is_some())
    }
}
