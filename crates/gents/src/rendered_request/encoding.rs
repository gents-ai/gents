//! Versioned, lossless storage encoding for large capture JSON values.
//!
//! Legacy rows store the canonical JSON value directly. New rows may store a
//! full envelope, a delta against an immutable field-commit witness, or a
//! manifest of content-defined byte blocks (#2333). Deltas operate
//! independently on top-level object fields; changed arrays/objects use a byte
//! splice, so an unrelated scalar change does not force a growing message list
//! to be repeated. Manifests split the canonical body bytes once at
//! content-defined boundaries and store each block as its own document, so a
//! later capture repeats only references.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::{write::ZlibEncoder, Compression, Decompress, FlushDecompress, Status};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) const LOSSLESS_JSON_VERSION: u32 = 1;
const CAPTURE_CONTAINER_VERSION: u32 = 1;
pub(crate) const MAX_DELTA_DEPTH: u8 = 8;
const MIN_DELTA_SAVINGS: usize = 256;
const COMPRESSED_JSON_VERSION: u32 = 2;
/// Bounds decompression allocation from replicated input. Larger legitimate
/// captures retain the uncompressed encoding and its existing decode behavior.
const MAX_COMPRESSED_RECORD_BYTES: usize = 64 * 1024 * 1024;
const MIN_COMPRESSION_BYTES: usize = 4096;

#[derive(Serialize, Deserialize)]
struct CompressedEnvelope {
    gents_lossless_json: u32,
    kind: String,
    uncompressed_bytes: usize,
    data: String,
}

fn compress_record(stored: &str) -> Result<String> {
    if !(MIN_COMPRESSION_BYTES..=MAX_COMPRESSED_RECORD_BYTES).contains(&stored.len()) {
        return Ok(stored.to_owned());
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(stored.as_bytes())?;
    let compressed = serde_json::to_string(&CompressedEnvelope {
        gents_lossless_json: COMPRESSED_JSON_VERSION,
        kind: "zlib".into(),
        uncompressed_bytes: stored.len(),
        data: STANDARD.encode(encoder.finish()?),
    })?;
    Ok(
        if compressed.len().saturating_add(MIN_DELTA_SAVINGS) < stored.len() {
            compressed
        } else {
            stored.to_owned()
        },
    )
}

fn decompress_record(stored: &str) -> Result<String> {
    let envelope: CompressedEnvelope = serde_json::from_str(stored)?;
    anyhow::ensure!(
        envelope.gents_lossless_json == COMPRESSED_JSON_VERSION && envelope.kind == "zlib",
        "unsupported compressed capture encoding"
    );
    anyhow::ensure!(
        envelope.uncompressed_bytes <= MAX_COMPRESSED_RECORD_BYTES,
        "compressed capture exceeds decoded byte bound"
    );
    anyhow::ensure!(
        envelope.data.len() <= MAX_COMPRESSED_RECORD_BYTES.div_ceil(3) * 4,
        "compressed capture exceeds encoded byte bound"
    );
    let bytes = STANDARD
        .decode(&envelope.data)
        .context("decoding compressed capture base64")?;
    let mut decoder = Decompress::new(true);
    let mut decoded = Vec::with_capacity(envelope.uncompressed_bytes + 1);
    let status = decoder
        .decompress_vec(&bytes, &mut decoded, FlushDecompress::Finish)
        .context("decompressing capture record")?;
    anyhow::ensure!(
        status == Status::StreamEnd,
        "compressed capture is incomplete or exceeds its byte bound"
    );
    anyhow::ensure!(
        decoded.len() == envelope.uncompressed_bytes,
        "compressed capture length mismatch"
    );
    anyhow::ensure!(
        decoder.total_in() == bytes.len() as u64,
        "compressed capture has trailing bytes"
    );
    String::from_utf8(decoded).context("compressed capture is not UTF-8")
}

/// Content-defined chunking parameters for v3 manifest captures (#2333):
/// boundaries are decided no closer than [`CHUNK_MIN_SIZE`] (2 KiB), no
/// further than [`CHUNK_MAX_SIZE`] (16 KiB), with a cut expected roughly every
/// `2^11` candidate positions past the minimum (mean chunk ≈ 4 KiB). The
/// boundary test is a fixed polynomial rolling hash over a
/// [`CHUNK_WINDOW_SIZE`]-byte window with a fixed multiplier, so chunking is
/// deterministic and platform-stable for identical bytes; no keyed or
/// randomized hash is involved.
const CHUNK_MIN_SIZE: usize = 2 * 1024;
const CHUNK_MAX_SIZE: usize = 16 * 1024;
const CHUNK_WINDOW_SIZE: usize = 48;
const CHUNK_BOUNDARY_MASK: u64 = (1 << 11) - 1;
const CHUNK_HASH_MULTIPLIER: u64 = 0x9E37_79B9_7F4A_7C15;

/// `CHUNK_HASH_MULTIPLIER^(CHUNK_WINDOW_SIZE - 1)` (mod 2^64): the weight of
/// the byte leaving the sliding window when the hash rolls.
const CHUNK_HASH_ROLLOUT: u64 = chunk_hash_power(CHUNK_HASH_MULTIPLIER, CHUNK_WINDOW_SIZE - 1);

const fn chunk_hash_power(mut base: u64, mut exponent: usize) -> u64 {
    let mut result = 1u64;
    while exponent > 0 {
        if exponent & 1 == 1 {
            result = result.wrapping_mul(base);
        }
        base = base.wrapping_mul(base);
        exponent >>= 1;
    }
    result
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BaseWitness {
    pub(crate) doc_id: String,
    pub(crate) field_commit_cid: String,
    pub(crate) depth: u8,
    pub(crate) node_did: String,
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

/// One ordered block reference inside a manifest record (#2333).
///
/// `content_key` is the writer-computed sha256 of the block bytes — a dedupe
/// and lookup key only. Integrity rides on `field_commit_cid`, the immutable
/// DefraDB field commit of the block document's content pinned at write time
/// and re-checked at read time; a reader must never trust the key over the
/// commit. `byte_len` lets resolution fail closed on short or over-long reads
/// instead of silently reassembling truncated bodies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManifestEntry {
    pub(crate) doc_id: String,
    pub(crate) content_key: String,
    pub(crate) field_commit_cid: String,
    pub(crate) byte_len: u64,
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
    Manifest {
        blocks: Vec<ManifestEntry>,
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
    #[serde(default, borrow)]
    blocks: Option<&'a RawValue>,
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

/// Test-only constructor for v2 full records. The durable writer stores every
/// payload as a manifest (#2333), but rows already stored as full records must
/// keep decoding forever, and the fences that pin that decode need to build
/// real full records. Both it and the returned record keep the decode-what-you-
/// wrote self-check the manifest writer mirrors.
#[cfg(test)]
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

/// Test-only constructor for v2 delta records. The durable writer stopped
/// producing deltas in #2333 — new captures are manifests — but rows already
/// stored as deltas must keep decoding forever, and the fences that pin that
/// decode need to build real delta records.
#[cfg(test)]
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

/// One content-defined block of a capture body, as the write path produces it.
pub(crate) struct ChunkedBlock {
    /// sha256 of `bytes`, hex-encoded. A dedupe/lookup key only; integrity
    /// rides on the field-commit witness the stored entry pins.
    pub(crate) content_key: String,
    pub(crate) bytes: Vec<u8>,
}

/// The write-side v3 policy: split canonical capture bytes into content-defined
/// blocks (#2333). Every cut lands on a UTF-8 character boundary.
///
/// The chunker decides boundaries, never content: reconstruction is pure
/// concatenation, so any partition is lossless and the chunker can change
/// without touching readability. Because a boundary depends only on the
/// trailing [`CHUNK_WINDOW_SIZE`] bytes, an insertion or deletion in the middle
/// of a body re-synchronizes boundaries after the edit — the tail chunks of an
/// edited body are byte-identical to the original's, which is what makes block
/// reuse O(edit) instead of O(body). Determinism is load-bearing: the fixed
/// multiplier and mask make identical bytes chunk identically on every
/// platform and build.
pub(crate) fn chunk_capture_body(canonical: &str) -> Vec<ChunkedBlock> {
    let bytes = canonical.as_bytes();
    let mut blocks = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let end = start + next_chunk_end(&bytes[start..]);
        let chunk = &bytes[start..end];
        blocks.push(ChunkedBlock {
            content_key: block_content_key(chunk),
            bytes: chunk.to_vec(),
        });
        start = end;
    }
    blocks
}

/// Length of the first chunk cut from `chunk`: a content-defined boundary
/// after [`CHUNK_MIN_SIZE`] bytes, a forced cut at [`CHUNK_MAX_SIZE`], or the
/// end of the body. A final block may be shorter than the minimum.
///
/// Cuts are advanced to the next UTF-8 character boundary, so a block carved
/// out of a valid UTF-8 body is itself valid UTF-8 and stores byte for byte in
/// the block document's String payload; a cut may therefore exceed its nominal
/// limit by at most three bytes.
fn next_chunk_end(chunk: &[u8]) -> usize {
    let limit = CHUNK_MAX_SIZE.min(chunk.len());
    if limit <= CHUNK_MIN_SIZE {
        return chunk.len();
    }
    let mut hash = chunk_window_hash(&chunk[CHUNK_MIN_SIZE - CHUNK_WINDOW_SIZE..CHUNK_MIN_SIZE]);
    let mut position = CHUNK_MIN_SIZE;
    while position < limit {
        if hash & CHUNK_BOUNDARY_MASK == 0 {
            return utf8_chunk_boundary(chunk, position);
        }
        hash = roll_chunk_hash(hash, chunk[position - CHUNK_WINDOW_SIZE], chunk[position]);
        position += 1;
    }
    utf8_chunk_boundary(chunk, limit)
}

/// The next UTF-8 character boundary at or after `index`.
fn utf8_chunk_boundary(bytes: &[u8], index: usize) -> usize {
    let mut boundary = index;
    while boundary < bytes.len() && (bytes[boundary] & 0xC0) == 0x80 {
        boundary += 1;
    }
    boundary
}

/// Polynomial hash of a full window: `Σ (byte+1) · multiplier^(window-1-i)`,
/// evaluated with wrapping u64 arithmetic.
fn chunk_window_hash(window: &[u8]) -> u64 {
    debug_assert_eq!(window.len(), CHUNK_WINDOW_SIZE);
    window.iter().fold(0u64, |hash, byte| {
        hash.wrapping_mul(CHUNK_HASH_MULTIPLIER)
            .wrapping_add(u64::from(*byte) + 1)
    })
}

/// Slide the window hash by one byte: drop `outgoing`'s term, multiply every
/// remaining power up by one, and add `incoming`'s term.
fn roll_chunk_hash(hash: u64, outgoing: u8, incoming: u8) -> u64 {
    hash.wrapping_sub((u64::from(outgoing) + 1).wrapping_mul(CHUNK_HASH_ROLLOUT))
        .wrapping_mul(CHUNK_HASH_MULTIPLIER)
        .wrapping_add(u64::from(incoming) + 1)
}

/// sha256 over raw block bytes, hex-encoded.
pub(crate) fn block_content_key(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Durable reader ceiling on one manifest's block count: the fuel the store
/// resolver starts with, minus the manifest document and the terminal list
/// frame (`k` full blocks first resolve at fuel `k + 2`). This is the Rust
/// reading of the fuel parameter `RenderedCapture.resolveBytes` threads
/// through every resolution, not a second policy: a manifest beyond it fails
/// closed exactly like an over-fuel Lean resolution, and the writer refuses to
/// produce one. Blocks span 2048..16387 bytes (the 16 KiB forced cut may
/// overshoot by up to three UTF-8 continuation bytes), so the ceiling decodes
/// a body of at least ~20 MiB when every cut lands at the minimum, against
/// ~40 MiB at the ~4 KiB mean cut — both far above any context window.
pub(crate) const MAX_MANIFEST_BLOCKS: usize = 10_000;

/// Fuel for durable manifest resolution: `MAX_MANIFEST_BLOCKS` blocks plus the
/// manifest document and terminal list frame.
pub(crate) const MANIFEST_RESOLUTION_FUEL: usize = MAX_MANIFEST_BLOCKS + 2;

/// Encode the manifest record for already-stored blocks. The entries' document
/// ids and field-commit witnesses come from the block writes that precede this
/// call.
pub(crate) fn encode_manifest(blocks: &[ManifestEntry]) -> Result<EncodedJson> {
    anyhow::ensure!(
        blocks.len() <= MAX_MANIFEST_BLOCKS,
        "capture manifest names {} blocks, above the {MAX_MANIFEST_BLOCKS} the reader resolves",
        blocks.len()
    );
    Ok(EncodedJson {
        stored: encode_envelope(Payload::Manifest {
            blocks: blocks.to_vec(),
        })?,
    })
}

/// Resolve a manifest record into its capture value through a block source.
///
/// `fetch_block` supplies the block document's bytes and its *actual* field
/// commit; resolution fails closed when the source is missing, the commit no
/// longer equals the pinned witness, the bytes do not match the pinned length,
/// or fuel runs out. `fuel` mirrors the Lean owner's one shared budget
/// (`RenderedCapture.resolveBytes`/`resolveBlocks`): the manifest document,
/// every entry (with its block document), and the terminal list frame each
/// consume one unit, so a manifest of `k` full blocks first resolves at fuel
/// `k + 2` and anything less fails closed.
pub(crate) fn resolve_manifest_with_limit<F>(
    fuel: usize,
    stored: &str,
    mut fetch_block: F,
) -> Result<Value>
where
    F: FnMut(&ManifestEntry) -> Result<(Vec<u8>, String)>,
{
    let blocks = match decode_record(stored)? {
        DecodedRecord::Manifest { blocks } => blocks,
        _ => anyhow::bail!("capture record is not a block manifest"),
    };
    let mut remaining = fuel;
    let mut assembled = Vec::new();
    anyhow::ensure!(remaining > 0, "capture manifest exhausted resolution fuel");
    remaining -= 1;
    for entry in &blocks {
        anyhow::ensure!(remaining > 0, "capture manifest exhausted resolution fuel");
        remaining -= 1;
        let (bytes, actual_commit) = fetch_block(entry)?;
        anyhow::ensure!(
            actual_commit == entry.field_commit_cid,
            "capture manifest block field commit changed"
        );
        let expected_len = usize::try_from(entry.byte_len)
            .context("capture manifest block length exceeds platform size")?;
        anyhow::ensure!(
            bytes.len() == expected_len,
            "capture manifest block length mismatch"
        );
        assembled.extend_from_slice(&bytes);
    }
    anyhow::ensure!(remaining > 0, "capture manifest exhausted resolution fuel");
    serde_json::from_slice(&assembled).context("decoding manifest-reassembled capture JSON")
}

pub(crate) fn encode_container(request: &EncodedJson, provenance: &EncodedJson) -> Result<String> {
    let request_body = RawValue::from_string(compress_record(&request.stored)?)
        .context("decoding request envelope")?;
    let provenance_payload = RawValue::from_string(compress_record(&provenance.stored)?)
        .context("decoding provenance envelope")?;
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
        2 | 3 => decode_record(&select_container_record(stored, kind)?),
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
            DecodedRecord::Legacy(_) | DecodedRecord::Full(_) | DecodedRecord::Manifest { .. } => 0,
            DecodedRecord::Delta { base, .. } => base
                .depth
                .checked_add(1)
                .context("capture delta depth overflow")?,
        },
    )
}

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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
    Manifest {
        blocks: Vec<ManifestEntry>,
    },
}

pub(crate) fn decode_record(stored: &str) -> Result<DecodedRecord> {
    let envelope: BorrowedEnvelope<'_> =
        serde_json::from_str(stored).context("decoding lossless envelope")?;
    if envelope.gents_lossless_json == COMPRESSED_JSON_VERSION {
        let decoded = decompress_record(stored)?;
        let inner: BorrowedEnvelope<'_> = serde_json::from_str(&decoded)?;
        anyhow::ensure!(
            inner.gents_lossless_json == LOSSLESS_JSON_VERSION,
            "compressed capture must contain an uncompressed lossless record"
        );
        return decode_record(&decoded);
    }
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
        "manifest" => Ok(DecodedRecord::Manifest {
            blocks: serde_json::from_str(
                envelope
                    .blocks
                    .context("capture manifest lacks block references")?
                    .get(),
            )
            .context("decoding capture manifest block references")?,
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
        2 | 3 => decode_record(stored),
        other => anyhow::bail!("unsupported rendered-request capture version {other}"),
    }
}

pub(crate) fn decode_inline_value(capture_version: u32, stored: &str) -> Result<Value> {
    match decode_versioned_record(capture_version, stored)? {
        DecodedRecord::Legacy(value) | DecodedRecord::Full(value) => Ok(value),
        DecodedRecord::Delta { .. } => anyhow::bail!("capture delta requires base resolution"),
        DecodedRecord::Manifest { .. } => {
            anyhow::bail!("capture manifest requires block resolution")
        }
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
            DecodedRecord::Manifest { .. } => {
                anyhow::bail!("capture manifest resolution requires a block source")
            }
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

pub(crate) fn resolve_capture_with<B, F>(
    capture_version: u32,
    stored: &str,
    kind: CapturePayloadKind,
    mut fetch_base: F,
    mut fetch_block: B,
) -> Result<Value>
where
    F: FnMut(&BaseWitness) -> Result<(u32, String, String)>,
    B: FnMut(&ManifestEntry) -> Result<(Vec<u8>, String)>,
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
        // A v3 row stores both container payloads as manifests; the writer
        // never stores any other record kind under version 3.
        3 => {
            let selected = select_container_record(stored, kind)?;
            match decode_record(&selected)? {
                DecodedRecord::Manifest { .. } => {
                    resolve_manifest_with_limit(MANIFEST_RESOLUTION_FUEL, &selected, |entry| {
                        fetch_block(entry)
                    })
                }
                _ => anyhow::bail!("v3 capture record is not a block manifest"),
            }
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

    #[test]
    fn compressed_records_preserve_full_delta_and_commit_witnesses() {
        let base = json!({"messages": (0..400).map(|i| format!("message {i}: {}", "payload ".repeat(20))).collect::<Vec<_>>()});
        let mut next = base.clone();
        next["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!("new payload ".repeat(1000)));
        let witness = BaseWitness {
            doc_id: "base".into(),
            field_commit_cid: "cid".into(),
            depth: 0,
            node_did: "agent".into(),
            requester_did: String::new(),
            session_id: "session".into(),
            source: "source".into(),
            capture_scope: "inference.1".into(),
        };
        let base_full = encode_full(&base).unwrap();
        let full_container = encode_container(&base_full, &base_full).unwrap();
        assert!(full_container.len() < base_full.stored.len() / 4);
        assert_eq!(
            capture_record_depth(2, &full_container, CapturePayloadKind::RequestBody).unwrap(),
            0
        );
        let delta = encode_against(&next, &base, witness).unwrap();
        assert!(matches!(
            decode_record(&delta.stored).unwrap(),
            DecodedRecord::Delta { .. }
        ));
        let container = encode_container(&delta, &base_full).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&container).unwrap()["request_body"]
                ["gents_lossless_json"],
            COMPRESSED_JSON_VERSION
        );
        assert_eq!(
            capture_record_depth(2, &container, CapturePayloadKind::RequestBody).unwrap(),
            1
        );
        assert_eq!(
            resolve_capture_with(
                2,
                &container,
                CapturePayloadKind::RequestBody,
                |_| Ok((2, full_container.clone(), "cid".into())),
                |_| unreachable!()
            )
            .unwrap(),
            next
        );
        assert!(resolve_capture_with(
            2,
            &container,
            CapturePayloadKind::RequestBody,
            |_| Ok((2, full_container.clone(), "wrong".into())),
            |_| unreachable!()
        )
        .is_err());
        assert_eq!(
            resolve_capture_with(
                2,
                &container,
                CapturePayloadKind::ProvenancePayload,
                |_| unreachable!(),
                |_| unreachable!()
            )
            .unwrap(),
            base
        );
    }

    #[test]
    fn compression_preserves_unbounded_plain_capture_support() {
        let large = "x".repeat(MAX_COMPRESSED_RECORD_BYTES + 1);
        assert_eq!(compress_record(&large).unwrap(), large);
        let small = encode_full(&json!({"message": "tiny"})).unwrap();
        assert_eq!(compress_record(&small.stored).unwrap(), small.stored);
    }

    #[test]
    fn compressed_records_reject_corruption_expansion_and_nesting() {
        let full = encode_full(&json!({"unicode": "αβγ🙂".repeat(4096)})).unwrap();
        let compressed = compress_record(&full.stored).unwrap();
        assert!(compressed.len() < full.stored.len());
        assert_eq!(decompress_record(&compressed).unwrap(), full.stored);
        let valid: Value = serde_json::from_str(&compressed).unwrap();
        for size in [
            0,
            full.stored.len() - 1,
            full.stored.len() + 1,
            MAX_COMPRESSED_RECORD_BYTES + 1,
        ] {
            let mut invalid = valid.clone();
            invalid["uncompressed_bytes"] = json!(size);
            assert!(decode_record(&invalid.to_string()).is_err(), "size {size}");
        }
        for bytes in [
            vec![0, 1, 2],
            {
                let mut v = STANDARD.decode(valid["data"].as_str().unwrap()).unwrap();
                v.pop();
                v
            },
            {
                let mut v = STANDARD.decode(valid["data"].as_str().unwrap()).unwrap();
                v.push(0);
                v
            },
            {
                let mut v = STANDARD.decode(valid["data"].as_str().unwrap()).unwrap();
                *v.last_mut().unwrap() ^= 1;
                v
            },
        ] {
            let mut invalid = valid.clone();
            invalid["data"] = json!(STANDARD.encode(bytes));
            assert!(decode_record(&invalid.to_string()).is_err());
        }
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(compressed.as_bytes()).unwrap();
        let nested = serde_json::to_string(&CompressedEnvelope {
            gents_lossless_json: COMPRESSED_JSON_VERSION,
            kind: "zlib".into(),
            uncompressed_bytes: compressed.len(),
            data: STANDARD.encode(encoder.finish().unwrap()),
        })
        .unwrap();
        assert!(decode_record(&nested).is_err());
    }

    fn nested_json(depth: usize) -> String {
        format!("{}0{}", "{\"n\":".repeat(depth), "}".repeat(depth))
    }

    #[test]
    fn generated_storage_cases_drive_the_lossless_codec() {
        let cases = crate::lean_vocab_test::lean_rendered_capture_storage_cases();
        assert_eq!(
            cases.len(),
            10,
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
                            node_did: "agent".into(),
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
                // The row's Lean store, rebuilt around the real v3 codec: each
                // modeled reference becomes a block document under its pinned
                // witness, answering with the stored commit or not at all.
                // Resolution runs at fuel `max_depth + 1`, mirroring the Lean
                // rows' `resolveRequest store (maxDepth + 1) 1 10`.
                "manifest" => {
                    let chunks = if case.manifest_blocks.is_empty() {
                        Vec::new()
                    } else {
                        let body = json!({
                            "request": case.request,
                            "payload": deterministic_capture_text(6000),
                        });
                        chunk_capture_body(&super::super::canonical_json_string(&body).unwrap())
                    };
                    assert_eq!(
                        chunks.len(),
                        case.manifest_blocks.len(),
                        "{}: the chunked body must span the modeled references",
                        case.name
                    );
                    let mut entries = Vec::new();
                    let mut store = BTreeMap::new();
                    for (block, chunk) in case.manifest_blocks.iter().zip(&chunks) {
                        let doc_id = format!("block-{}", block.r#ref);
                        entries.push(ManifestEntry {
                            doc_id: doc_id.clone(),
                            content_key: chunk.content_key.clone(),
                            field_commit_cid: format!("witness-{}", block.pinned_witness),
                            byte_len: u64::try_from(chunk.bytes.len()).unwrap(),
                        });
                        if let Some(stored) = block.stored_witness {
                            store
                                .insert(doc_id, (chunk.bytes.clone(), format!("witness-{stored}")));
                        }
                    }
                    let manifest = encode_manifest(&entries).unwrap();
                    resolve_manifest_with_limit(case.max_depth + 1, &manifest.stored, |entry| {
                        store
                            .get(&entry.doc_id)
                            .cloned()
                            .context("block document is missing")
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

    /// Deterministic pseudo-text for capture bodies under test: a fixed-seed
    /// xorshift64 stream mapped onto `a..z`. The exact stream is load-bearing
    /// for the manifest fence (the body must chunk into the block count the
    /// kernel rows pin), so the seed is fixed and never derived from the case.
    fn deterministic_capture_text(len: usize) -> String {
        let mut state = 0x243F_6A88_85A3_08D3_u64;
        let mut out = String::with_capacity(len);
        while out.len() < len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.push(char::from(b'a' + (state % 26) as u8));
        }
        out
    }

    #[test]
    fn content_defined_chunking_is_deterministic_and_partition_exact() {
        let body = deterministic_capture_text(24 * 1024);
        let first = chunk_capture_body(&body);
        let second = chunk_capture_body(&body);
        assert_eq!(
            first.iter().map(|b| b.bytes.clone()).collect::<Vec<_>>(),
            second.iter().map(|b| b.bytes.clone()).collect::<Vec<_>>(),
            "identical bytes must chunk identically"
        );
        let reassembled = first.iter().fold(String::new(), |mut out, block| {
            out.push_str(std::str::from_utf8(&block.bytes).unwrap());
            out
        });
        assert_eq!(reassembled, body, "chunking must be an exact partition");
        assert!(first.len() > 1, "a 24 KiB body should split");
    }

    #[test]
    fn mid_body_insertion_resynchronizes_chunk_boundaries() {
        let original = deterministic_capture_text(48 * 1024);
        let middle = original.len() / 2;
        let mut edited = String::with_capacity(original.len() + 200);
        edited.push_str(&original[..middle]);
        edited.push_str(&deterministic_capture_text(200));
        edited.push_str(&original[middle..]);

        let before = chunk_capture_body(&original);
        let after = chunk_capture_body(&edited);
        // The load-bearing property: boundaries after the edit land on the same
        // content, so the tail blocks are byte-identical and reusable instead
        // of all-new content.
        let mut shared_tail = 0;
        while shared_tail < before.len()
            && shared_tail < after.len()
            && before[before.len() - 1 - shared_tail].bytes
                == after[after.len() - 1 - shared_tail].bytes
        {
            shared_tail += 1;
        }
        assert!(
            shared_tail >= 3,
            "boundaries did not re-synchronize after a mid-body edit: \
                {shared_tail} shared tail blocks of {}/{}",
            before.len(),
            after.len()
        );
    }

    #[test]
    fn bodies_larger_than_the_maximum_split_within_bounds() {
        let blocks = chunk_capture_body(&"x".repeat(40 * 1024));
        assert!(
            blocks.len() >= 3,
            "a 40 KiB body must split well below the per-block maximum"
        );
        for (index, block) in blocks.iter().enumerate() {
            assert!(block.bytes.len() <= CHUNK_MAX_SIZE);
            if index + 1 < blocks.len() {
                assert!(block.bytes.len() >= CHUNK_MIN_SIZE);
            }
        }
    }

    /// Every cut lands on a UTF-8 character boundary, so a block is itself
    /// valid UTF-8 and stores byte for byte in a String column — a split
    /// inside a multi-byte character would corrupt the payload and break the
    /// content key. A forced cut may overshoot the maximum by at most the
    /// three continuation bytes it takes to reach one.
    #[test]
    fn cuts_land_on_utf8_character_boundaries() {
        let multibyte = "λ".repeat(9 * 1024);
        let body = json!({"model": "m", "text": multibyte, "tail": "λ"});
        let canonical = super::super::canonical_json_string(&body).unwrap();
        let blocks = chunk_capture_body(&canonical);
        assert!(blocks.len() >= 2, "the body should span several blocks");
        for block in &blocks {
            assert!(
                std::str::from_utf8(&block.bytes).is_ok(),
                "a block split inside a multi-byte character is not storable as written"
            );
            assert!(block.bytes.len() <= CHUNK_MAX_SIZE + 3);
        }
        let reassembled = blocks.iter().fold(String::new(), |mut out, block| {
            out.push_str(std::str::from_utf8(&block.bytes).unwrap());
            out
        });
        assert_eq!(reassembled, canonical);
    }

    fn manifest_store(
        blocks: &[ChunkedBlock],
    ) -> (Vec<ManifestEntry>, BTreeMap<String, (String, Vec<u8>)>) {
        let mut entries = Vec::new();
        let mut store = BTreeMap::new();
        for (index, block) in blocks.iter().enumerate() {
            let doc_id = format!("block-{index}");
            let witness = format!("witness-{index}");
            entries.push(ManifestEntry {
                doc_id: doc_id.clone(),
                content_key: block.content_key.clone(),
                field_commit_cid: witness.clone(),
                byte_len: u64::try_from(block.bytes.len()).unwrap(),
            });
            store.insert(doc_id, (witness, block.bytes.clone()));
        }
        (entries, store)
    }

    #[test]
    fn compressible_manifest_container_preserves_exact_block_resolution() {
        let chunks = std::iter::once("[")
            .chain(std::iter::repeat_n("\"λ\",", 64))
            .chain(std::iter::once("\"λ\"]"))
            .map(|text| ChunkedBlock {
                content_key: format!("{:x}", Sha256::digest(text.as_bytes())),
                bytes: text.as_bytes().to_vec(),
            })
            .collect::<Vec<_>>();
        let (entries, store) = manifest_store(&chunks);
        let manifest = encode_manifest(&entries).unwrap();
        assert!(manifest.stored.len() >= MIN_COMPRESSION_BYTES);
        let container = encode_container(&manifest, &manifest).unwrap();
        for kind in [
            CapturePayloadKind::RequestBody,
            CapturePayloadKind::ProvenancePayload,
        ] {
            let record = select_container_record(&container, kind).unwrap();
            let envelope: Value = serde_json::from_str(&record).unwrap();
            assert_eq!(envelope["kind"], "zlib");
            assert_eq!(decompress_record(&record).unwrap(), manifest.stored);
            let resolved = resolve_manifest_with_limit(entries.len() + 2, &record, |entry| {
                let (witness, bytes) = &store[&entry.doc_id];
                Ok((bytes.clone(), witness.clone()))
            })
            .unwrap();
            assert_eq!(resolved, json!(vec!["λ"; 65]));
        }
    }

    #[test]
    fn manifest_round_trip_reassembles_the_exact_canonical_bytes() {
        let body = json!({
            "request": 7,
            "payload": deterministic_capture_text(12 * 1024),
        });
        let canonical = super::super::canonical_json_string(&body).unwrap();
        let (entries, store) = manifest_store(&chunk_capture_body(&canonical));
        assert!(entries.len() > 1, "the body should span several blocks");
        let manifest = encode_manifest(&entries).unwrap();
        let resolved = resolve_manifest_with_limit(64, &manifest.stored, |entry| {
            let (witness, bytes) = &store[&entry.doc_id];
            Ok((bytes.clone(), witness.clone()))
        })
        .unwrap();
        // Reassemble-and-canonical-compare, the same discipline as encode_full.
        assert_eq!(
            super::super::canonical_json_string(&resolved).unwrap(),
            canonical
        );
        assert_eq!(resolved, body);
    }

    #[test]
    fn manifest_resolution_fails_closed() {
        let body = json!({"request": 9, "payload": deterministic_capture_text(6000)});
        let canonical = super::super::canonical_json_string(&body).unwrap();
        let (entries, store) = manifest_store(&chunk_capture_body(&canonical));
        let manifest = encode_manifest(&entries).unwrap();
        let block_count = entries.len();

        // The fuel bound mirrors the Lean owner: k full blocks first resolve at
        // fuel k + 2, so k + 1 fails closed even with every block present.
        assert!(
            resolve_manifest_with_limit(block_count + 1, &manifest.stored, |entry| {
                let (witness, bytes) = &store[&entry.doc_id];
                Ok((bytes.clone(), witness.clone()))
            })
            .is_err()
        );
        assert!(
            resolve_manifest_with_limit(block_count + 2, &manifest.stored, |entry| {
                let (witness, bytes) = &store[&entry.doc_id];
                Ok((bytes.clone(), witness.clone()))
            })
            .is_ok()
        );

        assert!(
            resolve_manifest_with_limit(64, &manifest.stored, |_| {
                anyhow::bail!("block document is missing")
            })
            .is_err(),
            "a missing block document fails closed"
        );
        assert!(
            resolve_manifest_with_limit(64, &manifest.stored, |entry| {
                let (witness, bytes) = &store[&entry.doc_id];
                Ok((bytes.clone(), format!("changed-{witness}")))
            })
            .is_err(),
            "a block stored under a different field commit fails closed"
        );
        assert!(
            resolve_manifest_with_limit(64, &manifest.stored, |entry| {
                let (witness, bytes) = &store[&entry.doc_id];
                Ok((bytes[..bytes.len() - 1].to_vec(), witness.clone()))
            })
            .is_err(),
            "a short block read fails closed"
        );

        let empty = encode_manifest(&[]).unwrap();
        assert!(
            resolve_manifest_with_limit(64, &empty.stored, |_| unreachable!()).is_err(),
            "an empty manifest reassembles zero bytes, which is no capture value"
        );
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
                node_did: "agent".into(),
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
            DecodedRecord::Manifest { .. } => panic!("unexpected manifest encoding"),
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
                node_did: "agent".into(),
                requester_did: String::new(),
                session_id: "session".into(),
                source: "source".into(),
                capture_scope: "inference.1".into(),
            },
        )
        .unwrap();
        let container = encode_container(&encoded, &encode_full(&next).unwrap()).unwrap();
        let resolved = resolve_capture_with(
            2,
            &container,
            CapturePayloadKind::RequestBody,
            |_| {
                let base_container =
                    encode_container(&encode_full(&base).unwrap(), &encode_full(&base).unwrap())
                        .unwrap();
                Ok((2, base_container, "cid".into()))
            },
            |_| unreachable!(),
        )
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
                    |_| unreachable!(),
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
                    node_did: "agent".into(),
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
                    |_| Ok((2, base_container.clone(), "cid".into())),
                    |_| unreachable!()
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
                node_did: "agent".into(),
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
                node_did: "agent".into(),
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
                node_did: "agent".into(),
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
                |_| unreachable!(),
                |_| unreachable!()
            )
            .unwrap(),
            body
        );
        assert_eq!(
            resolve_capture_with(
                2,
                &stored,
                CapturePayloadKind::ProvenancePayload,
                |_| unreachable!(),
                |_| unreachable!()
            )
            .unwrap(),
            provenance
        );

        let mut malformed: Value = serde_json::from_str(&stored).unwrap();
        malformed["request_body"]["gents_lossless_json"] = json!(99);
        assert!(resolve_capture_with(
            2,
            &malformed.to_string(),
            CapturePayloadKind::RequestBody,
            |_| unreachable!(),
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
            node_did: "agent".into(),
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
