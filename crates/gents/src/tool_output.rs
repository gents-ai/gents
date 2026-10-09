use anyhow::{anyhow, Result};
use serde_json::Value;

/// A JSON object that keeps its keys in the order given, for model-facing
/// results that must read top to bottom: the answer, then how to proceed, then
/// metadata. This build's `serde_json::Map` sorts keys (`preserve_order` is
/// off, and canonical digests such as `desired_state_document_digest` rely on
/// sorted keys), so `json!` alone would alphabetize them. Values are kept
/// pre-serialized so a nested `Ordered` keeps its order too. Null values are
/// omitted.
pub(crate) struct Ordered(
    pub(crate)  Vec<(
        std::borrow::Cow<'static, str>,
        Box<serde_json::value::RawValue>,
    )>,
);

impl serde::Serialize for Ordered {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let present = self.0.iter().filter(|(_, value)| value.get() != "null");
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
        Self(entries)
    }

    /// An object's keys with `first` in that order, then the rest.
    pub(crate) fn reading_order(value: Value, first: &[&'static str]) -> Self {
        let Value::Object(mut object) = value else {
            return Self(vec![Self::entry("value", &value)]);
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
        Self(entries)
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
