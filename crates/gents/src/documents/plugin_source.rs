use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use gents_protocol::job::PluginDataDeclaration;

/// Largest source text one plugin source document stores: the limit a skill's
/// source and a page's module have.
pub const MAX_PLUGIN_SOURCE_BYTES: usize = super::MAX_PAGE_SOURCE_BYTES;
/// Most cases one document carries, and the most JSON they hold together.
pub const MAX_PLUGIN_SOURCE_CASES: usize = 64;
pub const MAX_PLUGIN_SOURCE_CASES_BYTES: usize = 256 * 1024;

/// The namespace a plugin built from a node's own source is installed under.
/// It is the plugin's owning pack namespace in the plugin store, so a
/// registry pack cannot claim the same coordinate by accident.
pub const PLUGIN_SOURCE_NAMESPACE: &str = "node";

/// One case a build runs against the compiled artifact: the plugin is called
/// with `input` and must succeed, returning `expect` when that is given.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginSourceCase {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Value>,
}

/// A plugin's source held on the node: what `config plugin build` compiles,
/// tests and installs. The artifact and its digest stay in the plugin store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PluginSourceDocument {
    /// The plugin's name; it installs as `node/<plugin_id>`.
    pub plugin_id: String,
    pub node_did: String,
    /// What the model is shown for the tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    pub language: String,
    /// The single entry file's text, in `language`.
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(type = "unknown", optional = nullable))]
    pub input_schema: Option<Value>,
    /// The collections and operations the plugin's data calls may use;
    /// absent means it has no data access.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(type = "unknown", optional = nullable))]
    pub data: Option<PluginDataDeclaration>,
    #[serde(
        default,
        deserialize_with = "deserialize_cases",
        serialize_with = "serialize_cases",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "typescript", ts(type = "unknown", optional = nullable))]
    #[schemars(schema_with = "cases_schema")]
    pub tests: Option<Vec<PluginSourceCase>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_at: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub tags: Vec<String>,
}

/// The stored shape is `{"cases": [...]}`; a write may also send the bare
/// list. An empty list is stored as absent.
fn deserialize_cases<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<PluginSourceCase>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;

    let value = match Option::<Value>::deserialize(deserializer)? {
        Some(Value::Object(mut envelope))
            if envelope.len() == 1 && envelope.contains_key("cases") =>
        {
            envelope.remove("cases")
        }
        value => value,
    };
    let cases: Vec<PluginSourceCase> = super::serde_helpers::deserialize_dual_shape(
        value,
        "PluginSource.tests must be a list of {input, expect} cases",
    )
    .map_err(D::Error::custom)?;
    Ok((!cases.is_empty()).then_some(cases))
}

fn serialize_cases<S>(
    value: &Option<Vec<PluginSourceCase>>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    #[derive(Serialize)]
    struct Stored<'a> {
        cases: &'a [PluginSourceCase],
    }
    match value {
        Some(cases) => Stored { cases }.serialize(serializer),
        None => serializer.serialize_none(),
    }
}

fn cases_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let case = generator.subschema_for::<PluginSourceCase>();
    let cases = schemars::json_schema!({ "type": "array", "items": case });
    schemars::json_schema!({
        "anyOf": [
            cases.clone(),
            {
                "type": "object",
                "properties": { "cases": cases },
                "required": ["cases"],
                "additionalProperties": false
            },
            { "type": "null" }
        ]
    })
}

impl PluginSourceDocument {
    pub fn cases(&self) -> &[PluginSourceCase] {
        self.tests.as_deref().unwrap_or_default()
    }

    /// The store coordinate the built plugin installs under.
    pub fn coordinate(&self) -> String {
        format!("{PLUGIN_SOURCE_NAMESPACE}/{}", self.plugin_id)
    }

    /// What a stored source must satisfy: its name, its sizes and the shape
    /// of its schema. Whether the language builds and the manifest entry is
    /// admissible is the build owner's check (`crate::pack::authoring`),
    /// made before anything compiles.
    pub fn validate(&self) -> Result<()> {
        let id = &self.plugin_id;
        anyhow::ensure!(
            super::is_snake_case_name(id),
            "plugin name must be snake_case: {id:?}"
        );
        anyhow::ensure!(
            !self.language.trim().is_empty(),
            "plugin {id:?} needs a language"
        );
        anyhow::ensure!(
            self.source.len() <= MAX_PLUGIN_SOURCE_BYTES,
            "plugin {id:?} source is {} bytes; a plugin source document stores at most {} KiB",
            self.source.len(),
            MAX_PLUGIN_SOURCE_BYTES / 1024
        );
        anyhow::ensure!(
            !self.source.trim().is_empty(),
            "plugin {id:?} needs source: the text of its one entry file"
        );
        anyhow::ensure!(
            self.input_schema.as_ref().is_none_or(Value::is_object),
            "plugin {id:?} needs an object input_schema"
        );
        let cases = self.cases();
        anyhow::ensure!(
            cases.len() <= MAX_PLUGIN_SOURCE_CASES,
            "plugin {id:?} has {} test cases; the limit is {MAX_PLUGIN_SOURCE_CASES}",
            cases.len()
        );
        let case_bytes = serde_json::to_vec(cases)?.len();
        anyhow::ensure!(
            case_bytes <= MAX_PLUGIN_SOURCE_CASES_BYTES,
            "plugin {id:?} test cases are {case_bytes} bytes of JSON; the limit is {} KiB",
            MAX_PLUGIN_SOURCE_CASES_BYTES / 1024
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document(value: Value) -> PluginSourceDocument {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn cases_read_the_bare_list_and_the_stored_envelope_alike() {
        let cases = json!([{"name":"echo","input":{"a":1},"expect":{"a":1}}]);
        let bare = document(json!({"plugin_id":"echo","node_did":"did:key:n","language":"js","source":"module.exports = (i) => i;","tests":cases}));
        let stored = serde_json::to_value(&bare).unwrap();
        assert_eq!(stored["tests"], json!({"cases": cases}));
        assert_eq!(document(stored), bare);
    }

    #[test]
    fn validation_holds_the_name_shape_and_stored_limits() {
        let base = json!({"plugin_id":"echo","node_did":"did:key:n","language":"js","source":"module.exports = (i) => i;"});
        let valid = document(base.clone());
        valid.validate().unwrap();
        assert_eq!(valid.coordinate(), "node/echo");
        for (field, replacement, expected) in [
            ("plugin_id", json!("Echo Tool"), "snake_case"),
            ("language", json!(" "), "needs a language"),
            ("source", json!(" "), "needs source"),
            ("source", json!("x".repeat(MAX_PLUGIN_SOURCE_BYTES + 1)), "at most 1024 KiB"),
            ("input_schema", json!([1]), "object input_schema"),
            (
                "tests",
                json!((0..=MAX_PLUGIN_SOURCE_CASES).map(|_| json!({"input":{}})).collect::<Vec<_>>()),
                "the limit is 64",
            ),
        ] {
            let mut value = base.clone();
            value[field] = replacement;
            let error = format!("{:#}", document(value).validate().unwrap_err());
            assert!(error.contains(expected), "{field}: {error}");
        }
    }
}
