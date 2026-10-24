use serde::{Deserialize, Serialize};

use super::{RequestInputError, Result};

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

impl CapturedContext {
    pub fn to_document(&self) -> Result<crate::message::Document> {
        let serde_json::Value::Object(mut metadata) = serde_json::to_value(self)? else {
            return Err(RequestInputError::MetadataNotObject);
        };
        metadata.remove("content");
        Ok(crate::message::Document {
            data: crate::message::DocumentSourceKind::String(self.content.clone()),
            media_type: Some(crate::message::DocumentMediaType::TXT),
            additional_params: Some(serde_json::json!({ "captured_context": metadata })),
        })
    }

    /// The native marker identifies structured context; ordinary document and
    /// prose content is never inspected for it. The payload is the sole text
    /// source, including in forks whose origin request is no longer local.
    pub fn from_document(document: &crate::message::Document) -> Result<Option<Self>> {
        let Some(params) = document.additional_params.as_ref() else {
            return Ok(None);
        };
        let Some(metadata) = params.get("captured_context") else {
            return Ok(None);
        };
        if !params.as_object().is_some_and(|object| object.len() == 1) {
            return Err(RequestInputError::UnrelatedProviderParameters);
        }
        if !matches!(
            document.media_type,
            Some(crate::message::DocumentMediaType::TXT)
        ) {
            return Err(RequestInputError::RequiresTextDocument);
        }
        let crate::message::DocumentSourceKind::String(content) = &document.data else {
            return Err(RequestInputError::RequiresCapturedText);
        };
        let mut fields = metadata
            .as_object()
            .cloned()
            .ok_or(RequestInputError::MetadataNotObject)?;
        if fields.contains_key("content") {
            return Err(RequestInputError::ContentInMetadata);
        }
        fields.insert(
            "content".to_owned(),
            serde_json::Value::String(content.clone()),
        );
        let context: Self = serde_json::from_value(serde_json::Value::Object(fields))?;
        if !context.is_well_formed() {
            return Err(RequestInputError::InvalidCapturedContext);
        }
        Ok(Some(context))
    }

    pub fn from_message(message: &crate::message::Message) -> Result<Vec<Self>> {
        let crate::message::Message::User { content } = message else {
            return Ok(Vec::new());
        };
        content
            .iter()
            .filter_map(|part| match part {
                crate::message::UserContent::Document(document) => {
                    Self::from_document(document).transpose()
                }
                _ => None,
            })
            .collect()
    }
}
