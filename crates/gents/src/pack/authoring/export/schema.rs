use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::store::config_client::{preview_schema_install, ConfigAccess};

/// The SDL that carries a set of live application collections.
#[derive(Debug, Default)]
pub(crate) struct SchemaExport {
    /// One SDL per group of collections that relate to each other, keyed by
    /// the group's first collection by name. Install publishes each schema
    /// file on its own, and a type cannot name a type another file defines,
    /// so both sides of a relation travel in one file.
    pub(crate) files: BTreeMap<String, String>,
    /// The collections added because an exported collection relates to them,
    /// each with the relation that named it.
    pub(crate) pulled: Vec<(String, String)>,
}

/// The SDL of the live application collections `collections` and of every
/// collection they relate to, checked to describe exactly the collections it
/// was read from: a definition this renderer cannot express is refused rather
/// than shipped lossy, and a relation whose other side cannot be exported (a
/// view, a Gents-managed collection, one this node no longer has) is refused
/// by name.
pub(crate) async fn collection_schemas(
    access: &ConfigAccess,
    collections: &BTreeSet<String>,
) -> Result<SchemaExport> {
    if collections.is_empty() {
        return Ok(SchemaExport::default());
    }
    let versions = access.collection_versions().await?;
    let mut definitions = BTreeMap::<String, Value>::new();
    let mut relations = Vec::<(String, String)>::new();
    let mut pulled = Vec::new();
    let mut pending = collections
        .iter()
        .map(|collection| (collection.clone(), None::<String>))
        .collect::<Vec<_>>();
    while let Some((collection, via)) = pending.pop() {
        if definitions.contains_key(&collection) {
            continue;
        }
        let named = |problem: &str| {
            match &via {
            Some(via) => format!("{via} relates to collection {collection}, which {problem}; export it without that relation, or leave {collection}'s side out of the pack's data"),
            None => format!("collection {collection} {problem}"),
        }
        };
        anyhow::ensure!(
            via.is_none()
                || !crate::store::schema::migration::DEFAULT_REGISTRY
                    .managed_names()
                    .any(|name| name == collection),
            "{}",
            named("is Gents-managed and a pack cannot carry it")
        );
        let version = access
            .collection_version(&collection)
            .await?
            .with_context(|| named("is not on this node"))?;
        anyhow::ensure!(
            version.get("Query").is_none_or(Value::is_null),
            "{}",
            named(VIEW_REFUSAL)
        );
        for field in version["Fields"].as_array().into_iter().flatten() {
            let Some(field_name) = field["Name"].as_str().filter(|name| !name.starts_with('_'))
            else {
                continue;
            };
            if !field["Kind"].is_object() {
                continue;
            }
            let target = relation_target(&field["Kind"], &version, &versions).with_context(|| {
                format!("collection {collection}: field {field_name} relates to a collection this node does not list, so pack export cannot name it")
            })?;
            if !definitions.contains_key(&target) && !collections.contains(&target) {
                pending.push((
                    target.clone(),
                    Some(format!("collection/{collection} field {field_name}")),
                ));
            }
            relations.push((collection.clone(), target));
        }
        if let Some(via) = via {
            pulled.push((collection.clone(), via));
        }
        definitions.insert(collection, version);
    }
    pulled.sort();

    // Collections joined by a relation, directly or through others, share a group.
    let mut group: BTreeMap<&str, &str> = definitions
        .keys()
        .map(|name| (name.as_str(), name.as_str()))
        .collect();
    for (source, target) in &relations {
        let (from, to) = (group[source.as_str()], group[target.as_str()]);
        if from != to {
            let (keep, replace) = if from < to { (from, to) } else { (to, from) };
            for label in group.values_mut() {
                if *label == replace {
                    *label = keep;
                }
            }
        }
    }
    let mut files = BTreeMap::new();
    for first in group.values().copied().collect::<BTreeSet<_>>() {
        let members = group
            .iter()
            .filter(|(_, label)| **label == first)
            .map(|(name, _)| *name)
            .collect::<Vec<_>>();
        let described = members.join(", ");
        let mut sdl = String::new();
        for name in &members {
            if !sdl.is_empty() {
                sdl.push('\n');
            }
            sdl.push_str(
                &render(&definitions[*name], &definitions, &versions)
                    .with_context(|| format!("collection {name}"))?,
            );
        }
        let unwritable =
            || format!("collection {described} has a definition pack export cannot write as SDL");
        preview_schema_install(access, &sdl)
            .await
            .with_context(unwritable)?
            .require_satisfied()
            .with_context(unwritable)?;
        for written in query::parse_sdl(&sdl)? {
            let written = serde_json::to_value(written)?;
            let name = written["Name"].as_str().unwrap_or_default();
            let live = definitions
                .get(name)
                .with_context(|| format!("the SDL written for {described} declares {name}"))?;
            for kind in FIELD_INDEX_KINDS {
                anyhow::ensure!(
                    field_indexes(&written, kind) == field_indexes(live, kind),
                    "collection {name} has {kind} pack export cannot write as SDL"
                );
            }
        }
        files.insert(first.to_owned(), sdl);
    }
    Ok(SchemaExport { files, pulled })
}

/// A view is a query over collections, optionally through a lens transform.
/// The node keeps only the planned form of the query (no order, grouping,
/// limit or offset, and no source text), exposes no way to read a transform's
/// bytes back, and creates a view outside any transaction, so an install
/// could neither rebuild the view its author wrote nor roll it back.
const VIEW_REFUSAL: &str = "is a view, which a pack cannot carry: the node keeps a view's query only as its plan and does not give back a transform's bytes, and pack install has no step that creates a view. Create the view where the pack is installed with the schema tool's [\"view\",\"create\"]";

/// The collection a relation field's kind names: by name, by collection ID,
/// or by its position in the set of collections that refer to each other.
fn relation_target(kind: &Value, owner: &Value, versions: &[Value]) -> Option<String> {
    if let Some(name) = kind.get("Name").and_then(Value::as_str) {
        return Some(name.to_owned());
    }
    let named = |version: &Value| version["Name"].as_str().map(ToOwned::to_owned);
    if let Some(id) = kind.get("CollectionID").and_then(Value::as_str) {
        return versions
            .iter()
            .find(|version| version["CollectionID"].as_str() == Some(id))
            .and_then(named);
    }
    let relative = kind.get("RelativeID")?.as_str()?;
    let set = owner.get("CollectionSet")?.get("CollectionSetID")?;
    versions
        .iter()
        .find(|version| {
            &version["CollectionSet"]["CollectionSetID"] == set
                && version["CollectionSet"]["RelativeID"]
                    .as_i64()
                    .map(|id| id.to_string())
                    .as_deref()
                    == Some(relative)
        })
        .and_then(named)
}

/// Whether the other side of `field`'s relation holds one document: with a
/// primary single side that is a one-to-one relation, whose foreign-key
/// index DefraDB creates itself.
fn counterpart_is_single(owner: &str, field: &Value, target: &str, other: Option<&Value>) -> bool {
    other
        .and_then(|other| other["Fields"].as_array())
        .into_iter()
        .flatten()
        .any(|candidate| {
            candidate["RelationName"] == field["RelationName"]
                && candidate["Kind"].is_object()
                && candidate["Kind"]["Array"].as_bool() != Some(true)
                && !(owner == target && candidate["Name"] == field["Name"])
        })
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

/// `version` as one SDL type. `definitions` are the collections written with
/// it and `versions` every collection the node lists, which name the targets
/// of its relations.
fn render(
    version: &Value,
    definitions: &BTreeMap<String, Value>,
    versions: &[Value],
) -> Result<String> {
    let name = version["Name"].as_str().context("has no name")?;
    let fulltext = field_indexes(version, "FullTextIndexes");
    let encrypted = field_indexes(version, "EncryptedIndexes");
    let mut plain_indexes: BTreeMap<&str, (String, bool)> = BTreeMap::new();
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
                plain_indexes.insert(name, (directive, unique));
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
        let kind = &field["Kind"];
        let foreign_key = format!("_{field_name}ID");
        let mut index = plain_indexes.get(field_name);
        if kind.is_object() {
            let target = relation_target(kind, version, versions).with_context(|| {
                format!("field {field_name} relates to a collection this node does not list")
            })?;
            let array = kind["Array"].as_bool() == Some(true);
            let primary = !array && field["IsPrimary"].as_bool() == Some(true);
            sdl.push_str(&if array {
                format!("  {field_name}: [{target}]")
            } else {
                format!("  {field_name}: {target}")
            });
            if primary {
                sdl.push_str(" @primary");
            }
            if let Some(relation) = field["RelationName"]
                .as_str()
                .filter(|name| !name.is_empty())
            {
                sdl.push_str(&format!(
                    " @relation(name: {})",
                    Value::String(relation.to_owned())
                ));
            }
            // DefraDB stores an index on a single relation field under its
            // foreign key, and creates the unique one of a one-to-one
            // relation itself.
            index = plain_indexes
                .get(foreign_key.as_str())
                .filter(|(_, unique)| {
                    !(*unique
                        && primary
                        && counterpart_is_single(name, field, &target, definitions.get(&target)))
                });
        } else {
            let kind: schema::FieldKind = serde_json::from_value(kind.clone())
                .with_context(|| format!("field {field_name} has an unknown kind"))?;
            sdl.push_str(&format!("  {field_name}: {}", kind.graphql_type_name()));
        }
        if let Some((index, _)) = index {
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
        let rendered = render(&live, &BTreeMap::new(), &[]).unwrap();
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
            crate::store::config_client::collection_schema_contract_digest(
                &serde_json::to_value(version).unwrap(),
            )
            .unwrap()
        };
        assert_eq!(digest(&reparsed), digest(&parsed), "{rendered}");
    }

    /// Both sides of a relation are written, and the types parse back to
    /// the relation they were read from.
    #[test]
    fn related_types_render_both_sides_and_parse_back() {
        for source in [
            "type Author { name: String books: [Book] }\ntype Book { title: String author: Author @index }",
            "type Person { name: String badge: Badge @primary }\ntype Badge { code: String holder: Person }",
            "type Folder { name: String parent: Folder children: [Folder] }",
        ] {
            let parsed = query::parse_sdl(source).unwrap();
            let live = parsed
                .iter()
                .map(|version| serde_json::to_value(version).unwrap())
                .collect::<Vec<_>>();
            let definitions = live
                .iter()
                .map(|version| (version["Name"].as_str().unwrap().to_owned(), version.clone()))
                .collect::<BTreeMap<_, _>>();
            let rendered = definitions
                .values()
                .map(|version| render(version, &definitions, &live).unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            let digests = |versions: &[schema::CollectionVersion]| {
                versions
                    .iter()
                    .map(|version| {
                        let version = serde_json::to_value(version).unwrap();
                        (
                            version["Name"].as_str().unwrap().to_owned(),
                            crate::store::config_client::collection_schema_contract_digest(&version)
                                .unwrap(),
                        )
                    })
                    .collect::<BTreeMap<_, _>>()
            };
            assert_eq!(
                digests(&query::parse_sdl(&rendered).unwrap()),
                digests(&parsed),
                "{rendered}"
            );
        }
    }
}
