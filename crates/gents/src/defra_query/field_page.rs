use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::error::DefraQueryFailure;
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
    pub fn validate(&self, fields: &[String]) -> Result<(), DefraQueryFailure> {
        if self.doc_id.trim().is_empty() {
            return Err(DefraQueryFailure::FieldPageDocIdEmpty);
        }
        if !fields.contains(&self.field) {
            return Err(DefraQueryFailure::FieldPageFieldNotSelected);
        }
        if self.offset_bytes != 0 && self.expected_hash.is_none() {
            return Err(DefraQueryFailure::FieldPageHashRequired);
        }
        Ok(())
    }

    pub fn read(&self, row: &Value, fields: &[String]) -> Result<Value, DefraQueryFailure> {
        self.validate(fields)?;
        if row["_docID"].as_str() != Some(self.doc_id.as_str()) {
            return Err(DefraQueryFailure::FieldPageDocMismatch);
        }
        let text = row[&self.field]
            .as_str()
            .ok_or(DefraQueryFailure::FieldPageNotString)?;
        let page = utf8_page(
            text,
            self.offset_bytes,
            self.expected_hash.as_deref(),
            MAX_FIELD_STRING_BYTES,
        )
        .map_err(|error| match error {
            Utf8PageError::MissingHash => DefraQueryFailure::FieldPageHashRequired,
            Utf8PageError::Changed => DefraQueryFailure::FieldPageChanged,
            Utf8PageError::Offset => DefraQueryFailure::FieldPageOffset,
        })?;
        Ok(
            json!({"doc_id":self.doc_id,"field":self.field,"text":page.text,"offset_bytes":self.offset_bytes,"next_offset_bytes":page.end,"total_bytes":text.len(),"value_hash":page.hash,"complete":page.end == text.len()}),
        )
    }
}

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

// `row` is the result index: a caller that did not select `_docID` has no
// other way to put the recovered value back where it came from.
pub(super) fn recovery_metadata(rows: &Value) -> Vec<Value> {
    rows.as_array().into_iter().flatten().enumerate().flat_map(|(index, row)| {
        row.as_object().into_iter().flatten().filter_map(move |(field, value)| {
            let text = value.as_str()?;
            (text.len() > MAX_FIELD_STRING_BYTES).then(|| json!({"row":index,"doc_id":row["_docID"],"field":field,"total_bytes":text.len(),"field_page":{"doc_id":row["_docID"],"field":field,"offset_bytes":0}}))
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
