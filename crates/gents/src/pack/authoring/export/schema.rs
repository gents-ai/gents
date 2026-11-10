use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::store::config_client::{preview_schema_install, ConfigAccess};

/// The SDL of one live application collection, checked to describe exactly
/// the collection it was read from: a definition this renderer cannot express
/// (a relation, a default, a view) is refused rather than shipped lossy.
pub(super) async fn collection_sdl(access: &ConfigAccess, collection: &str) -> Result<String> {
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
    Ok(sdl)
}

fn render(version: &Value) -> Result<String> {
    let name = version["Name"].as_str().context("has no name")?;
    anyhow::ensure!(
        version.get("Query").is_none_or(Value::is_null),
        "is a view; pack export carries collections only"
    );
    for kind in ["EncryptedIndexes", "FullTextIndexes"] {
        anyhow::ensure!(
            version[kind].as_array().is_none_or(Vec::is_empty),
            "has {kind}, which pack export does not write"
        );
    }
    let mut field_indexes: BTreeMap<&str, String> = BTreeMap::new();
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
                field_indexes.insert(name, directive);
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
        if let Some(index) = field_indexes.get(field_name) {
            sdl.push(' ');
            sdl.push_str(index);
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
        let source = "type TicketNote @index(fields: [\"queue\", \"ticket\"], unique: true) {\n  ticket: String @index(unique: true) @immutable\n  queue: String @index\n  body: String\n  score: Int\n  seen_at: DateTime @index(direction: DESC)\n  labels: [String!]\n}\n";
        let parsed = query::parse_sdl(source).unwrap().remove(0);
        let rendered = render(&serde_json::to_value(&parsed).unwrap()).unwrap();
        let reparsed = query::parse_sdl(&rendered).unwrap().remove(0);
        let digest = |version: &schema::CollectionVersion| {
            crate::store::config_client::collection_schema_contract_digest(
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
