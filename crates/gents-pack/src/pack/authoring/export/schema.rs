use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::Value;

use gents_store::config_client::{preview_schema_install, ConfigAccess};

/// The SDL of one live application collection, checked to describe exactly
/// the collection it was read from: a definition this renderer cannot express
/// (a relation, a view) is refused rather than shipped lossy.
pub async fn collection_sdl(access: &ConfigAccess, collection: &str) -> Result<String> {
    let version = access
        .collection_version(collection)
        .await?
        .with_context(|| format!("collection {collection} is not on this node"))?;
    let sdl = render(&version).with_context(|| format!("collection {collection}"))?;
    let plan = preview_schema_install(access, &sdl)
        .await
        .with_context(|| {
            format!("collection {collection} has a definition pack export cannot write as SDL")
        })?;
    plan.require_satisfied().with_context(|| {
        format!("collection {collection} has a definition pack export cannot write as SDL")
    })?;
    let written = query::parse_sdl(&sdl)?
        .into_iter()
        .next()
        .map(serde_json::to_value)
        .transpose()?
        .unwrap_or_default();
    for kind in FIELD_INDEX_KINDS {
        anyhow::ensure!(
            field_indexes(&written, kind) == field_indexes(&version, kind),
            "collection {collection} has {kind} pack export cannot write as SDL"
        );
    }
    Ok(sdl)
}

/// The per-field index lists the schema contract digest does not cover, so
/// the SDL written for them is compared with the live definition here.
const FIELD_INDEX_KINDS: [&str; 2] = ["FullTextIndexes", "EncryptedIndexes"];

fn field_indexes(version: &Value, kind: &str) -> BTreeMap<String, Value> {
    version[kind]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|index| Some((index["FieldName"].as_str()?.to_owned(), index.clone())))
        .collect()
}

/// A GraphQL float literal: `1` would read back as an Int.
fn float_literal(value: &Value) -> Option<String> {
    let number = value.as_f64()?;
    Some(if number.fract() == 0.0 && number.is_finite() {
        format!("{number:.1}")
    } else {
        number.to_string()
    })
}

/// `@default(value: ...)` for a stored default. DefraDB keeps a default as
/// the JSON scalar its SDL literal parsed to, so the literal is that scalar.
fn default_directive(field_name: &str, default: &Value) -> Result<Option<String>> {
    match default {
        Value::Null => Ok(None),
        Value::String(_) | Value::Number(_) | Value::Bool(_) => {
            Ok(Some(format!("@default(value: {default})")))
        }
        _ => anyhow::bail!(
            "field {field_name} has a structured default, which pack export does not write"
        ),
    }
}

fn render(version: &Value) -> Result<String> {
    let name = version["Name"].as_str().context("has no name")?;
    anyhow::ensure!(
        version.get("Query").is_none_or(Value::is_null),
        "is a view; pack export carries collections only"
    );
    let fulltext = field_indexes(version, "FullTextIndexes");
    let encrypted = field_indexes(version, "EncryptedIndexes");
    let mut plain_indexes: BTreeMap<&str, String> = BTreeMap::new();
    let mut type_directives = Vec::new();
    if let Some(policy) = version.get("Policy").filter(|policy| !policy.is_null()) {
        type_directives.push(format!(
            "@policy(id: {}, resource: {})",
            Value::String(policy["ID"].as_str().unwrap_or_default().to_owned()),
            Value::String(
                policy["ResourceName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            ),
        ));
    }
    if version["IsBranchable"].as_bool() == Some(true) {
        type_directives.push("@branchable".to_owned());
    }
    for index in version["Indexes"].as_array().into_iter().flatten() {
        let fields = index["Fields"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let unique = index["Unique"].as_bool() == Some(true);
        match fields {
            [field] => {
                let mut arguments = Vec::new();
                if unique {
                    arguments.push("unique: true");
                }
                if field["Descending"].as_bool() == Some(true) {
                    arguments.push("direction: DESC");
                }
                let directive = if arguments.is_empty() {
                    "@index".to_owned()
                } else {
                    format!("@index({})", arguments.join(", "))
                };
                let name = field["Name"].as_str().context("index field has no name")?;
                plain_indexes.insert(name, directive);
            }
            fields => {
                let names = fields
                    .iter()
                    .map(|field| {
                        anyhow::ensure!(
                            field["Descending"].as_bool() != Some(true),
                            "has a composite descending index, which pack export does not write"
                        );
                        Ok(Value::String(
                            field["Name"]
                                .as_str()
                                .context("index field has no name")?
                                .to_owned(),
                        )
                        .to_string())
                    })
                    .collect::<Result<Vec<_>>>()?;
                type_directives.push(format!(
                    "@index(fields: [{}]{})",
                    names.join(", "),
                    if unique { ", unique: true" } else { "" }
                ));
            }
        }
    }
    let mut sdl = format!("type {name}");
    for directive in &type_directives {
        sdl.push(' ');
        sdl.push_str(directive);
    }
    sdl.push_str(" {\n");
    for field in version["Fields"].as_array().into_iter().flatten() {
        let field_name = field["Name"].as_str().context("field has no name")?;
        if field_name.starts_with('_') {
            continue;
        }
        anyhow::ensure!(
            !field["Kind"].is_object(),
            "field {field_name} is a relation, which pack export does not write"
        );
        let kind: schema::FieldKind = serde_json::from_value(field["Kind"].clone())
            .with_context(|| format!("field {field_name} has an unknown kind"))?;
        sdl.push_str(&format!("  {field_name}: {}", kind.graphql_type_name()));
        if let Some(index) = plain_indexes.get(field_name) {
            sdl.push(' ');
            sdl.push_str(index);
        }
        if let Some(index) = fulltext.get(field_name) {
            let language = index["Language"].as_str().unwrap_or("english");
            sdl.push_str(&format!(
                " @fulltext(language: {}",
                Value::String(language.to_owned())
            ));
            for (argument, key) in [("k1", "K1"), ("b", "B")] {
                if let Some(value) = float_literal(&index[key]) {
                    sdl.push_str(&format!(", {argument}: {value}"));
                }
            }
            sdl.push(')');
        }
        if encrypted.contains_key(field_name) {
            sdl.push_str(" @encryptedIndex");
        }
        if let Some(default) = default_directive(field_name, &field["DefaultValue"])? {
            sdl.push(' ');
            sdl.push_str(&default);
        }
        if field["Immutable"].as_bool() == Some(true) {
            sdl.push_str(" @immutable");
        }
        sdl.push('\n');
    }
    sdl.push_str("}\n");
    Ok(sdl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_sdl_parses_back_to_the_fields_and_indexes_it_was_read_from() {
        let source = "type TicketNote @index(fields: [\"queue\", \"ticket\"], unique: true) {\n  ticket: String @index(unique: true) @immutable\n  queue: String @index @default(value: \"in\\\"box\")\n  body: String @fulltext(language: \"german\", k1: 2.0, b: 0.5)\n  subject: String @fulltext\n  secret: String @encryptedIndex\n  winner: String @default(value: \"\")\n  score: Int @default(value: 20)\n  weight: Float @default(value: 1.5)\n  open: Boolean @default(value: true)\n  seen_at: DateTime @index(direction: DESC)\n  labels: [String!]\n}\n";
        let parsed = query::parse_sdl(source).unwrap().remove(0);
        let live = serde_json::to_value(&parsed).unwrap();
        assert_eq!(live["FullTextIndexes"].as_array().unwrap().len(), 2);
        let rendered = render(&live).unwrap();
        let reparsed = query::parse_sdl(&rendered).unwrap().remove(0);
        let written = serde_json::to_value(&reparsed).unwrap();
        for kind in FIELD_INDEX_KINDS {
            assert_eq!(
                field_indexes(&written, kind),
                field_indexes(&live, kind),
                "{rendered}"
            );
        }
        let digest = |version: &schema::CollectionVersion| {
            gents_store::config_client::collection_schema_contract_digest(
                &serde_json::to_value(version).unwrap(),
            )
            .unwrap()
        };
        assert_eq!(digest(&reparsed), digest(&parsed), "{rendered}");
    }

    #[test]
    fn a_relation_is_refused_by_name() {
        let parsed = query::parse_sdl(
            "type Author { name: String books: [Book] }\ntype Book { title: String author: Author }",
        )
        .unwrap();
        let error = render(&serde_json::to_value(&parsed[0]).unwrap()).unwrap_err();
        assert!(format!("{error:#}").contains("is a relation"), "{error:#}");
    }
}
