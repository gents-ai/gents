//! `metadata.user_id`: the JSON-encoded install, account and session identity
//! (Claude Code `Gze`, `chunk-gpjkk9qe.js`).

use rapidhash::{HashMapExt, RapidHashMap};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Longest serialized `user_id` that keeps the extra metadata, in UTF-16 code units (`ie`).
const MAX_USER_ID_UNITS: usize = 512;

/// Keys of `CLAUDE_CODE_EXTRA_METADATA` that Claude Code drops.
const RESERVED_EXTRA_KEYS: [&str; 7] = ["ti", "os", "sb", "he", "uf", "ap", "tk"];

/// Why a `user_id` cannot be built.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MetadataError {
    /// The device id is not 64 lowercase hex characters.
    #[error("the device id must be 64 lowercase hex characters")]
    InvalidDeviceId,
}

/// The install, account and session identity serialized into `metadata.user_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserIdentity {
    /// 64 lowercase hex characters, see [`device_id`].
    pub device_id: String,
    /// Account UUID, empty when unknown.
    pub account_uuid: String,
    /// Session id.
    pub session_id: String,
    /// Parent session id of a subagent or forked session.
    pub parent_session_id: Option<String>,
}

/// Derives the stable install id of one Gents node and account: lowercase hex SHA-256 of `seed`.
///
/// Claude Code stores a random 32-byte value once per install. The caller supplies
/// a seed unique per node and account (for example node DID plus account key).
pub fn device_id(seed: &[u8]) -> String {
    let digest = Sha256::digest(seed);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 15), 16).unwrap_or('0'));
    }
    out
}

/// `metadata.user_id`: `JSON.stringify` of the filtered extras followed by the identity.
///
/// Extras keep their position when they collide with an identity key but take the
/// identity's value; an extra object that makes the result exceed 512 UTF-16 units
/// is dropped entirely, as Claude Code does.
pub fn user_id(
    identity: &UserIdentity,
    extra: &[(String, Value)],
) -> Result<String, MetadataError> {
    let valid_device = identity.device_id.len() == 64
        && identity
            .device_id
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !valid_device {
        return Err(MetadataError::InvalidDeviceId);
    }
    let identity_entries = || {
        [
            Some(("device_id", string(&identity.device_id))),
            Some(("account_uuid", string(&identity.account_uuid))),
            Some(("session_id", string(&identity.session_id))),
            identity
                .parent_session_id
                .as_deref()
                .map(|id| ("parent_session_id", string(id))),
        ]
        .into_iter()
        .flatten()
    };

    let mut entries: Vec<(&str, String)> = Vec::with_capacity(extra.len() + 4);
    let mut at: RapidHashMap<&str, usize> = RapidHashMap::with_capacity(extra.len() + 4);
    for (key, value) in extra {
        if RESERVED_EXTRA_KEYS.contains(&key.as_str()) {
            continue;
        }
        let Some(rendered) = scalar(value) else {
            continue;
        };
        place(&mut entries, &mut at, key, rendered);
    }
    let has_extra = !entries.is_empty();
    for (key, rendered) in identity_entries() {
        place(&mut entries, &mut at, key, rendered);
    }
    // JS objects enumerate canonical array-index keys first, ascending.
    entries.sort_by_key(|(key, _)| array_index(key).unwrap_or(u32::MAX));

    if has_extra {
        let full = object(&entries);
        if full.encode_utf16().count() <= MAX_USER_ID_UNITS {
            return Ok(full);
        }
    }
    let only: Vec<(&str, String)> = identity_entries().collect();
    Ok(object(&only))
}

fn place<'a>(
    entries: &mut Vec<(&'a str, String)>,
    at: &mut RapidHashMap<&'a str, usize>,
    key: &'a str,
    rendered: String,
) {
    match at.get(key) {
        Some(&i) => entries[i].1 = rendered,
        None => {
            at.insert(key, entries.len());
            entries.push((key, rendered));
        }
    }
}

fn object(entries: &[(&str, String)]) -> String {
    let size = entries
        .iter()
        .map(|(k, v)| k.len() + v.len() + 6)
        .sum::<usize>()
        + 2;
    let mut out = String::with_capacity(size);
    out.push('{');
    for (i, (key, rendered)) in entries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&string(key));
        out.push(':');
        out.push_str(rendered);
    }
    out.push('}');
    out
}

fn string(text: &str) -> String {
    // A str always serializes; the fallback only satisfies the type.
    serde_json::to_string(text).unwrap_or_else(|_| String::from("\"\""))
}

/// A JS array index: canonical decimal below 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    let canonical = key == "0" || (!key.starts_with('0') && !key.is_empty());
    if !canonical || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|&n| n != u32::MAX)
}

/// The `JSON.stringify` form of a string, number or boolean; `None` for anything else.
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(string(text)),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(match (number.as_i64(), number.as_u64()) {
            (Some(n), _) => n.to_string(),
            (_, Some(n)) => n.to_string(),
            _ => number.as_f64().map_or_else(|| "null".into(), js_number),
        }),
        _ => None,
    }
}

/// ECMAScript `Number::toString` for a finite value.
fn js_number(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let sci = format!("{:e}", value.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let n = digits.len() as i32;
    let k = exp.parse::<i32>().unwrap_or(0) + 1;
    let mut out = String::new();
    if value < 0.0 {
        out.push('-');
    }
    if n <= k && k <= 21 {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', (k - n) as usize));
    } else if 0 < k && k <= 21 {
        out.push_str(&digits[..k as usize]);
        out.push('.');
        out.push_str(&digits[k as usize..]);
    } else if -6 < k && k <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-k) as usize));
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if n > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if k > 0 { '+' } else { '-' });
        out.push_str(&(k - 1).abs().to_string());
    }
    out
}

#[cfg(test)]
#[path = "metadata_tests.rs"]
mod tests;
