use anyhow::{anyhow, Result};
use serde_json::Value;

/// A JSON object that keeps its keys in the order given, for model-facing
/// results that must read top to bottom: the answer, then how to proceed, then
/// metadata. This build's `serde_json::Map` sorts keys (`preserve_order` is
/// off, and canonical digests such as `desired_state_document_digest` rely on
/// sorted keys), so `json!` alone would alphabetize them. Values are kept
/// pre-serialized so a nested `Ordered` keeps its order too. Null values are
/// omitted by default; existing payload envelopes can retain them.
pub(crate) struct Ordered(
    Vec<(
        std::borrow::Cow<'static, str>,
        Box<serde_json::value::RawValue>,
    )>,
    bool,
);

impl serde::Serialize for Ordered {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let present = self
            .0
            .iter()
            .filter(|(_, value)| !self.1 || value.get() != "null");
        let mut map = serializer.serialize_map(Some(present.clone().count()))?;
        for (key, value) in present {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl Ordered {
    pub(crate) fn entry(
        key: impl Into<std::borrow::Cow<'static, str>>,
        value: &impl serde::Serialize,
    ) -> (
        std::borrow::Cow<'static, str>,
        Box<serde_json::value::RawValue>,
    ) {
        (
            key.into(),
            serde_json::value::to_raw_value(value).expect("model-facing results serialize"),
        )
    }

    pub(crate) fn new(
        entries: Vec<(
            std::borrow::Cow<'static, str>,
            Box<serde_json::value::RawValue>,
        )>,
    ) -> Self {
        Self(entries, true)
    }

    /// An object's keys with `first` in that order, then the rest.
    pub(crate) fn reading_order(value: Value, first: &[&'static str]) -> Self {
        let Value::Object(mut object) = value else {
            return Self(vec![Self::entry("value", &value)], true);
        };
        let mut entries = first
            .iter()
            .filter_map(|key| object.remove(*key).map(|value| Self::entry(*key, &value)))
            .collect::<Vec<_>>();
        entries.extend(
            object
                .into_iter()
                .map(|(key, value)| Self::entry(key, &value)),
        );
        Self(entries, true)
    }

    pub(crate) fn preserving_nulls(value: Value, first: &[&'static str]) -> Self {
        let mut ordered = Self::reading_order(value, first);
        ordered.1 = false;
        ordered
    }

    pub(crate) fn push(
        &mut self,
        entry: (
            std::borrow::Cow<'static, str>,
            Box<serde_json::value::RawValue>,
        ),
    ) {
        self.0.push(entry);
    }

    pub(crate) fn pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|error| anyhow!("serialize result: {error}"))
    }
}

/// `ordered!{"key": expr, ...}`: each value is any `Serialize` expression
/// (use `json!` for literal objects).
macro_rules! ordered {
    ($($key:literal : $value:expr),* $(,)?) => {
        $crate::tool_output::Ordered::new(vec![$($crate::tool_output::Ordered::entry($key, &$value)),*])
    };
}
pub(crate) use ordered;

/// Reorder only the model-facing envelope; payloads and explicit nulls retain
/// their JSON meaning. Callers choose their own answer and continuation fields.
/// Oversized envelopes retain their original ordering so moving a large answer
/// cannot push recovery metadata beyond the provider's existing truncation cap.
pub(crate) fn render(value: &impl serde::Serialize, first: &[&'static str]) -> Result<String> {
    let original = serde_json::to_string_pretty(value)?;
    let value = serde_json::to_value(value)?;
    if !value.is_object() {
        return Ok(original);
    }
    render_value_or_original(value, original, first)
}

pub(crate) fn render_json_text(original: String, first: &[&'static str]) -> Result<String> {
    let Ok(Value::Object(value)) = serde_json::from_str::<Value>(&original) else {
        return Ok(original);
    };
    render_value_or_original(Value::Object(value), original, first)
}

fn render_value_or_original(
    value: Value,
    original: String,
    first: &[&'static str],
) -> Result<String> {
    let ordered = Ordered::preserving_nulls(value, first).pretty()?;
    if crate::truncation::truncate(
        &ordered,
        crate::truncation::TruncationMode::Head,
        &crate::truncation::TruncationLimits::default(),
    )
    .truncated
    {
        return Ok(original);
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_envelopes_preserve_payloads_and_nulls() {
        let value = json!({"metadata":1,"next_call":null,"answer":{"z":2,"a":null}});
        let rendered = render(&value, &["answer", "next_call"]).unwrap();
        assert!(rendered.find("\"answer\"").unwrap() < rendered.find("\"next_call\"").unwrap());
        assert!(rendered.find("\"next_call\"").unwrap() < rendered.find("\"metadata\"").unwrap());
        assert_eq!(serde_json::from_str::<Value>(&rendered).unwrap(), value);
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            r#"{"answer":{"a":null,"z":2},"metadata":1,"next_call":null}"#
        );
    }

    #[test]
    fn oversized_answers_keep_recovery_metadata_in_its_original_position() {
        #[derive(serde::Serialize)]
        struct FileResult {
            path: &'static str,
            content_hash: &'static str,
            diff: String,
        }
        let value = FileResult {
            path: "file",
            content_hash: "hash",
            diff: "x".repeat(crate::truncation::TruncationLimits::default().max_bytes),
        };
        let rendered = render(&value, &["diff"]).unwrap();
        assert!(rendered.starts_with("{\n  \"path\":"));
        assert_eq!(
            serde_json::from_str::<Value>(&rendered).unwrap(),
            serde_json::to_value(value).unwrap()
        );
    }

    #[test]
    fn oversized_native_results_keep_the_exact_metadata_first_json() {
        let original = format!(
            r#"{{"path":"root","truncated":true,"entries":["{}"]}}"#,
            "x".repeat(crate::truncation::TruncationLimits::default().max_bytes)
        );
        assert_eq!(
            render_json_text(original.clone(), &["entries"]).unwrap(),
            original
        );
        let small = r#"{"path":"root","entries":["answer"]}"#.to_owned();
        let rendered = render_json_text(small.clone(), &["entries"]).unwrap();
        assert!(rendered.starts_with("{\n  \"entries\":"));
        assert_eq!(
            serde_json::from_str::<Value>(&rendered).unwrap(),
            serde_json::from_str::<Value>(&small).unwrap()
        );
    }

    #[test]
    fn config_objects_still_omit_nulls_and_preserve_nested_order() {
        let nested = ordered! {"z": 1, "a": 2};
        let output = ordered! {"answer": nested, "absent": Value::Null};
        assert_eq!(
            serde_json::to_string(&output).unwrap(),
            r#"{"answer":{"z":1,"a":2}}"#
        );
    }
}
