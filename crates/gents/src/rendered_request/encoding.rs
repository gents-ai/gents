//! Versioned, lossless storage encoding for large capture JSON values.
//!
//! Legacy rows store the canonical JSON value directly. New rows may store a
//! full envelope or a delta against an immutable field-commit witness. Deltas
//! operate independently on top-level object fields; changed arrays/objects
//! use a byte splice, so an unrelated scalar change does not force a growing
//! message list to be repeated.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::Value;

pub(crate) const LOSSLESS_JSON_VERSION: u32 = 1;
const CAPTURE_CONTAINER_VERSION: u32 = 1;
pub(crate) const MAX_DELTA_DEPTH: u8 = 8;
const MIN_DELTA_SAVINGS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BaseWitness {
    pub(crate) doc_id: String,
    pub(crate) field_commit_cid: String,
    pub(crate) depth: u8,
    pub(crate) agent_did: String,
    pub(crate) requester_did: String,
    pub(crate) session_id: String,
    pub(crate) source: String,
    pub(crate) capture_scope: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum FieldDelta {
    Full {
        value: Value,
    },
    Splice {
        prefix_bytes: u64,
        suffix_bytes: u64,
        middle: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Payload {
    Full {
        value: Value,
    },
    ObjectDelta {
        base: BaseWitness,
        changed: BTreeMap<String, FieldDelta>,
        removed: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Envelope {
    gents_lossless_json: u32,
    #[serde(flatten)]
    payload: Payload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturePayloadKind {
    RequestBody,
    ProvenancePayload,
}

#[derive(Serialize)]
struct RawCaptureContainer<'a> {
    gents_capture_json: u32,
    request_body: &'a RawValue,
    provenance_payload: &'a RawValue,
}

#[derive(Deserialize)]
struct BorrowedCaptureContainer<'a> {
    gents_capture_json: u32,
    #[serde(borrow)]
    request_body: &'a RawValue,
    #[serde(borrow)]
    provenance_payload: &'a RawValue,
}

#[derive(Deserialize)]
struct BorrowedEnvelope<'a> {
    gents_lossless_json: u32,
    kind: String,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    value: Option<&'a RawValue>,
    #[serde(default)]
    base: Option<BaseWitness>,
    #[serde(default, borrow)]
    changed: Option<&'a RawValue>,
    #[serde(default)]
    removed: Option<Vec<String>>,
}

fn deserialize_present_raw<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<&'de RawValue>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    <&RawValue>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct BorrowedFieldDelta {
    kind: String,
    #[serde(default, deserialize_with = "deserialize_present_boxed_raw")]
    value: Option<Box<RawValue>>,
    prefix_bytes: Option<u64>,
    suffix_bytes: Option<u64>,
    middle: Option<String>,
}

fn deserialize_present_boxed_raw<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Box<RawValue>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Box::<RawValue>::deserialize(deserializer).map(Some)
}

pub(crate) struct EncodedJson {
    pub(crate) stored: String,
}

pub(crate) fn encode_full(value: &Value) -> Result<EncodedJson> {
    let stored = encode_envelope(Payload::Full {
        value: value.clone(),
    })?;
    let DecodedRecord::Full(decoded) = decode_record(&stored)? else {
        anyhow::bail!("new full capture encoding did not decode as full")
    };
    anyhow::ensure!(
        decoded == *value,
        "new full capture encoding did not preserve the incoming value"
    );
    Ok(EncodedJson { stored })
}

pub(crate) fn encode_against(
    value: &Value,
    base_value: &Value,
    base: BaseWitness,
) -> Result<EncodedJson> {
    let full = encode_full(value)?;
    if base.depth >= MAX_DELTA_DEPTH {
        return Ok(full);
    }
    let (Some(current), Some(previous)) = (value.as_object(), base_value.as_object()) else {
        return Ok(full);
    };
    let mut changed = BTreeMap::new();
    for (key, value) in current {
        if previous.get(key) == Some(value) {
            continue;
        }
        let delta = previous
            .get(key)
            .and_then(|prior| splice_delta(prior, value))
            .unwrap_or_else(|| FieldDelta::Full {
                value: value.clone(),
            });
        changed.insert(key.clone(), delta);
    }
    let removed = previous
        .keys()
        .filter(|key| !current.contains_key(*key))
        .cloned()
        .collect();
    let delta = encode_envelope(Payload::ObjectDelta {
        base,
        changed,
        removed,
    })?;
    if delta.len().saturating_add(MIN_DELTA_SAVINGS) >= full.stored.len() {
        Ok(full)
    } else {
        Ok(EncodedJson { stored: delta })
    }
}

fn encode_envelope(payload: Payload) -> Result<String> {
    super::canonical_json_string(&serde_json::to_value(Envelope {
        gents_lossless_json: LOSSLESS_JSON_VERSION,
        payload,
    })?)
}

pub(crate) fn encode_container(request: &EncodedJson, provenance: &EncodedJson) -> Result<String> {
    let request_body =
        RawValue::from_string(request.stored.clone()).context("decoding request envelope")?;
    let provenance_payload =
        RawValue::from_string(provenance.stored.clone()).context("decoding provenance envelope")?;
    serde_json::to_string(&RawCaptureContainer {
        gents_capture_json: CAPTURE_CONTAINER_VERSION,
        request_body: &request_body,
        provenance_payload: &provenance_payload,
    })
    .context("encoding capture container")
}

fn select_container_record(stored: &str, kind: CapturePayloadKind) -> Result<String> {
    let container: BorrowedCaptureContainer<'_> =
        serde_json::from_str(stored).context("decoding capture container")?;
    anyhow::ensure!(
        container.gents_capture_json == CAPTURE_CONTAINER_VERSION,
        "unsupported capture container version {}",
        container.gents_capture_json
    );
    let envelope = match kind {
        CapturePayloadKind::RequestBody => container.request_body,
        CapturePayloadKind::ProvenancePayload => container.provenance_payload,
    };
    Ok(envelope.get().to_owned())
}

pub(crate) fn decode_capture_record(
    capture_version: u32,
    stored: &str,
    kind: CapturePayloadKind,
) -> Result<DecodedRecord> {
    match capture_version {
        1 if kind == CapturePayloadKind::RequestBody => decode_versioned_record(1, stored),
        1 => anyhow::bail!("legacy provenance payload lives in provenance_json"),
        2 => decode_record(&select_container_record(stored, kind)?),
        other => anyhow::bail!("unsupported rendered-request capture version {other}"),
    }
}

pub(crate) fn capture_record_depth(
    capture_version: u32,
    stored: &str,
    kind: CapturePayloadKind,
) -> Result<u8> {
    Ok(
        match decode_capture_record(capture_version, stored, kind)? {
            DecodedRecord::Legacy(_) | DecodedRecord::Full(_) => 0,
            DecodedRecord::Delta { base, .. } => base
                .depth
                .checked_add(1)
                .context("capture delta depth overflow")?,
        },
    )
}

fn splice_delta(base: &Value, value: &Value) -> Option<FieldDelta> {
    let base = super::canonical_json_string(base).ok()?;
    let value = super::canonical_json_string(value).ok()?;
    let prefix_bytes = common_prefix_boundary(&base, &value);
    let suffix_bytes = common_suffix_boundary(&base[prefix_bytes..], &value[prefix_bytes..]);
    let middle = value[prefix_bytes..value.len() - suffix_bytes].to_owned();
    let splice = FieldDelta::Splice {
        prefix_bytes: u64::try_from(prefix_bytes).ok()?,
        suffix_bytes: u64::try_from(suffix_bytes).ok()?,
        middle,
    };
    (serde_json::to_vec(&splice).ok()?.len() < value.len()).then_some(splice)
}

fn common_prefix_boundary(left: &str, right: &str) -> usize {
    let mut prefix = left
        .bytes()
        .zip(right.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    while !left.is_char_boundary(prefix) || !right.is_char_boundary(prefix) {
        prefix -= 1;
    }
    prefix
}

fn common_suffix_boundary(left: &str, right: &str) -> usize {
    let mut suffix = left
        .bytes()
        .rev()
        .zip(right.bytes().rev())
        .take_while(|(left, right)| left == right)
        .count();
    while !left.is_char_boundary(left.len() - suffix)
        || !right.is_char_boundary(right.len() - suffix)
    {
        suffix -= 1;
    }
    suffix
}

pub(crate) enum DecodedRecord {
    Legacy(Value),
    Full(Value),
    Delta {
        base: BaseWitness,
        changed: BTreeMap<String, FieldDelta>,
        removed: Vec<String>,
    },
}

pub(crate) fn decode_record(stored: &str) -> Result<DecodedRecord> {
    let envelope: BorrowedEnvelope<'_> =
        serde_json::from_str(stored).context("decoding lossless envelope")?;
    anyhow::ensure!(
        envelope.gents_lossless_json == LOSSLESS_JSON_VERSION,
        "unsupported lossless JSON version {}",
        envelope.gents_lossless_json
    );
    match envelope.kind.as_str() {
        "full" => Ok(DecodedRecord::Full(
            serde_json::from_str(
                envelope
                    .value
                    .context("full capture envelope lacks value")?
                    .get(),
            )
            .context("decoding full capture value")?,
        )),
        "object_delta" => Ok(DecodedRecord::Delta {
            base: envelope.base.context("capture delta lacks base witness")?,
            changed: decode_changed_fields(
                envelope
                    .changed
                    .context("capture delta lacks changed fields")?
                    .get(),
            )?,
            removed: envelope
                .removed
                .context("capture delta lacks removed fields")?,
        }),
        other => anyhow::bail!("unsupported lossless JSON payload kind {other}"),
    }
}

fn decode_changed_fields(stored: &str) -> Result<BTreeMap<String, FieldDelta>> {
    let raw: BTreeMap<String, Box<RawValue>> =
        serde_json::from_str(stored).context("decoding capture delta field map")?;
    raw.into_iter()
        .map(|(key, raw)| {
            let field: BorrowedFieldDelta = serde_json::from_str(raw.get())
                .with_context(|| format!("decoding capture delta field {key}"))?;
            let delta = match field.kind.as_str() {
                "full" => FieldDelta::Full {
                    value: serde_json::from_str(
                        field.value.context("full delta field lacks value")?.get(),
                    )
                    .with_context(|| format!("decoding full delta field value {key}"))?,
                },
                "splice" => FieldDelta::Splice {
                    prefix_bytes: field
                        .prefix_bytes
                        .context("splice delta field lacks prefix_bytes")?,
                    suffix_bytes: field
                        .suffix_bytes
                        .context("splice delta field lacks suffix_bytes")?,
                    middle: field.middle.context("splice delta field lacks middle")?,
                },
                other => anyhow::bail!("unsupported capture field delta kind {other}"),
            };
            Ok((key, delta))
        })
        .collect()
}

pub(crate) fn decode_versioned_record(capture_version: u32, stored: &str) -> Result<DecodedRecord> {
    match capture_version {
        1 => serde_json::from_str(stored)
            .map(DecodedRecord::Legacy)
            .context("decoding legacy capture JSON"),
        2 => decode_record(stored),
        other => anyhow::bail!("unsupported rendered-request capture version {other}"),
    }
}

pub(crate) fn decode_inline_value(capture_version: u32, stored: &str) -> Result<Value> {
    match decode_versioned_record(capture_version, stored)? {
        DecodedRecord::Legacy(value) | DecodedRecord::Full(value) => Ok(value),
        DecodedRecord::Delta { .. } => anyhow::bail!("capture delta requires base resolution"),
    }
}

pub(crate) fn resolve_with<F>(capture_version: u32, stored: &str, fetch_base: F) -> Result<Value>
where
    F: FnMut(&BaseWitness) -> Result<(u32, String, String)>,
{
    resolve_with_limit(capture_version, stored, MAX_DELTA_DEPTH, fetch_base)
}

pub(crate) fn resolve_with_limit<F>(
    capture_version: u32,
    stored: &str,
    max_depth: u8,
    mut fetch_base: F,
) -> Result<Value>
where
    F: FnMut(&BaseWitness) -> Result<(u32, String, String)>,
{
    let mut version = capture_version;
    let mut encoded = stored.to_owned();
    let mut pending = Vec::new();
    let mut visited = BTreeSet::new();
    let base = loop {
        match decode_versioned_record(version, &encoded)? {
            DecodedRecord::Legacy(value) | DecodedRecord::Full(value) => break value,
            DecodedRecord::Delta {
                base,
                changed,
                removed,
            } => {
                anyhow::ensure!(
                    pending.len() < usize::from(max_depth),
                    "capture delta chain exceeds maximum depth"
                );
                anyhow::ensure!(
                    visited.insert((base.doc_id.clone(), base.field_commit_cid.clone())),
                    "capture delta chain contains a cycle"
                );
                let (base_version, base_stored, actual_commit) = fetch_base(&base)?;
                anyhow::ensure!(
                    actual_commit == base.field_commit_cid,
                    "capture delta base field commit changed"
                );
                pending.push((changed, removed));
                version = base_version;
                encoded = base_stored;
            }
        }
    };
    pending
        .into_iter()
        .rev()
        .try_fold(base, |value, (changed, removed)| {
            apply_delta(value, changed, removed)
        })
}

pub(crate) fn resolve_capture_with<F>(
    capture_version: u32,
    stored: &str,
    kind: CapturePayloadKind,
    mut fetch_base: F,
) -> Result<Value>
where
    F: FnMut(&BaseWitness) -> Result<(u32, String, String)>,
{
    match capture_version {
        1 if kind == CapturePayloadKind::RequestBody => decode_inline_value(1, stored),
        1 => anyhow::bail!("legacy provenance payload lives in provenance_json"),
        2 => {
            let selected = select_container_record(stored, kind)?;
            resolve_with(2, &selected, |base| {
                let (version, container, commit) = fetch_base(base)?;
                anyhow::ensure!(version == 2, "v2 delta base is not a v2 capture");
                Ok((2, select_container_record(&container, kind)?, commit))
            })
        }
        other => anyhow::bail!("unsupported rendered-request capture version {other}"),
    }
}

pub(crate) fn apply_delta(
    base_value: Value,
    changed: BTreeMap<String, FieldDelta>,
    removed: Vec<String>,
) -> Result<Value> {
    let mut object = base_value
        .as_object()
        .cloned()
        .context("lossless delta base is not an object")?;
    for key in removed {
        object.remove(&key);
    }
    for (key, delta) in changed {
        let value = match delta {
            FieldDelta::Full { value } => value,
            FieldDelta::Splice {
                prefix_bytes,
                suffix_bytes,
                middle,
            } => {
                let prefix_bytes = usize::try_from(prefix_bytes)
                    .context("lossless splice prefix exceeds platform size")?;
                let suffix_bytes = usize::try_from(suffix_bytes)
                    .context("lossless splice suffix exceeds platform size")?;
                let base = object.get(&key).context("splice base field is missing")?;
                let base = super::canonical_json_string(base)?;
                let retained = prefix_bytes
                    .checked_add(suffix_bytes)
                    .context("lossless splice bounds overflow")?;
                anyhow::ensure!(
                    retained <= base.len()
                        && base.is_char_boundary(prefix_bytes)
                        && base.is_char_boundary(base.len() - suffix_bytes),
                    "invalid lossless splice bounds"
                );
                let capacity = prefix_bytes
                    .checked_add(middle.len())
                    .and_then(|value| value.checked_add(suffix_bytes))
                    .context("lossless splice capacity overflow")?;
                let mut reconstructed = String::with_capacity(capacity);
                reconstructed.push_str(&base[..prefix_bytes]);
                reconstructed.push_str(&middle);
                reconstructed.push_str(&base[base.len() - suffix_bytes..]);
                serde_json::from_str(&reconstructed).context("decoding spliced JSON field")?
            }
        };
        object.insert(key, value);
    }
    Ok(Value::Object(object))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nested_json(depth: usize) -> String {
        format!("{}0{}", "{\"n\":".repeat(depth), "}".repeat(depth))
    }

    #[test]
    fn generated_storage_cases_drive_the_lossless_codec() {
        let cases = crate::lean_vocab_test::lean_rendered_capture_storage_cases();
        assert_eq!(
            cases.len(),
            5,
            "Lean storage cases changed without a Rust fence"
        );
        for case in cases {
            let request = json!({"request": case.request, "payload": "x".repeat(2048)});
            let resolved = match case.encoding.as_str() {
                "legacy_full" => resolve_with_limit(
                    1,
                    &request.to_string(),
                    case.max_depth as u8,
                    |_| unreachable!(),
                ),
                "full" => {
                    let full = encode_full(&request).unwrap();
                    resolve_with_limit(2, &full.stored, case.max_depth as u8, |_| unreachable!())
                }
                "delta" => {
                    let base_value = json!({"request": 0, "payload": "x".repeat(2048)});
                    let base_full = encode_full(&base_value).unwrap();
                    let expected = case.base_witness.expect("delta witness").to_string();
                    let delta = encode_against(
                        &request,
                        &base_value,
                        BaseWitness {
                            doc_id: "base-doc".into(),
                            field_commit_cid: expected.clone(),
                            depth: case.base_depth as u8,
                            agent_did: "agent".into(),
                            requester_did: String::new(),
                            session_id: "session".into(),
                            source: "source".into(),
                            capture_scope: "inference.1".into(),
                        },
                    )
                    .unwrap();
                    resolve_with_limit(2, &delta.stored, case.max_depth as u8, |_| {
                        Ok((
                            2,
                            base_full.stored.clone(),
                            if case.base_verified {
                                expected.clone()
                            } else {
                                "changed".into()
                            },
                        ))
                    })
                }
                other => panic!("unknown Lean encoding {other}"),
            };
            assert_eq!(resolved.is_ok(), case.send_permitted, "{}", case.name);
            assert_eq!(
                resolved.ok().and_then(|value| value["request"].as_u64()),
                case.decoded_request,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn legacy_full_and_delta_round_trip_losslessly() {
        let base = json!({"max_tokens": 100, "messages": ["α", "b"], "tools": [1, 2]});
        let next = json!({"max_tokens": 200, "messages": ["α", "b", "c"], "tools": [1, 2]});
        assert_eq!(
            match decode_versioned_record(1, &super::super::canonical_json_string(&base).unwrap(),)
                .unwrap()
            {
                DecodedRecord::Legacy(value) => value,
                _ => panic!("legacy value was reinterpreted"),
            },
            base
        );
        let encoded = encode_against(
            &next,
            &base,
            BaseWitness {
                doc_id: "base".into(),
                field_commit_cid: "bafy".into(),
                depth: 0,
                agent_did: "agent".into(),
                requester_did: String::new(),
                session_id: "session".into(),
                source: "source".into(),
                capture_scope: "inference.1".into(),
            },
        )
        .unwrap();
        let decoded = match decode_record(&encoded.stored).unwrap() {
            DecodedRecord::Full(value) => value,
            DecodedRecord::Delta {
                changed, removed, ..
            } => apply_delta(base, changed, removed).unwrap(),
            DecodedRecord::Legacy(_) => panic!("new value lacked an envelope"),
        };
        assert_eq!(decoded, next);
    }

    #[test]
    fn full_delta_and_container_preserve_json_number_spellings() {
        let base: Value = serde_json::from_str(
            r#"{"integer":9007199254740991,"negative":-17,"decimal":1.25,"exponent":1e+20,"subnormal":5e-324,"extreme":1.7976931348623157e308,"awkward":1.2345678901234567,"payload":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}"#,
        )
        .unwrap();
        let mut next = base.clone();
        next["negative"] = json!(-18);
        next["payload"] = json!(format!("{}tail", "x".repeat(300)));
        let encoded = encode_against(
            &next,
            &base,
            BaseWitness {
                doc_id: "base".into(),
                field_commit_cid: "cid".into(),
                depth: 0,
                agent_did: "agent".into(),
                requester_did: String::new(),
                session_id: "session".into(),
                source: "source".into(),
                capture_scope: "inference.1".into(),
            },
        )
        .unwrap();
        let container = encode_container(&encoded, &encode_full(&next).unwrap()).unwrap();
        let resolved = resolve_capture_with(2, &container, CapturePayloadKind::RequestBody, |_| {
            let base_container =
                encode_container(&encode_full(&base).unwrap(), &encode_full(&base).unwrap())
                    .unwrap();
            Ok((2, base_container, "cid".into()))
        })
        .unwrap();
        assert_eq!(resolved, next);
        assert_eq!(resolved["integer"].as_u64(), Some(9_007_199_254_740_991));
        assert_eq!(resolved["negative"].as_i64(), Some(-18));
        assert_eq!(resolved["decimal"].as_f64(), Some(1.25));
        assert_eq!(resolved["exponent"].as_f64(), Some(1e20));
        assert_eq!(resolved["subnormal"].as_f64(), Some(f64::from_bits(1)));
        assert_eq!(resolved["extreme"].as_f64(), Some(f64::MAX));
        assert_eq!(resolved["awkward"].as_f64(), Some(1.234_567_890_123_456_7));
    }

    #[test]
    fn storage_wrappers_preserve_the_original_json_nesting_budget() {
        let null = encode_full(&Value::Null).expect("JSON null is a present full value");
        assert!(matches!(
            decode_record(&null.stored).unwrap(),
            DecodedRecord::Full(Value::Null)
        ));
        for depth in [126, 127] {
            let mut base: Value = serde_json::from_str(&nested_json(depth)).unwrap();
            base.as_object_mut()
                .unwrap()
                .insert("padding".into(), json!("x".repeat(2048)));
            base.as_object_mut()
                .unwrap()
                .insert("turn".into(), json!(0));
            let mut next = base.clone();
            next["turn"] = json!(1);

            let full = encode_full(&next).expect("baseline-readable full value");
            let full_container =
                encode_container(&full, &encode_full(&json!({})).unwrap()).unwrap();
            assert_eq!(
                resolve_capture_with(
                    2,
                    &full_container,
                    CapturePayloadKind::RequestBody,
                    |_| unreachable!()
                )
                .unwrap(),
                next
            );

            let delta = encode_against(
                &next,
                &base,
                BaseWitness {
                    doc_id: "base".into(),
                    field_commit_cid: "cid".into(),
                    depth: 0,
                    agent_did: "agent".into(),
                    requester_did: String::new(),
                    session_id: "session".into(),
                    source: "source".into(),
                    capture_scope: "inference.1".into(),
                },
            )
            .unwrap();
            assert!(matches!(
                decode_record(&delta.stored).unwrap(),
                DecodedRecord::Delta { .. }
            ));
            let delta_container =
                encode_container(&delta, &encode_full(&json!({})).unwrap()).unwrap();
            let base_container = encode_container(
                &encode_full(&base).unwrap(),
                &encode_full(&json!({})).unwrap(),
            )
            .unwrap();
            assert_eq!(
                resolve_capture_with(
                    2,
                    &delta_container,
                    CapturePayloadKind::RequestBody,
                    |_| Ok((2, base_container.clone(), "cid".into()))
                )
                .unwrap(),
                next
            );
        }

        let unsupported = serde_json::from_str::<Value>(&nested_json(128));
        assert!(
            unsupported.is_err(),
            "storage must retain serde_json's bound"
        );

        let mut deep: Value = serde_json::from_str(&nested_json(126)).unwrap();
        deep.as_object_mut()
            .unwrap()
            .insert("leaf".into(), json!(true));
        let base = json!({"padding":"x".repeat(16 * 1024)});
        let next = json!({"padding":"x".repeat(16 * 1024), "deep":deep});
        let delta = encode_against(
            &next,
            &base,
            BaseWitness {
                doc_id: "base".into(),
                field_commit_cid: "cid".into(),
                depth: 0,
                agent_did: "agent".into(),
                requester_did: String::new(),
                session_id: "session".into(),
                source: "source".into(),
                capture_scope: "inference.1".into(),
            },
        )
        .unwrap();
        assert!(matches!(
            decode_record(&delta.stored).unwrap(),
            DecodedRecord::Delta { .. }
        ));
        assert_eq!(
            resolve_with(2, &delta.stored, |_| {
                Ok((2, encode_full(&base).unwrap().stored, "cid".into()))
            })
            .unwrap(),
            next
        );
    }

    #[test]
    fn realistic_growing_fields_are_smaller_than_repeated_full_values() {
        let messages = (0..120)
            .map(|index| json!({"role":"user","content":"x".repeat(200),"index":index}))
            .collect::<Vec<_>>();
        let tool_results = (0..80)
            .map(|index| json!({"message_index":index,"content":[{"text":"y".repeat(160)}]}))
            .collect::<Vec<_>>();
        let base = json!({"max_tokens":100,"effective_messages":messages,"threaded_tool_results":tool_results});
        let mut next = base.clone();
        next["max_tokens"] = json!(200);
        next["effective_messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"assistant","content":"tail"}));
        next["threaded_tool_results"]
            .as_array_mut()
            .unwrap()
            .push(json!({"message_index":120,"content":[{"text":"tail"}]}));
        let full = encode_full(&next).unwrap();
        let delta = encode_against(
            &next,
            &base,
            BaseWitness {
                doc_id: "base".into(),
                field_commit_cid: "cid".into(),
                depth: 0,
                agent_did: "agent".into(),
                requester_did: String::new(),
                session_id: "session".into(),
                source: "source".into(),
                capture_scope: "inference.1".into(),
            },
        )
        .unwrap();
        match decode_record(&delta.stored).unwrap() {
            DecodedRecord::Delta { changed, .. } => {
                assert!(matches!(
                    changed.get("effective_messages"),
                    Some(FieldDelta::Splice { .. })
                ));
                assert!(matches!(
                    changed.get("threaded_tool_results"),
                    Some(FieldDelta::Splice { .. })
                ));
            }
            _ => panic!("realistic growth should use field splices"),
        }
        assert!(
            delta.stored.len() * 4 < full.stored.len(),
            "{} vs {}",
            delta.stored.len(),
            full.stored.len()
        );
    }

    #[test]
    fn field_splices_preserve_utf8_and_removals() {
        let base = json!({"text": format!("{}tail", "λ".repeat(600)), "removed": true});
        let next = json!({"text": format!("{}new-tail", "λ".repeat(600))});
        let delta = encode_against(
            &next,
            &base,
            BaseWitness {
                doc_id: "base".into(),
                field_commit_cid: "cid".into(),
                depth: 0,
                agent_did: "agent".into(),
                requester_did: String::new(),
                session_id: "session".into(),
                source: "source".into(),
                capture_scope: "inference.1".into(),
            },
        )
        .unwrap();
        let DecodedRecord::Delta {
            changed, removed, ..
        } = decode_record(&delta.stored).unwrap()
        else {
            panic!("expected delta")
        };
        assert_eq!(removed, vec!["removed"]);
        assert_eq!(apply_delta(base, changed, removed).unwrap(), next);
    }

    #[test]
    fn capture_version_strictly_selects_the_storage_format() {
        let colliding_legacy = json!({"gents_lossless_json": 1, "kind": "provider_payload"});
        assert_eq!(
            decode_inline_value(1, &colliding_legacy.to_string()).unwrap(),
            colliding_legacy
        );
        assert!(decode_inline_value(2, r#"{"provider":"body"}"#).is_err());
        assert!(decode_inline_value(3, r#"{"provider":"body"}"#).is_err());
    }

    #[test]
    fn capture_container_keeps_body_and_provenance_independently_lossless() {
        let body = json!({"messages": ["wire body"]});
        let provenance = json!({"effective_messages": ["native message"]});
        let stored = encode_container(
            &encode_full(&body).unwrap(),
            &encode_full(&provenance).unwrap(),
        )
        .unwrap();
        assert_eq!(
            resolve_capture_with(
                2,
                &stored,
                CapturePayloadKind::RequestBody,
                |_| unreachable!()
            )
            .unwrap(),
            body
        );
        assert_eq!(
            resolve_capture_with(2, &stored, CapturePayloadKind::ProvenancePayload, |_| {
                unreachable!()
            })
            .unwrap(),
            provenance
        );

        let mut malformed: Value = serde_json::from_str(&stored).unwrap();
        malformed["request_body"]["gents_lossless_json"] = json!(99);
        assert!(resolve_capture_with(
            2,
            &malformed.to_string(),
            CapturePayloadKind::RequestBody,
            |_| unreachable!()
        )
        .is_err());
    }

    #[test]
    fn malicious_splice_bounds_fail_without_overflow_or_allocation() {
        let changed = BTreeMap::from([(
            "body".to_owned(),
            FieldDelta::Splice {
                prefix_bytes: u64::MAX,
                suffix_bytes: u64::MAX,
                middle: String::new(),
            },
        )]);
        assert!(apply_delta(json!({"body": "small"}), changed, Vec::new()).is_err());
    }

    #[test]
    fn witnessed_chain_rejects_missing_changed_and_cyclic_bases() {
        let witness = BaseWitness {
            doc_id: "base-doc".into(),
            field_commit_cid: "expected-cid".into(),
            depth: 0,
            agent_did: "agent".into(),
            requester_did: String::new(),
            session_id: "session".into(),
            source: "source".into(),
            capture_scope: "inference.1".into(),
        };
        let delta = encode_envelope(Payload::ObjectDelta {
            base: witness.clone(),
            changed: BTreeMap::from([("value".into(), FieldDelta::Full { value: json!(2) })]),
            removed: Vec::new(),
        })
        .unwrap();

        assert!(resolve_with(2, &delta, |_| anyhow::bail!("missing base")).is_err());
        let full = encode_full(&json!({"value": 1})).unwrap();
        assert!(resolve_with(2, &delta, |_| {
            Ok((2, full.stored.clone(), "changed-cid".into()))
        })
        .is_err());
        assert!(resolve_with(2, &delta, |_| {
            Ok((2, delta.clone(), "expected-cid".into()))
        })
        .is_err());
    }
}
