use crate::message::{
    Document, DocumentMediaType, DocumentSourceKind, Image, ImageDetail, ImageMediaType, Message,
    UserContent,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_ATTACHMENT_ENCODED_BYTES: usize = 5_592_408;
pub const MAX_TOTAL_ATTACHMENT_ENCODED_BYTES: usize = 11_184_816;
pub const MAX_ATTACHMENTS: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
    #[error("The attachment needs a file name")]
    MissingName,
    #[error("The attachment file name exceeds 1024 UTF-8 bytes")]
    NameTooLong,
    #[error("The file is empty")]
    Empty,
    #[error("Attachment exceeds 4 MiB")]
    TooLarge,
    #[error("Unsupported file. Choose PNG, JPEG, GIF, WebP, PDF or UTF-8 text")]
    Unsupported,
    #[error("Invalid attachment encoding")]
    InvalidEncoding,
    #[error("Attachment encoding must be canonical base64")]
    NonCanonicalEncoding,
    #[error("Text attachments must be UTF-8")]
    TextNotUtf8,
    #[error("Attach up to 4 files, at most 4 MiB each and 8 MiB total")]
    LimitsExceeded,
}

/// Frozen bytes are carried by the signed request and its existing ACP. The
/// name is display metadata, never a path to reopen on the executing node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestAttachment {
    pub name: String,
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AttachmentView {
    pub name: String,
    pub media_type: String,
    pub digest: String,
    pub size: usize,
}

impl RequestAttachment {
    pub fn is_well_formed(&self) -> bool {
        !self.name.trim().is_empty()
            && self.name.len() <= 1024
            && matches!(
                self.media_type.as_str(),
                "image/png"
                    | "image/jpeg"
                    | "image/gif"
                    | "image/webp"
                    | "application/pdf"
                    | "text/plain"
            )
            && !self.data.is_empty()
            && self.data.len() <= MAX_ATTACHMENT_ENCODED_BYTES
    }

    pub fn validate(&self) -> Result<(), AttachmentError> {
        if self.name.trim().is_empty() {
            return Err(AttachmentError::MissingName);
        }
        if self.name.len() > 1024 {
            return Err(AttachmentError::NameTooLong);
        }
        if self.data.is_empty() {
            return Err(AttachmentError::Empty);
        }
        if self.data.len() > MAX_ATTACHMENT_ENCODED_BYTES {
            return Err(AttachmentError::TooLarge);
        }
        if !self.is_well_formed() {
            return Err(AttachmentError::Unsupported);
        }
        let bytes = STANDARD
            .decode(&self.data)
            .map_err(|_| AttachmentError::InvalidEncoding)?;
        if STANDARD.encode(&bytes) != self.data {
            return Err(AttachmentError::NonCanonicalEncoding);
        }
        if self.media_type == "text/plain" {
            std::str::from_utf8(&bytes).map_err(|_| AttachmentError::TextNotUtf8)?;
        }
        Ok(())
    }

    pub fn view(&self) -> AttachmentView {
        AttachmentView {
            name: self.name.clone(),
            media_type: self.media_type.clone(),
            digest: format!("{:x}", Sha256::digest(self.data.as_bytes())),
            size: STANDARD.decode(&self.data).map(|b| b.len()).unwrap_or(0),
        }
    }

    pub fn to_content(&self) -> UserContent {
        let metadata = Some(serde_json::json!({"gents_attachment": self.name}));
        let data = DocumentSourceKind::Base64(self.data.clone());
        if let Some(media_type) = ImageMediaType::from_mime_type(&self.media_type) {
            UserContent::Image(Image {
                data,
                media_type: Some(media_type),
                detail: Some(ImageDetail::Auto),
                additional_params: metadata,
            })
        } else {
            UserContent::Document(Document {
                data,
                media_type: Some(if self.media_type == "application/pdf" {
                    DocumentMediaType::PDF
                } else {
                    DocumentMediaType::TXT
                }),
                additional_params: metadata,
            })
        }
    }

    pub fn from_message(message: &Message) -> Vec<Self> {
        let Message::User { content } = message else {
            return Vec::new();
        };
        content
            .iter()
            .filter_map(|part| {
                let (data, params, mime) = match part {
                    UserContent::Image(i) => (
                        &i.data,
                        &i.additional_params,
                        match i.media_type {
                            Some(ImageMediaType::PNG) => "image/png",
                            Some(ImageMediaType::JPEG) => "image/jpeg",
                            Some(ImageMediaType::GIF) => "image/gif",
                            Some(ImageMediaType::WEBP) => "image/webp",
                            _ => return None,
                        },
                    ),
                    UserContent::Document(d) => (
                        &d.data,
                        &d.additional_params,
                        match d.media_type {
                            Some(DocumentMediaType::PDF) => "application/pdf",
                            Some(DocumentMediaType::TXT) => "text/plain",
                            _ => return None,
                        },
                    ),
                    _ => return None,
                };
                let name = params.as_ref()?.get("gents_attachment")?.as_str()?;
                let DocumentSourceKind::Base64(data) = data else {
                    return None;
                };
                Some(Self {
                    name: name.into(),
                    media_type: mime.into(),
                    data: data.clone(),
                })
            })
            .collect()
    }
}

impl super::RequestInput {
    pub fn attachments_well_formed(&self) -> bool {
        self.attachments.len() <= MAX_ATTACHMENTS
            && self
                .attachments
                .iter()
                .all(RequestAttachment::is_well_formed)
            && self.attachments.iter().map(|a| a.data.len()).sum::<usize>()
                <= MAX_TOTAL_ATTACHMENT_ENCODED_BYTES
    }
    pub fn validate_attachments(&self) -> Result<(), AttachmentError> {
        if !self.attachments_well_formed() {
            return Err(AttachmentError::LimitsExceeded);
        }
        for attachment in &self.attachments {
            attachment.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_attachment_codec_retains_exact_frozen_bytes() {
        let attachment = RequestAttachment {
            name: "résumé.txt".into(),
            media_type: "text/plain".into(),
            data: STANDARD.encode("unsaved text\n"),
        };
        attachment.validate().unwrap();
        let input = crate::request_input::RequestInput {
            attachments: vec![attachment.clone()],
            ..Default::default()
        };
        let native = input.authored_message("").unwrap();
        assert_eq!(
            RequestAttachment::from_message(&native),
            vec![attachment.clone()]
        );
        let Message::User { content } = native else {
            panic!()
        };
        assert_eq!(content.len(), 1);
        let encoded = serde_json::to_string(&input).unwrap();
        let decoded: crate::request_input::RequestInput = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, input);
        assert_eq!(input.for_fresh_session().attachments, input.attachments);
        assert_eq!(input.reissued("source").attachments, input.attachments);
    }
    #[test]
    fn binary_codec_rejects_noncanonical_base64_and_non_utf8_text() {
        for data in ["%%bad", "/w=="] {
            assert!(RequestAttachment {
                name: "file.txt".into(),
                media_type: "text/plain".into(),
                data: data.into()
            }
            .validate()
            .is_err());
        }
    }
}
