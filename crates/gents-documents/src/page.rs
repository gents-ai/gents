use std::collections::BTreeSet;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;

/// Largest module or stylesheet one page document stores: the limit a skill's
/// source has, and well inside what the desktop evaluates
/// (`crate::plugin::ux::MAX_MODULE_BYTES`). A document replicates whole, so
/// a page is a UI, not a bundle.
pub const MAX_PAGE_SOURCE_BYTES: usize = 1024 * 1024;

/// What a page document may register. It is a page: a nav row and its own
/// pane. Transcript directives and agent sections stay with pack pages, whose
/// manifest declares them one by one.
pub const PAGE_AREAS: &[&str] = &["nav", "plugin.pages"];

/// `[a-z][a-z0-9_]*`: the shape of a pack, plugin, page and datastore tool
/// name, each of which lands in a path or an identifier.
pub fn is_snake_case_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// One `DatastoreToolSurface` a page may call into, by the surface's id on
/// the agent's node, narrowed to the named tools. A pack page declares these
/// under `ux[].data`; a page document under `data`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UxDataSurface {
    pub surface: String,
    pub tools: Vec<String>,
}

/// Longest surface id a page may name.
pub const MAX_UX_DATA_SURFACE_ID_LEN: usize = 128;

/// A tool name a page may name: `[a-z][a-z0-9_]{0,63}`, the shape a
/// `DatastoreToolSurface` entry's `tool_name` takes.
pub fn is_valid_ux_data_tool_name(name: &str) -> bool {
    name.len() <= 64 && is_snake_case_name(name)
}

/// The rules a page's data grants satisfy, whichever door declares them.
/// `what` names the declarer in a refusal: `ux plugin` for a pack's manifest,
/// `page` for a page document.
pub fn validate_ux_data(what: &str, name: &str, data: &[UxDataSurface]) -> Result<()> {
    let mut surfaces = BTreeSet::new();
    for declared in data {
        let surface = declared.surface.as_str();
        anyhow::ensure!(
            !surface.is_empty()
                && !surface.chars().any(char::is_whitespace)
                && surface.len() <= MAX_UX_DATA_SURFACE_ID_LEN,
            "{what} {name:?} declares the data surface {surface:?}; a surface id is non-empty, \
             has no whitespace and is at most {MAX_UX_DATA_SURFACE_ID_LEN} characters"
        );
        anyhow::ensure!(
            surfaces.insert(surface),
            "{what} {name:?} declares the data surface {surface:?} twice"
        );
        anyhow::ensure!(
            !declared.tools.is_empty(),
            "{what} {name:?} declares the data surface {surface:?} with no tools; name the \
             tools its pages may run"
        );
        let mut tools = BTreeSet::new();
        for tool in &declared.tools {
            anyhow::ensure!(
                is_valid_ux_data_tool_name(tool),
                "{what} {name:?} declares the tool {tool:?} on data surface {surface:?}; a tool \
                 name is lowercase letters, digits and underscores, starting with a letter, \
                 at most 64 characters"
            );
            anyhow::ensure!(
                tools.insert(tool.as_str()),
                "{what} {name:?} declares the tool {tool:?} on data surface {surface:?} twice"
            );
        }
    }
    Ok(())
}

/// A desktop page held on the node: its module, optional stylesheet and the
/// datastore tools it may run. `enabled` is its only state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PageDocument {
    pub page_id: String,
    pub node_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    /// One plain ES module whose default export is `{id, name, register}`;
    /// its `id` is `page_id`.
    pub module: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub css: Option<String>,
    /// The datastore tools the page may run, by surface: the host refuses any
    /// (surface, tool) pair not named here (`crate::observe::ux_data`).
    #[serde(
        default,
        deserialize_with = "deserialize_data",
        serialize_with = "serialize_data",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(
        feature = "typescript",
        ts(
            type = "Array<{ surface: string, tools: Array<string> }>",
            optional = nullable
        )
    )]
    #[schemars(schema_with = "data_schema")]
    pub data: Option<Vec<UxDataSurface>>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    #[schemars(transform = crate::accepts_null)]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_at: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    #[schemars(transform = crate::accepts_null)]
    pub tags: Vec<String>,
}

/// The stored shape is `{"surfaces": [...]}`; a write may also send the bare
/// list. An empty list is stored as absent.
fn deserialize_data<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<UxDataSurface>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    use serde_json::Value;

    let value = match Option::<Value>::deserialize(deserializer)? {
        Some(Value::Object(mut envelope))
            if envelope.len() == 1 && envelope.contains_key("surfaces") =>
        {
            envelope.remove("surfaces")
        }
        value => value,
    };
    let surfaces: Vec<UxDataSurface> = super::serde_helpers::deserialize_dual_shape(
        value,
        "Page.data must be a list of {surface, tools} objects",
    )
    .map_err(D::Error::custom)?;
    Ok((!surfaces.is_empty()).then_some(surfaces))
}

fn serialize_data<S>(
    value: &Option<Vec<UxDataSurface>>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    #[derive(Serialize)]
    struct Stored<'a> {
        surfaces: &'a [UxDataSurface],
    }
    match value {
        Some(surfaces) => Stored { surfaces }.serialize(serializer),
        None => serializer.serialize_none(),
    }
}

fn data_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let surface = generator.subschema_for::<UxDataSurface>();
    let surfaces = schemars::json_schema!({ "type": "array", "items": surface });
    schemars::json_schema!({
        "anyOf": [
            surfaces.clone(),
            {
                "type": "object",
                "properties": { "surfaces": surfaces },
                "required": ["surfaces"],
                "additionalProperties": false
            },
            { "type": "null" }
        ]
    })
}

impl PageDocument {
    pub fn data(&self) -> &[UxDataSurface] {
        self.data.as_deref().unwrap_or_default()
    }

    /// The id, size and data-grant rules. The grant rules are the ones a
    /// pack page's manifest satisfies ([`validate_ux_data`]), so both doors
    /// admit the same declarations.
    pub fn validate(&self) -> Result<()> {
        let id = &self.page_id;
        anyhow::ensure!(is_snake_case_name(id), "page id must be snake_case: {id:?}");
        for (what, text) in [
            ("module", Some(self.module.as_str())),
            ("css", self.css.as_deref()),
        ] {
            let bytes = text.map_or(0, str::len);
            anyhow::ensure!(
                bytes <= MAX_PAGE_SOURCE_BYTES,
                "page {id:?} {what} is {bytes} bytes; a page document stores at most {} KiB of it",
                MAX_PAGE_SOURCE_BYTES / 1024
            );
        }
        anyhow::ensure!(
            !self.module.trim().is_empty(),
            "page {id:?} needs a module: one ES module whose default export is {{id, name, register}}"
        );
        validate_ux_data("page", id, self.data())
    }
}

/// Every page document `node_did` owns that `access` may read, by id. Through
/// the embedded node or a served node's GraphQL endpoint, so document ACP
/// decides what the reader sees.
pub async fn list_pages(access: &ConfigAccess, node_did: &str) -> Result<Vec<PageDocument>> {
    anyhow::ensure!(!node_did.trim().is_empty(), "page owner is required");
    let (fields, _) = crate::config_projection(gents_store::collection::Collection::Page, None)?;
    let owner = escape_graphql_string(node_did);
    let query = format!(
        "{{ Page(filter: {{ node_did: {{ _eq: \"{owner}\" }} }}) {{ {} }} }}",
        fields.join(" "),
    );
    let response = access.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("Page"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("page query returned no row array"))?;
    let mut pages = rows
        .iter()
        .map(|row| {
            let page: PageDocument = serde_json::from_value(row.clone())?;
            anyhow::ensure!(page.node_did == node_did, "page owner mismatch");
            Ok(page)
        })
        .collect::<Result<Vec<_>>>()?;
    pages.sort_by(|left, right| left.page_id.cmp(&right.page_id));
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page(value: serde_json::Value) -> PageDocument {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn data_reads_the_bare_list_and_the_stored_envelope_alike() {
        let surfaces = json!([{"surface": "board", "tools": ["list_cards"]}]);
        let bare = page(
            json!({"page_id":"board","node_did":"did:key:n","module":"export default {}","data":surfaces}),
        );
        let stored = serde_json::to_value(&bare).unwrap();
        assert_eq!(stored["data"], json!({"surfaces": surfaces}));
        assert_eq!(page(stored), bare);
        let empty = page(json!({"page_id":"board","node_did":"did:key:n","module":"x","data":[]}));
        assert!(empty.data.is_none());
        assert!(serde_json::to_value(&empty).unwrap().get("data").is_none());
    }

    #[test]
    fn validation_uses_the_pack_page_rules_and_the_stored_source_limit() {
        let base = json!({"page_id":"board","node_did":"did:key:n","module":"export default {}"});
        page(base.clone()).validate().unwrap();
        for (field, replacement, expected) in [
            ("page_id", json!("Board Page"), "snake_case"),
            ("module", json!("  "), "needs a module"),
            (
                "module",
                json!("x".repeat(MAX_PAGE_SOURCE_BYTES + 1)),
                "at most 1024 KiB",
            ),
            (
                "css",
                json!("x".repeat(MAX_PAGE_SOURCE_BYTES + 1)),
                "at most 1024 KiB",
            ),
            (
                "data",
                json!([{"surface":"board","tools":[]}]),
                "with no tools",
            ),
            (
                "data",
                json!([{"surface":"board","tools":["Bad Tool"]}]),
                "tool name",
            ),
        ] {
            let mut document = base.clone();
            document[field] = replacement;
            let error = format!("{:#}", page(document).validate().unwrap_err());
            assert!(error.contains(expected), "{field}: {error}");
            assert!(!error.contains("ux plugin"), "{error}");
        }
    }
}
