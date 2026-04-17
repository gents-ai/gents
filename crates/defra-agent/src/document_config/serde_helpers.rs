use std::fmt;

use serde::de::{DeserializeOwned, SeqAccess, Visitor};
use serde::Deserializer;
use serde_json::Value;

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
                    let parsed = match serde_json::from_value(row.clone()) {
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
