use anyhow::{anyhow, Result};
use serde_json::Value;

pub(crate) struct Ordered(
    Vec<(
        std::borrow::Cow<'static, str>,
        Box<serde_json::value::RawValue>,
    )>,
    bool,
);

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

macro_rules! ordered {
    ($($key:literal : $value:expr),* $(,)?) => {
        $crate::tools::output::Ordered::new(vec![$($crate::tools::output::Ordered::entry($key, &$value)),*])
    };
}
pub(crate) use ordered;

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
