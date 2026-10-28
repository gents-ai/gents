//! Typed question carried in an `ask` MailboxItem payload; the answer is the
//! ordinary interactive reply request whose content
//! [`MailboxQuestion::reply_content`] renders.

use serde::{Deserialize, Serialize};

pub const MAILBOX_QUESTION_VERSION: u32 = 3;
pub const STATUS_QUESTION_CONTRACT_FIELD: &str = "mailbox_question_contract_version";
pub const MAILBOX_QUESTION_MIN_OPTIONS: usize = 2;
pub const MAILBOX_QUESTION_MAX_OPTIONS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct MailboxQuestionOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct MailboxQuestion {
    pub version: u32,
    pub prompt: String,
    pub options: Vec<MailboxQuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default)]
    pub allow_free_text: bool,
    #[serde(default, skip_serializing_if = "absent_or_false")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub optional: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub recommended_order: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct MailboxQuestionAnswer {
    #[serde(default, deserialize_with = "crate::row::deserialize_null_default")]
    pub option_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub free_text: Option<String>,
    #[serde(default, skip_serializing_if = "absent_or_false")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub skip: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub question_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxQuestionReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_version: Option<u32>,
    pub source_doc_id: String,
    pub question_hash: String,
    #[serde(serialize_with = "serialize_stored_answer")]
    pub answer: MailboxQuestionAnswer,
}

#[derive(Debug, thiserror::Error)]
pub enum MailboxQuestionReplyError {
    #[error(transparent)]
    Question(#[from] MailboxQuestionError),
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
    #[error("the question changed; reload its current prompt before replying")]
    SourceChangedBeforeReply,
    #[error("the question changed; review its current prompt before replying")]
    SourceChangedBeforeReview,
    #[error("the question version changed; reload its current prompt before replying")]
    VersionChanged,
    #[error("the signed reply does not match its question answer")]
    SignedAnswerMismatch,
}

/// DefraDB represents empty nested lists as null. Normalize only stored
/// evidence; public answer bytes also identify retained caller submissions.
fn serialize_stored_answer<S>(
    answer: &MailboxQuestionAnswer,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let mut value =
        serde_json::to_value(answer).map_err(<S::Error as serde::ser::Error>::custom)?;
    if answer.option_ids.is_empty() {
        value["option_ids"] = serde_json::Value::Null;
    }
    value.serialize(serializer)
}

impl MailboxQuestionReply {
    pub fn new(
        source_doc_id: &str,
        title: &str,
        summary: Option<&str>,
        payload: &str,
        request_id: Option<&str>,
        answer: MailboxQuestionAnswer,
    ) -> Result<Self, MailboxQuestionReplyError> {
        let question = MailboxQuestion::from_payload(payload)?;
        let question_hash = question_source_hash(title, summary, payload, request_id)?;
        if answer.question_hash.as_deref() != Some(question_hash.as_str()) {
            return Err(MailboxQuestionReplyError::SourceChangedBeforeReply);
        }
        Ok(Self {
            question_version: (question.version >= 3).then_some(question.version),
            source_doc_id: source_doc_id.into(),
            question_hash,
            answer,
        })
    }

    pub fn validate_for(
        &self,
        source_doc_id: &str,
        title: &str,
        summary: Option<&str>,
        payload: &str,
        request_id: Option<&str>,
        content: &str,
    ) -> Result<(), MailboxQuestionReplyError> {
        if self.source_doc_id != source_doc_id
            || self.question_hash != question_source_hash(title, summary, payload, request_id)?
            || self.answer.question_hash.as_deref() != Some(self.question_hash.as_str())
        {
            return Err(MailboxQuestionReplyError::SourceChangedBeforeReview);
        }
        let question = MailboxQuestion::from_payload(payload)?;
        if !self
            .question_version
            .map_or(question.version <= 2, |version| {
                version >= 3 && version == question.version
            })
        {
            return Err(MailboxQuestionReplyError::VersionChanged);
        }
        if question.reply_content(title, &self.answer)? != content {
            return Err(MailboxQuestionReplyError::SignedAnswerMismatch);
        }
        Ok(())
    }
}

pub fn question_source_hash(
    title: &str,
    summary: Option<&str>,
    payload: &str,
    request_id: Option<&str>,
) -> Result<String, MailboxQuestionReplyError> {
    use sha2::{Digest, Sha256};
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&(title, summary, payload, request_id))?)
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailboxQuestionError {
    Version,
    EmptyPrompt,
    OptionCount,
    EmptyOption,
    PromptNotAllowed,
    RecommendedOrder,
    DuplicateOption(String),
    UnknownOption(String),
    MultipleSelected,
    RepeatedSelection(String),
    FreeTextNotAllowed,
    Empty,
    SkipNotAllowed,
    SkipHasAnswer,
    Decode(String),
}

impl std::fmt::Display for MailboxQuestionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Version => write!(f, "question version must be 1-{MAILBOX_QUESTION_VERSION}; optional questions require version 2 or later"),
            Self::EmptyPrompt => f.write_str("question prompt must not be empty"),
            Self::OptionCount => write!(
                f,
                "question needs {MAILBOX_QUESTION_MIN_OPTIONS}-{MAILBOX_QUESTION_MAX_OPTIONS} options, or version 2 or later with no options and free text enabled"
            ),
            Self::PromptNotAllowed => f.write_str("editable prompts require question version 3, non-empty prompt text, allow_free_text and a single choice"),
            Self::RecommendedOrder => f.write_str("recommended_order requires question version 3 and multi_select; list every option id exactly once"),
            Self::EmptyOption => f.write_str("question option ids and labels must be non-empty"),
            Self::DuplicateOption(id) => write!(f, "question option id {id:?} is duplicated"),
            Self::UnknownOption(id) => write!(f, "answer names unknown option {id:?}"),
            Self::MultipleSelected => {
                f.write_str("answer selects more than one option of a single-select question")
            }
            Self::RepeatedSelection(id) => write!(f, "answer repeats option {id:?}"),
            Self::FreeTextNotAllowed => f.write_str("question does not accept a free-text answer"),
            Self::Empty => f.write_str("answer selects nothing"),
            Self::SkipNotAllowed => f.write_str("answer this question; only an explicitly optional version 2 or later question can be skipped"),
            Self::SkipHasAnswer => f.write_str("send either Skip or an answer, not both"),
            Self::Decode(error) => write!(f, "payload is not a question: {error}"),
        }
    }
}

impl std::error::Error for MailboxQuestionError {}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn absent_or_false(value: &Option<bool>) -> bool {
    *value != Some(true)
}

pub fn question_requires_reply_receipt(payload: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| value.get("version").and_then(serde_json::Value::as_u64))
        .is_some_and(|version| version >= 2)
}

impl MailboxQuestion {
    pub fn awaiting_answer(&self, is_open: bool, same_parent: bool, completed: bool) -> bool {
        self.validate().is_ok()
            && self.version >= 2
            && self.optional != Some(true)
            && is_open
            && same_parent
            && completed
    }

    pub fn validate(&self) -> Result<(), MailboxQuestionError> {
        if !(1..=MAILBOX_QUESTION_VERSION).contains(&self.version)
            || (self.version == 1 && self.optional == Some(true))
        {
            return Err(MailboxQuestionError::Version);
        }
        if self.prompt.trim().is_empty() {
            return Err(MailboxQuestionError::EmptyPrompt);
        }
        if !((MAILBOX_QUESTION_MIN_OPTIONS..=MAILBOX_QUESTION_MAX_OPTIONS)
            .contains(&self.options.len())
            || (self.version >= 2 && self.options.is_empty() && self.allow_free_text))
        {
            return Err(MailboxQuestionError::OptionCount);
        }
        let mut seen = std::collections::BTreeSet::new();
        for option in &self.options {
            if option.id.trim().is_empty() || option.label.trim().is_empty() {
                return Err(MailboxQuestionError::EmptyOption);
            }
            if !seen.insert(option.id.trim()) {
                return Err(MailboxQuestionError::DuplicateOption(option.id.clone()));
            }
            if option.prompt.as_deref().is_some_and(|prompt| {
                self.version != 3
                    || !self.allow_free_text
                    || self.multi_select
                    || prompt.trim().is_empty()
            }) {
                return Err(MailboxQuestionError::PromptNotAllowed);
            }
        }
        if let Some(order) = &self.recommended_order {
            if self.version != 3
                || !self.multi_select
                || self.options.is_empty()
                || order.len() != self.options.len()
                || order
                    .iter()
                    .map(|id| id.trim())
                    .collect::<std::collections::BTreeSet<_>>()
                    != seen
            {
                return Err(MailboxQuestionError::RecommendedOrder);
            }
        }
        Ok(())
    }

    pub fn from_payload(payload: &str) -> Result<Self, MailboxQuestionError> {
        let question: Self = serde_json::from_str(payload)
            .map_err(|error| MailboxQuestionError::Decode(error.to_string()))?;
        question.validate()?;
        Ok(question)
    }

    pub fn reply_content(
        &self,
        title: &str,
        answer: &MailboxQuestionAnswer,
    ) -> Result<String, MailboxQuestionError> {
        self.validate()?;
        let free_text = trimmed(answer.free_text.as_deref());
        if answer.skip == Some(true) {
            if self.version < 2 || self.optional != Some(true) {
                return Err(MailboxQuestionError::SkipNotAllowed);
            }
            if !answer.option_ids.is_empty() || free_text.is_some() {
                return Err(MailboxQuestionError::SkipHasAnswer);
            }
            return Ok(format!("Skipped optional decision on {}.", title.trim()));
        }
        if free_text.is_some() && !self.allow_free_text {
            return Err(MailboxQuestionError::FreeTextNotAllowed);
        }
        if answer.option_ids.len() > 1 && !self.multi_select {
            return Err(MailboxQuestionError::MultipleSelected);
        }
        let mut chosen = Vec::with_capacity(answer.option_ids.len());
        for id in &answer.option_ids {
            let option = self
                .options
                .iter()
                .find(|option| option.id.trim() == id.trim())
                .ok_or_else(|| MailboxQuestionError::UnknownOption(id.clone()))?;
            if chosen
                .iter()
                .any(|seen: &&MailboxQuestionOption| seen.id == option.id)
            {
                return Err(MailboxQuestionError::RepeatedSelection(id.clone()));
            }
            chosen.push(option);
        }
        let choice = match (chosen.is_empty(), free_text) {
            (true, None) => return Err(MailboxQuestionError::Empty),
            (true, Some(_)) => "Other".to_string(),
            (false, _) => chosen
                .iter()
                .map(|option| format!("{} ({})", option.label.trim(), option.id.trim()))
                .collect::<Vec<_>>()
                .join(", "),
        };
        let mut content = format!("Decision on {}: {choice}", title.trim());
        if let Some(note) = free_text {
            content.push_str("\n\n");
            content.push_str(note);
        }
        Ok(content)
    }
}
