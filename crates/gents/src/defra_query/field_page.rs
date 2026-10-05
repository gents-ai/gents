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
        let hash = format!("sha256:{:x}", Sha256::digest(text.as_bytes()));
        ensure!(
            self.expected_hash
                .as_ref()
                .is_none_or(|expected| expected == &hash),
            "field_page value changed; restart at offset 0"
        );
        let offset = self.offset_bytes;
        ensure!(
            offset <= text.len() && text.is_char_boundary(offset),
            "field_page offset is not a UTF-8 boundary within the value"
        );
        let mut end = offset
            .saturating_add(MAX_FIELD_STRING_BYTES)
            .min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Ok(
            json!({"doc_id":self.doc_id,"field":self.field,"text":&text[offset..end],"offset_bytes":offset,"next_offset_bytes":end,"total_bytes":text.len(),"value_hash":hash,"complete":end == text.len()}),
        )
    }
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
