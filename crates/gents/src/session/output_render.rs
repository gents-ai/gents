use anyhow::{anyhow, Result};
use serde_json::Value;

pub struct Ordered(
    Vec<(
        std::borrow::Cow<'static, str>,
        Box<serde_json::value::RawValue>,
    )>,
    bool,
);

struct Fields(
    Vec<(
        std::borrow::Cow<'static, str>,
        Box<serde_json::value::RawValue>,
    )>,
);

impl<'de> serde::Deserialize<'de> for Fields {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct FieldsVisitor;

        impl<'de> serde::de::Visitor<'de> for FieldsVisitor {
            type Value = Fields;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Fields, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut fields = Vec::new();
                while let Some((key, value)) =
                    map.next_entry::<String, Box<serde_json::value::RawValue>>()?
                {
                    fields.push((key.into(), value));
                }
                Ok(Fields(fields))
            }
        }

        deserializer.deserialize_map(FieldsVisitor)
    }
}

impl serde::Serialize for Ordered {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
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
    pub fn entry(
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

    pub fn new(
        entries: Vec<(
            std::borrow::Cow<'static, str>,
            Box<serde_json::value::RawValue>,
        )>,
    ) -> Self {
        Self(entries, true)
    }

    /// A serializable struct's fields in declaration order, each value in
    /// its compact form.
    pub fn from_serialize(value: &impl serde::Serialize) -> Result<Self> {
        let Fields(fields) = serde_json::from_str(&serde_json::to_string(value)?)?;
        Ok(Self(fields, true))
    }

    pub fn reading_order(value: Value, first: &[&'static str]) -> Self {
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

    pub fn preserving_nulls(value: Value, first: &[&'static str]) -> Self {
        let mut ordered = Self::reading_order(value, first);
        ordered.1 = false;
        ordered
    }

    pub fn push(
        &mut self,
        entry: (
            std::borrow::Cow<'static, str>,
            Box<serde_json::value::RawValue>,
        ),
    ) {
        self.0.push(entry);
    }

    pub fn pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|error| anyhow!("serialize result: {error}"))
    }
}

macro_rules! ordered {
    ($($key:literal : $value:expr),* $(,)?) => {
        $crate::session::output_render::Ordered::new(vec![$($crate::session::output_render::Ordered::entry($key, &$value)),*])
    };
}
pub use ordered;

pub fn terminal_output_tail(source: &str, budget: usize) -> &str {
    let mut start = source.len().saturating_sub(budget);
    while !source.is_char_boundary(start) {
        start += 1;
    }
    &source[start..]
}

pub fn render(value: &impl serde::Serialize, first: &[&'static str]) -> Result<String> {
    let original = serde_json::to_string_pretty(value)?;
    let value = serde_json::to_value(value)?;
    if !value.is_object() {
        return Ok(original);
    }
    render_value_or_original(value, original, first)
}

pub fn render_json_text(original: String, first: &[&'static str]) -> Result<String> {
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
