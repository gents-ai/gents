//! JSON Schemas of what a user authors: a pack's `pack_config.json` and its
//! `manifest.json`, and each canonical configuration document they hold, as
//! the pack loader accepts them.

use gents::pack::{authored_document_schema, authored_pack_config_schema, PackManifest};
use gents::Collection;
use schemars::{schema_for, Schema};
use serde::Serialize;
use serde_json::Value;

pub(crate) struct ConfigDocument {
    pub(crate) id: String,
    pub(crate) schema: Schema,
}

#[derive(Debug, Serialize)]
pub(crate) struct ConfigIndexEntry {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) schema: String,
}

impl ConfigDocument {
    fn new(id: impl Into<String>, schema: Schema) -> Self {
        Self {
            id: id.into(),
            schema,
        }
    }

    pub(crate) fn index_entry(&self) -> ConfigIndexEntry {
        let text = |key: &str| {
            self.schema
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        ConfigIndexEntry {
            title: text("title"),
            description: text("description"),
            schema: format!("config/{}.schema.json", self.id),
            id: self.id.clone(),
        }
    }
}

/// The pack root first, then every canonical collection in apply order, then
/// the pack manifest.
pub(crate) fn config_reference() -> Vec<ConfigDocument> {
    let mut documents = Vec::with_capacity(Collection::ALL.len() + 2);
    documents.push(ConfigDocument::new(
        "pack_config",
        authored_pack_config_schema(),
    ));
    documents.extend(Collection::ALL.into_iter().map(|collection| {
        ConfigDocument::new(
            document_id(collection),
            authored_document_schema(collection),
        )
    }));
    documents.push(ConfigDocument::new(
        "pack_manifest",
        schema_for!(PackManifest),
    ));
    documents
}

/// The collection's GraphQL type name in snake_case.
pub(crate) fn document_id(collection: Collection) -> String {
    let mut id = String::new();
    for (index, character) in collection.graphql_type().char_indices() {
        if character.is_ascii_uppercase() && index > 0 {
            id.push('_');
        }
        id.push(character.to_ascii_lowercase());
    }
    id
}
