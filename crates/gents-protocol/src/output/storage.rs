use super::{
    MediaData, MessageBlock, MessagePublication, MessageRole, OutputOutcome, PayloadRef,
    ReasoningPart, ToolResultPart, TranscriptMessage,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Key of the provider request id inside a stored `publication` (see [`TranscriptMessage`]).
pub const NATIVE_REQUEST_ID_KEY: &str = "native_request_id";

/// Whether a stored `publication` carries [`NATIVE_REQUEST_ID_KEY`]. Off: at v1.0.0-rc0
/// `MessagePublication` (and `TranscriptMessage`) refuse unknown keys, so an rc0-era reader (a
/// node not yet upgraded, a client built on rc0's protocol) could not decode any message that
/// carried it. This release reads the key either way; turn this on once no rc0-era reader
/// remains. Enforced here, at the one place a publication is encoded, so no writer can bypass
/// it.
pub const PERSIST_NATIVE_REQUEST_ID: bool = false;

/// The stored `AgentMessage` document: exactly the v1.0.0-rc0 `AgentMessage` schema.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TranscriptMessageWire {
    message_key: String,
    session_id: String,
    node_did: String,
    #[serde(default)]
    requester_did: Option<String>,
    #[serde(default)]
    request_doc_id: Option<String>,
    publication: StoredPublication,
    outcome: OutputOutcome,
    sequence: u32,
    role: MessageRole,
    #[serde(default)]
    native_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_blocks")]
    blocks: Vec<MessageBlock>,
    created_at: String,
}

impl TryFrom<TranscriptMessageWire> for TranscriptMessage {
    type Error = String;

    fn try_from(wire: TranscriptMessageWire) -> Result<Self, Self::Error> {
        let native_request_id = wire.publication.native_request_id;
        Ok(Self {
            message_key: wire.message_key,
            session_id: wire.session_id,
            node_did: wire.node_did,
            requester_did: wire.requester_did,
            request_doc_id: wire.request_doc_id,
            publication: wire.publication.publication,
            outcome: wire.outcome,
            sequence: wire.sequence,
            role: wire.role,
            native_id: wire.native_id,
            native_request_id,
            blocks: wire.blocks,
            created_at: wire.created_at,
        })
    }
}

impl Serialize for TranscriptMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Stored<'a> {
            message_key: &'a str,
            session_id: &'a str,
            node_did: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            requester_did: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            request_doc_id: Option<&'a str>,
            publication: Value,
            outcome: &'a OutputOutcome,
            sequence: u32,
            role: &'a MessageRole,
            #[serde(skip_serializing_if = "Option::is_none")]
            native_id: Option<&'a str>,
            #[serde(skip_serializing_if = "<[MessageBlock]>::is_empty")]
            blocks: &'a [MessageBlock],
            created_at: &'a str,
        }
        let publication = StoredPublication::encode(
            &self.publication,
            self.native_request_id.as_deref(),
            PERSIST_NATIVE_REQUEST_ID,
        )
        .map_err(serde::ser::Error::custom)?;
        Stored {
            message_key: &self.message_key,
            session_id: &self.session_id,
            node_did: &self.node_did,
            requester_did: self.requester_did.as_deref(),
            request_doc_id: self.request_doc_id.as_deref(),
            publication,
            outcome: &self.outcome,
            sequence: self.sequence,
            role: &self.role,
            native_id: self.native_id.as_deref(),
            blocks: &self.blocks,
            created_at: &self.created_at,
        }
        .serialize(serializer)
    }
}

/// A stored `publication`: the tagged [`MessagePublication`] object, carrying
/// [`NATIVE_REQUEST_ID_KEY`] only when [`PERSIST_NATIVE_REQUEST_ID`] is on.
struct StoredPublication {
    publication: MessagePublication,
    native_request_id: Option<String>,
}

impl StoredPublication {
    fn encode(
        publication: &MessagePublication,
        native_request_id: Option<&str>,
        persist_request_id: bool,
    ) -> serde_json::Result<Value> {
        let mut value = serde_json::to_value(publication)?;
        if let Some(id) = native_request_id.filter(|_| persist_request_id) {
            value
                .as_object_mut()
                .ok_or_else(|| {
                    <serde_json::Error as serde::ser::Error>::custom(
                        "publication is not a JSON object",
                    )
                })?
                .insert(
                    NATIVE_REQUEST_ID_KEY.to_owned(),
                    Value::String(id.to_owned()),
                );
        }
        Ok(value)
    }
}

impl<'de> Deserialize<'de> for StoredPublication {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let mut value = Value::deserialize(deserializer)?;
        let native_request_id = match value
            .as_object_mut()
            .and_then(|object| object.remove(NATIVE_REQUEST_ID_KEY))
        {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) => Some(id),
            Some(other) => {
                return Err(D::Error::custom(format!(
                    "publication.{NATIVE_REQUEST_ID_KEY} is not a string: {other}"
                )))
            }
        };
        Ok(Self {
            publication: serde_json::from_value(value).map_err(D::Error::custom)?,
            native_request_id,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceTable {
    closing_documents: Vec<String>,
    blocks: Vec<MessageBlock>,
}

/// Compacts only typed references; provider-owned JSON never enters the table.
/// The ordinary wire representation remains the expanded block array, so this
/// returns the table only when it serializes smaller.
pub fn encode_blocks(blocks: &[MessageBlock]) -> serde_json::Result<Option<Value>> {
    let mut table = ReferenceTable {
        closing_documents: Vec::new(),
        blocks: blocks.to_vec(),
    };
    let mut slots = rapidhash::RapidHashMap::default();
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
    let encoded = serde_json::to_vec(&table)?;
    if encoded.len() < serde_json::to_vec(blocks)?.len() {
        serde_json::from_slice(&encoded).map(Some)
    } else {
        Ok(None)
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
                additional_params: Some(json!({"close_doc_id":"0", "closing_documents":[]})),
            },
        ];
        let encoded = encode_blocks(&blocks).unwrap().unwrap();
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

#[cfg(test)]
mod stored_message_tests {
    use super::*;
    use crate::output::{MessagePublication, OutputOutcome};

    fn message(native_request_id: Option<&str>) -> TranscriptMessage {
        TranscriptMessage {
            message_key: "provider-turn".into(),
            session_id: "session".into(),
            node_did: "did:key:node".into(),
            requester_did: None,
            request_doc_id: Some("request".into()),
            publication: MessagePublication::RequestExecution {
                execution_generation: "generation".into(),
            },
            outcome: OutputOutcome::Complete,
            sequence: 1,
            role: MessageRole::Assistant,
            native_id: Some("msg_1".into()),
            native_request_id: native_request_id.map(str::to_owned),
            blocks: Vec::new(),
            created_at: "2026-11-02T00:00:00Z".into(),
        }
    }

    /// `AgentMessage` exactly as v1.0.0-rc0's `TranscriptMessage` decoded it (689d75369
    /// `gents-protocol/src/output.rs`): these fields, `deny_unknown_fields`, and a
    /// `publication` that is rc0's `MessagePublication` (unchanged since, and also
    /// `deny_unknown_fields`). The blocks column is rc0's reference-table JSON, kept opaque.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Rc0TranscriptMessage {
        message_key: String,
        session_id: String,
        node_did: String,
        #[serde(default)]
        requester_did: Option<String>,
        #[serde(default)]
        request_doc_id: Option<String>,
        publication: MessagePublication,
        outcome: OutputOutcome,
        sequence: u32,
        role: MessageRole,
        #[serde(default)]
        native_id: Option<String>,
        #[serde(default)]
        blocks: Option<Value>,
        created_at: String,
    }

    fn row_for(message: &TranscriptMessage, persist: bool) -> Value {
        let mut row = serde_json::to_value(message).unwrap();
        row["publication"] = StoredPublication::encode(
            &message.publication,
            message.native_request_id.as_deref(),
            persist,
        )
        .unwrap();
        row
    }

    #[test]
    fn an_rc0_reader_decodes_every_message_this_build_writes() {
        assert!(
            !PERSIST_NATIVE_REQUEST_ID,
            "persisting the request id makes rc0-era readers refuse every reply that carries \
             one; update this test and the runbook when an rc0-era reader no longer exists"
        );
        for written in [message(Some("req_1")), message(None)] {
            let row = serde_json::to_value(&written).unwrap();
            assert!(
                row["publication"].get(NATIVE_REQUEST_ID_KEY).is_none(),
                "{row}"
            );
            assert!(row.get(NATIVE_REQUEST_ID_KEY).is_none(), "{row}");
            serde_json::from_value::<Rc0TranscriptMessage>(row.clone())
                .unwrap_or_else(|error| panic!("rc0 reader refused {row}: {error}"));
            let back: TranscriptMessage = serde_json::from_value(row).unwrap();
            assert_eq!(back, message(None));
        }
    }

    #[test]
    fn persisting_the_request_id_nests_it_and_an_rc0_reader_refuses_it() {
        let row = row_for(&message(Some("req_1")), true);
        assert!(row.get(NATIVE_REQUEST_ID_KEY).is_none(), "{row}");
        assert_eq!(
            row["publication"],
            serde_json::json!({"kind": "request_execution", "execution_generation": "generation",
                "native_request_id": "req_1"})
        );
        // Why the gate is off while rc0-era readers remain.
        assert!(serde_json::from_value::<Rc0TranscriptMessage>(row.clone()).is_err());
        // This build reads it back.
        let back: TranscriptMessage = serde_json::from_value(row).unwrap();
        assert_eq!(back, message(Some("req_1")));
        // Without an id there is nothing to nest, whatever the gate.
        let row = row_for(&message(None), true);
        assert!(row["publication"].get(NATIVE_REQUEST_ID_KEY).is_none());
        serde_json::from_value::<Rc0TranscriptMessage>(row).unwrap();
    }

    #[test]
    fn malformed_or_unknown_keys_are_refused() {
        let mut number = row_for(&message(None), true);
        number["publication"][NATIVE_REQUEST_ID_KEY] = 7.into();
        assert!(serde_json::from_value::<TranscriptMessage>(number).is_err());

        let mut unknown = serde_json::to_value(message(None)).unwrap();
        unknown["publication"]["surprise"] = true.into();
        assert!(serde_json::from_value::<TranscriptMessage>(unknown.clone()).is_err());
        unknown["publication"]
            .as_object_mut()
            .unwrap()
            .remove("surprise");
        // A top-level request id (v1.0.0-rc1's column) is no field of this schema.
        unknown[NATIVE_REQUEST_ID_KEY] = "req_1".into();
        assert!(serde_json::from_value::<TranscriptMessage>(unknown).is_err());
    }
}
