use super::{MediaData, MessageBlock, PayloadRef, ReasoningPart, ToolResultPart};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceTable {
    reference_table_version: u32,
    closing_documents: Vec<String>,
    blocks: Vec<MessageBlock>,
}

/// Compacts only typed references; provider-owned JSON never enters the table.
/// The ordinary wire representation remains the expanded block array.
pub fn encode_blocks(blocks: &[MessageBlock]) -> serde_json::Result<Value> {
    let legacy = serde_json::to_value(blocks)?;
    let mut table = ReferenceTable {
        reference_table_version: 1,
        closing_documents: Vec::new(),
        blocks: blocks.to_vec(),
    };
    let mut slots = std::collections::HashMap::new();
    visit_references(&mut table.blocks, |reference| {
        let slot = *slots
            .entry(reference.close_doc_id.clone())
            .or_insert_with(|| {
                let slot = table.closing_documents.len();
                table.closing_documents.push(reference.close_doc_id.clone());
                slot
            });
        reference.close_doc_id = slot.to_string();
        Ok(())
    })
    .map_err(<serde_json::Error as serde::ser::Error>::custom)?;
    let compact = serde_json::to_value(table)?;
    if serde_json::to_vec(&compact)?.len() < serde_json::to_vec(&legacy)?.len() {
        Ok(compact)
    } else {
        Ok(legacy)
    }
}

pub(super) fn deserialize_blocks<'de, D>(deserializer: D) -> Result<Vec<MessageBlock>, D::Error>
where
    D: Deserializer<'de>,
{
    decode_blocks(Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

fn decode_blocks(value: Value) -> Result<Vec<MessageBlock>, String> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    if value.is_array() {
        return serde_json::from_value(value).map_err(|error| error.to_string());
    }
    let mut table: ReferenceTable =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    if table.reference_table_version != 1 {
        return Err("unsupported transcript reference table version".into());
    }
    visit_references(&mut table.blocks, |reference| {
        let slot = reference
            .close_doc_id
            .parse::<usize>()
            .map_err(|_| "invalid transcript reference slot")?;
        if reference.close_doc_id != slot.to_string() {
            return Err("noncanonical transcript reference slot".into());
        }
        reference.close_doc_id = table
            .closing_documents
            .get(slot)
            .ok_or("missing transcript reference slot")?
            .clone();
        Ok(())
    })?;
    Ok(table.blocks)
}

fn visit_references(
    blocks: &mut [MessageBlock],
    mut visit: impl FnMut(&mut PayloadRef) -> Result<(), String>,
) -> Result<(), String> {
    fn media(
        data: &mut MediaData,
        visit: &mut impl FnMut(&mut PayloadRef) -> Result<(), String>,
    ) -> Result<(), String> {
        match data {
            MediaData::Base64 { data } | MediaData::Raw { data } | MediaData::String { data } => {
                visit(data)
            }
            MediaData::Url { .. } | MediaData::Unknown => Ok(()),
        }
    }
    for block in blocks {
        match block {
            MessageBlock::Text { text } => visit(&mut text.output)?,
            MessageBlock::Reasoning { parts, .. } => {
                for part in parts {
                    match part {
                        ReasoningPart::Text { text, .. } | ReasoningPart::Summary { text } => {
                            visit(text)?
                        }
                        ReasoningPart::Encrypted { data } | ReasoningPart::Redacted { data } => {
                            visit(data)?
                        }
                    }
                }
            }
            MessageBlock::ToolCall { arguments, .. } => visit(arguments)?,
            MessageBlock::ToolResult { parts, .. } => {
                for part in parts {
                    match part {
                        ToolResultPart::Text { text } => visit(&mut text.output)?,
                        ToolResultPart::Media(block) => media(&mut block.data, &mut visit)?,
                    }
                }
            }
            MessageBlock::Media(block) => media(&mut block.data, &mut visit)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn storage_roundtrip_preserves_opaque_metadata_and_rejects_invalid_slots() {
        let closing = "bafyreia-long-closing-document-identity".repeat(3);
        let blocks = vec![
            MessageBlock::Reasoning {
                id: Some("provider-id".into()),
                parts: (0..32)
                    .map(|stream| ReasoningPart::Text {
                        text: PayloadRef {
                            close_doc_id: closing.clone(),
                            stream,
                        },
                        signature: Some("sig".into()),
                    })
                    .collect(),
            },
            MessageBlock::ToolCall {
                tool_call_doc_id: "tool".into(),
                id: "id".into(),
                call_id: None,
                name: "test".into(),
                arguments: PayloadRef {
                    close_doc_id: closing,
                    stream: 32,
                },
                signature: None,
                additional_params: Some(json!({"close_doc_id":"0", "reference_table_version":1})),
            },
        ];
        let encoded = encode_blocks(&blocks).unwrap();
        assert!(
            serde_json::to_vec(&encoded).unwrap().len()
                < serde_json::to_vec(&blocks).unwrap().len() / 2
        );
        assert_eq!(decode_blocks(encoded.clone()).unwrap(), blocks);
        assert_eq!(
            decode_blocks(serde_json::to_value(&blocks).unwrap()).unwrap(),
            blocks
        );
        for slot in ["999", "01", "-1", "NaN"] {
            let mut invalid = encoded.clone();
            invalid["blocks"][0]["parts"][0]["text"]["close_doc_id"] = json!(slot);
            assert!(decode_blocks(invalid).is_err());
        }
    }
}
