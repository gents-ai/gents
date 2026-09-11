use std::fmt;

use serde::de::{DeserializeOwned, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Treat a missing (with `serde(default)`) or null field as its canonical default.
/// Wrong non-null types remain errors; required identities/auth must not use this.
pub(crate) fn deserialize_default_on_null<'de, D, T>(
    deserializer: D,
) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

pub(super) fn default_display_name_for_did(agent_did: &str) -> String {
    agent_did
        .rsplit(':')
        .next()
        .filter(|segment| !segment.trim().is_empty())
        .unwrap_or(agent_did)
        .to_string()
}

pub(super) fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

pub(super) fn deserialize_optional_string_vec<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionalStringVecVisitor;

    impl<'de> Visitor<'de> for OptionalStringVecVisitor {
        type Value = Option<Vec<String>>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a string list, null, or empty string")
        }

        fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            if value.trim().is_empty() {
                Ok(Some(Vec::new()))
            } else {
                Ok(Some(vec![value.to_string()]))
            }
        }

        fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            self.visit_str(&value)
        }

        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element::<String>()? {
                values.push(value);
            }
            Ok(Some(values))
        }
    }

    deserializer.deserialize_any(OptionalStringVecVisitor)
}

/// Like [`deserialize_optional_string_vec`] but yields a plain `Vec<String>`,
/// mapping null / unit / empty-string / missing to an empty vec. DefraDB returns
/// `null` for an unset `[String!]` field, which a bare `Vec<String>` cannot
/// deserialize; this keeps such fields null-safe without an `Option` wrapper.
/// Walk a JSON value that is either a list of `T` objects, a list of JSON
/// strings encoding `T`, a single JSON string, null, or missing.
///
/// Shared by write-tool and surface-entry deserializers in this crate and the
/// CLI storage walkers so the dual-shape contract cannot drift.
pub fn deserialize_dual_shape<T: DeserializeOwned>(
    value: Option<Value>,
    label: &str,
) -> std::result::Result<Vec<T>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    match value {
        Value::Null => Ok(Vec::new()),
        Value::String(s) if s.trim().is_empty() => Ok(Vec::new()),
        Value::String(s) => {
            let item = serde_json::from_str(&s).map_err(|error| error.to_string())?;
            Ok(vec![item])
        }
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let parsed = match item {
                    Value::String(s) => serde_json::from_str(&s),
                    other => serde_json::from_value(other),
                }
                .map_err(|error| error.to_string())?;
                out.push(parsed);
            }
            Ok(out)
        }
        other => Err(format!("{label}, got {other}")),
    }
}

pub(super) fn deserialize_string_vec_or_null<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(deserialize_optional_string_vec(deserializer)?.unwrap_or_default())
}

pub(super) fn first_row_with_doc_id<T>(data: Option<&Value>, field: &str) -> Option<(String, T)>
where
    T: DeserializeOwned,
{
    rows_with_doc_id(data, field).into_iter().next()
}

pub(super) fn rows_with_doc_id<T>(data: Option<&Value>, field: &str) -> Vec<(String, T)>
where
    T: DeserializeOwned,
{
    data.and_then(|data| data.get(field))
        .and_then(|value| value.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let doc_id = row.get("_docID")?.as_str()?.to_string();
                    // Storage identity is envelope metadata, not authored config.
                    let mut payload = row.clone();
                    payload.as_object_mut()?.remove("_docID");
                    let parsed = match serde_json::from_value(payload) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            tracing::warn!(
                                field = field,
                                doc_id = %doc_id,
                                error = %error,
                                "failed to deserialize document row"
                            );
                            return None;
                        }
                    };
                    Some((doc_id, parsed))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Installed configuration is enabled unless explicitly disabled. Missing and
/// null agree; false remains an explicit override. Tool grants default separately.
pub(super) fn default_enabled() -> bool {
    true
}
pub(super) fn is_enabled(value: &bool) -> bool {
    *value
}
pub(super) fn deserialize_enabled<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<bool, D::Error> {
    Ok(Option::<bool>::deserialize(deserializer)?.unwrap_or(true))
}

pub(super) fn is_disabled(value: &bool) -> bool {
    !*value
}

/// Strict storage-envelope decoding for signing/configuration owners. A bad row
/// must surface an error rather than disappearing from a scoped identity check.
pub(super) fn try_rows_with_doc_id<T: DeserializeOwned>(
    data: Option<&Value>,
    field: &str,
) -> anyhow::Result<Vec<(String, T)>> {
    use anyhow::Context;
    let rows = data
        .and_then(|data| data.get(field))
        .and_then(Value::as_array)
        .with_context(|| format!("{field} response has no rows array"))?;
    rows.iter()
        .map(|row| {
            let mut payload = row.clone();
            let doc_id = payload
                .as_object_mut()
                .context("document row must be an object")?
                .remove("_docID")
                .and_then(|value| value.as_str().map(str::to_string))
                .filter(|value| !value.trim().is_empty())
                .context("document row requires physical ID")?;
            Ok((
                doc_id,
                serde_json::from_value(payload).with_context(|| format!("invalid {field} row"))?,
            ))
        })
        .collect()
}
