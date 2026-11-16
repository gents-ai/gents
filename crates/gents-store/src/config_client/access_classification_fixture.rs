//! Inputs and outputs of the `AccessClassification` families: production
//! access errors for each modeled failure, the access owner's classification
//! of whatever holds one, and the owner variants a source tree declares.

use std::collections::BTreeSet;
use std::path::Path;

use super::retry::{classified_transaction_conflict, transaction_conflict};
use super::write_telemetry::ConflictSource;
use super::{ConfigClientError, TransactionFailure};

/// `AccessClassification.Failure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessFailure {
    Unauthorized,
    Forbidden,
    PermissionDeniedCode,
    TransactionConflict,
    Other,
}

impl AccessFailure {
    pub fn error(self) -> ConfigClientError {
        let begin = |status| ConfigClientError::TxBeginStatus {
            status,
            endpoint: "http://node/api/v0/tx".into(),
            body: String::new(),
        };
        match self {
            Self::Unauthorized => begin(reqwest::StatusCode::UNAUTHORIZED),
            Self::Forbidden => begin(reqwest::StatusCode::FORBIDDEN),
            Self::PermissionDeniedCode => {
                ConfigClientError::Database(gents_protocol::database_error::GraphqlFailure::new(
                    "read",
                    serde_json::json!([{
                        "message": "opaque",
                        "extensions": {
                            "code": gents_protocol::database_error::PERMISSION_DENIED_CODE
                        }
                    }]),
                ))
            }
            Self::TransactionConflict => transaction_conflict(ConflictSource::StructuredCode),
            Self::Other => begin(reqwest::StatusCode::INTERNAL_SERVER_ERROR),
        }
    }
}

/// `AccessClassification.Erasure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Erasure {
    Typed,
    Anyhow,
    Boxed,
}

/// `AccessClassification.Classification`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessClassification {
    Refused,
    Conflict,
    Neither,
}

pub fn classify<E>(error: &E) -> AccessClassification
where
    E: TransactionFailure + ?Sized,
{
    if error.permission_denied() {
        AccessClassification::Refused
    } else if classified_transaction_conflict(error).is_some() {
        AccessClassification::Conflict
    } else {
        AccessClassification::Neither
    }
}

/// The classification of `error` as a receiver of `erasure` gets it, for an
/// owner with a classifier of its own.
pub fn classify_owner<E>(erasure: Erasure, error: E) -> AccessClassification
where
    E: TransactionFailure + std::error::Error + Send + Sync + 'static,
{
    match erasure {
        Erasure::Typed => classify(&error),
        Erasure::Anyhow | Erasure::Boxed => classify_erased(erasure, error),
    }
}

/// [`classify_owner`] for an owner with no classifier of its own: its typed
/// value is searched as the erased one is.
pub fn classify_erased<E>(erasure: Erasure, error: E) -> AccessClassification
where
    E: std::error::Error + Send + Sync + 'static,
{
    match erasure {
        Erasure::Anyhow => classify(&anyhow::Error::new(error)),
        Erasure::Typed | Erasure::Boxed => {
            classify(&(Box::new(error) as Box<dyn std::error::Error + Send + Sync>))
        }
    }
}

/// Every `<owner>_<variant>` (in a family's wire spelling) of a `thiserror`
/// enum under `directory` whose fields name one of `held`. The access owners
/// themselves are not holders.
pub fn error_owner_variants_holding(directory: &Path, held: &[&str]) -> BTreeSet<String> {
    let mut holders = BTreeSet::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            let entries = std::fs::read_dir(&path).expect("source directory");
            pending.extend(entries.map(|entry| entry.expect("source entry").path()));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let source = std::fs::read_to_string(&path).expect("source file");
            holders.extend(holders_in(&without_comments(&source), held));
        }
    }
    holders
}

fn without_comments(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_identifier(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn names(text: &str, name: &str) -> bool {
    text.match_indices(name).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + name.len()..].chars().next();
        !before.is_some_and(is_identifier) && !after.is_some_and(is_identifier)
    })
}

/// The end of the bracketed group opening at `open`, skipping string literals.
fn group_end(text: &str, open: usize) -> usize {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (offset, character) in text[open..].char_indices() {
        if quoted {
            quoted = escaped || character != '"';
            escaped = !escaped && character == '\\';
            continue;
        }
        match character {
            '"' => quoted = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return open + offset;
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced group in a scanned source");
}

fn wire_name(owner: &str, variant: &str) -> String {
    let mut wire = String::new();
    for character in owner.chars().chain(variant.chars()) {
        if character.is_ascii_uppercase() && !wire.is_empty() {
            wire.push('_');
        }
        wire.push(character.to_ascii_lowercase());
    }
    wire
}

fn holders_in(source: &str, held: &[&str]) -> Vec<String> {
    let mut holders = Vec::new();
    let mut rest = source;
    while let Some(derive) = rest.find("thiserror::Error") {
        rest = &rest[derive..];
        let Some(keyword) = rest.find("enum ").filter(|keyword| {
            let between = &rest[..*keyword];
            !between.contains('{') && !between.contains(';')
        }) else {
            rest = &rest["thiserror::Error".len()..];
            continue;
        };
        let header = &rest[keyword + "enum ".len()..];
        let owner: String = header.chars().take_while(|c| is_identifier(*c)).collect();
        let open = keyword + "enum ".len() + header.find('{').expect("enum body");
        let close = group_end(rest, open);
        let body = &rest[open + 1..close];
        rest = &rest[close..];
        if held.contains(&owner.as_str()) {
            continue;
        }
        let mut cursor = 0;
        while cursor < body.len() {
            let item = &body[cursor..];
            let trimmed = item.trim_start();
            let start = cursor + (item.len() - trimmed.len());
            if trimmed.is_empty() {
                break;
            }
            if trimmed.starts_with('#') {
                let open = start + trimmed.find('[').expect("attribute");
                cursor = group_end(body, open) + 1;
                continue;
            }
            let variant: String = trimmed.chars().take_while(|c| is_identifier(*c)).collect();
            let mut end = start + variant.len();
            let after = body[end..].trim_start();
            if after.starts_with('(') || after.starts_with('{') {
                let open = end + (body[end..].len() - after.len());
                end = group_end(body, open) + 1;
                if held.iter().any(|name| names(&body[open..end], name)) {
                    holders.push(wire_name(&owner, &variant));
                }
            }
            cursor = body[end..]
                .find(',')
                .map_or(body.len(), |comma| end + comma + 1);
        }
    }
    holders
}

#[cfg(test)]
mod tests {
    use super::holders_in;

    #[test]
    fn finds_tuple_and_struct_holders_and_skips_other_variants() {
        let source = r#"
            #[derive(Debug, DERIVE)]
            pub enum ReadError {
                #[error("no actor, said {0}")]
                Actor(String),
                #[error(transparent)]
                Execute(#[from] ConfigClientError),
                #[error("issuing {tuple} failed (see {})", "a, b")]
                Issue {
                    tuple: &'static str,
                    #[source]
                    source: Box<crate::config_client::ConfigClientError>,
                },
                Unit,
                #[error("response")]
                Response(#[source] GraphqlResponseError),
                NotHeld(NotConfigClientError),
            }
            #[derive(Debug, DERIVE)]
            #[error("a struct")]
            pub struct Step(#[source] ConfigClientError);
            #[derive(Debug, DERIVE)]
            pub enum ConfigClientError {
                Response(GraphqlResponseError),
            }
        "#
        .replace("DERIVE", concat!("thiserror", "::Error"));
        assert_eq!(
            holders_in(&source, &["ConfigClientError", "GraphqlResponseError"]),
            [
                "read_error_execute",
                "read_error_issue",
                "read_error_response"
            ]
        );
    }
}
