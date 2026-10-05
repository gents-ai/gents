use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::MAX_FIELD_STRING_BYTES;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FieldPage {
    pub doc_id: String,
    pub field: String,
    #[serde(default)]
    pub offset_bytes: usize,
    pub expected_hash: Option<String>,
}

impl FieldPage {
    pub fn validate(&self, fields: &[String]) -> Result<()> {
        ensure!(
            !self.doc_id.trim().is_empty(),
            "field_page.doc_id must be nonempty"
        );
        ensure!(
            fields.contains(&self.field),
            "field_page.field must be in the selected projection"
        );
        ensure!(
            self.offset_bytes == 0 || self.expected_hash.is_some(),
            "field_page.expected_hash is required for continuation"
        );
        Ok(())
    }

    pub fn read(&self, row: &Value, fields: &[String]) -> Result<Value> {
        self.validate(fields)?;
        ensure!(
            row["_docID"].as_str() == Some(self.doc_id.as_str()),
            "field_page document does not match"
        );
        let text = row[&self.field]
            .as_str()
            .context("field_page requires a String value")?;
        let page = utf8_page(
            text,
            self.offset_bytes,
            self.expected_hash.as_deref(),
            MAX_FIELD_STRING_BYTES,
        )
        .map_err(|error| {
            anyhow::anyhow!(match error {
                Utf8PageError::MissingHash =>
                    "field_page.expected_hash is required for continuation",
                Utf8PageError::Changed => "field_page value changed; restart at offset 0",
                Utf8PageError::Offset =>
                    "field_page offset is not a UTF-8 boundary within the value",
            })
        })?;
        Ok(
            json!({"doc_id":self.doc_id,"field":self.field,"text":page.text,"offset_bytes":self.offset_bytes,"next_offset_bytes":page.end,"total_bytes":text.len(),"value_hash":page.hash,"complete":page.end == text.len()}),
        )
    }
}

/// One byte page of an immutable UTF-8 value: `ToolPolicy.FieldRead.page`.
pub(crate) struct Utf8Page<'a> {
    pub text: &'a str,
    pub end: usize,
    pub hash: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Utf8PageError {
    MissingHash,
    Changed,
    Offset,
}

/// Select the longest whole-scalar page of at most `budget` bytes at `offset`.
/// A continuation must carry the SHA-256 of the value its earlier pages came
/// from, so pages of different versions are never joined.
pub(crate) fn utf8_page<'a>(
    text: &'a str,
    offset: usize,
    expected_hash: Option<&str>,
    budget: usize,
) -> std::result::Result<Utf8Page<'a>, Utf8PageError> {
    if offset > 0 && expected_hash.is_none() {
        return Err(Utf8PageError::MissingHash);
    }
    let hash = format!("sha256:{:x}", Sha256::digest(text.as_bytes()));
    if expected_hash.is_some_and(|expected| expected != hash) {
        return Err(Utf8PageError::Changed);
    }
    if offset > text.len() || !text.is_char_boundary(offset) {
        return Err(Utf8PageError::Offset);
    }
    let mut end = offset.saturating_add(budget).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok(Utf8Page {
        text: &text[offset..end],
        end,
        hash,
    })
}

pub(super) fn recovery_metadata(rows: &Value) -> Vec<Value> {
    rows.as_array().into_iter().flatten().flat_map(|row| {
        row.as_object().into_iter().flatten().filter_map(move |(field, value)| {
            let text = value.as_str()?;
            (text.len() > MAX_FIELD_STRING_BYTES).then(|| json!({"doc_id":row["_docID"],"field":field,"total_bytes":text.len(),"field_page":{"doc_id":row["_docID"],"field":field,"offset_bytes":0}}))
        })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn field_pages_match_lean() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().field_read_cases;
        assert!(!cases.is_empty());
        for case in cases {
            let text: String = case["widths"]
                .as_array()
                .unwrap()
                .iter()
                .map(|w| match w.as_u64().unwrap() {
                    1 => 'a',
                    2 => 'é',
                    3 => '€',
                    4 => '😀',
                    _ => panic!(),
                })
                .collect();
            let granted = case["granted"].as_bool().unwrap();
            let present = case["present"].as_bool().unwrap();
            let matching = case["hashOk"].as_bool().unwrap();
            let hash = format!("sha256:{:x}", Sha256::digest(text.as_bytes()));
            let page = FieldPage {
                doc_id: "doc".into(),
                field: "value".into(),
                offset_bytes: case["offset"].as_u64().unwrap() as usize,
                expected_hash: present.then(|| if matching { hash } else { "changed".into() }),
            };
            let fields = if granted {
                vec!["value".into()]
            } else {
                vec![]
            };
            let result = page.read(&json!({"_docID":"doc","value":text}), &fields);
            match case["next"].as_u64() {
                None => assert!(result.is_err(), "{case}"),
                Some(next) => {
                    let result = result.unwrap();
                    assert_eq!(result["next_offset_bytes"], next);
                    assert!(result["text"].as_str().unwrap().len() <= MAX_FIELD_STRING_BYTES);
                }
            }
        }
    }
}
