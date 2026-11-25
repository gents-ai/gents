use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::query::is_restricted_field;
use super::CollectionScope;
use gents_store::config_client::schema_introspection::{
    is_aggregate_pseudo_field, parse_collection_schema, validate_identifier, CollectionSchema,
};
use gents_store::config_client::ConfigRead;
use gents_store::graphql::escape_graphql_string;

/// Lean `ToolPolicy.QuerySelection.Kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Value,
    Relation,
    Unavailable,
}

/// Lean `ToolPolicy.QuerySelection.Decision`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Decision {
    Select,
    Unavailable,
    NotARelation,
    OutsideGrant,
    NeedsField,
    TooDeep,
}

/// Lean `ToolPolicy.QuerySelection.Ranking`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Ranking {
    Bm25,
    Similarity,
    Refused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    pub kind: Kind,
    pub granted: bool,
}

/// Lean `ToolPolicy.QuerySelection.maxRelations`.
const MAX_RELATIONS: usize = 1;
/// Lean `ToolPolicy.QuerySelection.listLimit`.
pub(crate) const LIST_LIMIT: usize = 20;

/// Lean `ToolPolicy.QuerySelection.decide`.
pub(crate) fn decide(segments: &[Segment]) -> (Decision, usize) {
    let mut crossed = 0;
    for (index, segment) in segments.iter().enumerate() {
        let last = index + 1 == segments.len();
        match segment.kind {
            Kind::Unavailable => return (Decision::Unavailable, index),
            Kind::Value if last => return (Decision::Select, index),
            Kind::Value => return (Decision::NotARelation, index),
            Kind::Relation if !segment.granted => return (Decision::OutsideGrant, index),
            Kind::Relation if last => return (Decision::NeedsField, index),
            Kind::Relation if crossed >= MAX_RELATIONS => return (Decision::TooDeep, index),
            Kind::Relation => crossed += 1,
        }
    }
    (Decision::Unavailable, 0)
}

/// Lean `ToolPolicy.QuerySelection.relatedPage`.
pub(crate) fn related_page(related: &mut Vec<Value>) -> bool {
    let more = related.len() > LIST_LIMIT;
    related.truncate(LIST_LIMIT);
    more
}

/// Lean `ToolPolicy.QuerySelection.ranking`.
pub(crate) fn ranking(keywords: bool, vector: bool) -> Ranking {
    match (keywords, vector) {
        (true, false) => Ranking::Bm25,
        (false, true) => Ranking::Similarity,
        _ => Ranking::Refused,
    }
}

#[derive(Clone, Debug, Deserialize)]
struct TypeRef {
    kind: String,
    name: Option<String>,
    #[serde(rename = "ofType")]
    of_type: Option<Box<TypeRef>>,
}

impl TypeRef {
    fn unwrap_non_null(&self) -> &TypeRef {
        match (&*self.kind, self.of_type.as_deref()) {
            ("NON_NULL", Some(inner)) => inner,
            _ => self,
        }
    }

    fn display(&self) -> String {
        match (&*self.kind, self.of_type.as_deref(), &self.name) {
            ("NON_NULL", Some(inner), _) => format!("{}!", inner.display()),
            ("LIST", Some(inner), _) => format!("[{}]", inner.display()),
            (_, _, Some(name)) => name.clone(),
            (kind, _, None) => kind.to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    Value { numeric_list: bool },
    One(String),
    Many(String),
}

fn shape(ty: &TypeRef) -> Shape {
    let outer = ty.unwrap_non_null();
    let (inner, list) = match (&*outer.kind, outer.of_type.as_deref()) {
        ("LIST", Some(item)) => (item.unwrap_non_null(), true),
        _ => (outer, false),
    };
    match (&*inner.kind, &inner.name) {
        ("OBJECT", Some(target)) if list => Shape::Many(target.clone()),
        ("OBJECT", Some(target)) => Shape::One(target.clone()),
        (_, name) => Shape::Value {
            numeric_list: list
                && matches!(
                    name.as_deref(),
                    Some("Float32" | "Float64" | "Float" | "Int")
                ),
        },
    }
}

#[derive(Deserialize)]
struct OutputField {
    name: String,
    #[serde(rename = "type")]
    type_ref: TypeRef,
}

#[derive(Clone, Debug)]
pub(crate) struct Observed {
    pub collection: String,
    pub schema: CollectionSchema,
    shapes: BTreeMap<String, (Shape, String)>,
}

impl Observed {
    pub fn shape(&self, field: &str) -> Option<&Shape> {
        self.shapes.get(field).map(|(shape, _)| shape)
    }

    pub fn display(&self, field: &str) -> Option<&str> {
        self.shapes.get(field).map(|(_, display)| display.as_str())
    }

    pub fn visible(&self) -> Vec<String> {
        super::schema::visible_fields(&self.schema, &self.collection)
            .iter()
            .map(|field| field.name.clone())
            .collect()
    }

    fn readable(&self, field: &str) -> bool {
        self.shapes.contains_key(field)
            && !is_restricted_field(&self.collection, field)
            && !is_aggregate_pseudo_field(field)
            && (!field.starts_with('_') || field == "_docID")
    }

    pub fn segment(&self, field: &str, scope: &CollectionScope) -> (Segment, Option<String>) {
        match self.shape(field).filter(|_| self.readable(field)) {
            None => (
                Segment {
                    kind: Kind::Unavailable,
                    granted: false,
                },
                None,
            ),
            Some(Shape::Value { .. }) => (
                Segment {
                    kind: Kind::Value,
                    granted: false,
                },
                None,
            ),
            Some(Shape::One(target) | Shape::Many(target)) => (
                Segment {
                    kind: Kind::Relation,
                    granted: scope.ensure_allowed(target).is_ok(),
                },
                Some(target.clone()),
            ),
        }
    }
}

pub(crate) fn introspection(collection: &str) -> Result<String> {
    validate_identifier(collection)?;
    Ok(format!(
        "{{ __type(name: \"{}\") {{ fields {{ name type {{ kind name ofType {{ kind name ofType {{ kind name ofType {{ kind name }} }} }} }} }} }} }}",
        escape_graphql_string(collection)
    ))
}

pub(crate) fn parse(collection: &str, response: &Value) -> Option<Observed> {
    let data = response.get("data");
    let schema = parse_collection_schema(data)?;
    let fields: Vec<OutputField> =
        serde_json::from_value(data?.get("__type")?.get("fields")?.clone()).ok()?;
    Some(Observed {
        collection: collection.to_owned(),
        schema,
        shapes: fields
            .into_iter()
            .map(|field| {
                let display = field.type_ref.display();
                (field.name, (shape(&field.type_ref), display))
            })
            .collect(),
    })
}

pub(crate) async fn observe(access: &dyn ConfigRead, collection: &str) -> Result<Option<Observed>> {
    let response = access.execute_read(&introspection(collection)?).await?;
    Ok(parse(collection, &response))
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct SelectionRefusal {
    pub message: String,
    pub next_call: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct RelationSelection {
    pub relation: String,
    pub many: bool,
    pub fields: Vec<String>,
}

fn fields_call(collection: &str) -> Value {
    json!({"tool":"query","args":{"argv":["fields"],"collection":collection}})
}

fn refusal(
    decision: Decision,
    path: &str,
    owner: &Observed,
    segment: &str,
    target: Option<&str>,
    scope: &CollectionScope,
) -> SelectionRefusal {
    let collection = owner.collection.as_str();
    let (message, next_call) = match decision {
        Decision::Unavailable => {
            let visible = owner.visible();
            let hint = super::schema::suggest_fields(segment, &visible);
            let hint = if hint.is_empty() {
                String::new()
            } else {
                let hint: Vec<String> = hint.iter().map(|name| format!("{name:?}")).collect();
                format!(" (did you mean {}?)", hint.join(" or "))
            };
            (
                format!("{path}: {collection} has no readable field {segment:?}{hint}"),
                fields_call(collection),
            )
        }
        Decision::NotARelation => (
            format!("{path}: {collection}.{segment} is a value, not a relationship; select it as {segment}"),
            fields_call(collection),
        ),
        Decision::OutsideGrant => {
            let target = target.unwrap_or_default();
            let reason = scope
                .ensure_allowed(target)
                .err()
                .map(|error| format!("{error:#}"))
                .unwrap_or_default();
            let message = format!("{path}: relationship {collection}.{segment} reads {target}; {reason}");
            if gents_documents::reject_protected_collection_name(target).is_err() {
                (message, json!({"tool":"query","args":{"argv":["help","find"]}}))
            } else {
                (
                    format!("{message}. Reading it needs {target} in datastore.defra_query_collections"),
                    json!({"tool":"config","args":{"argv":["help","tools"]}}),
                )
            }
        }
        Decision::NeedsField => {
            let target = target.unwrap_or_default();
            (
                format!("{path} is a relationship to {target}; select its fields as {segment}.FIELD, for example {segment}._docID"),
                fields_call(target),
            )
        }
        Decision::TooDeep => {
            let target = target.unwrap_or(collection);
            (
                format!("{path} crosses more than one relationship; query {target} to follow {collection}.{segment}"),
                fields_call(target),
            )
        }
        Decision::Select => unreachable!("a selected path is not refused"),
    };
    SelectionRefusal { message, next_call }
}

pub(crate) async fn resolve(
    access: &dyn ConfigRead,
    root: &Observed,
    requested: &[String],
    scope: &CollectionScope,
) -> std::result::Result<(Vec<String>, Vec<RelationSelection>), ResolveError> {
    let mut plain = Vec::new();
    let mut relations: Vec<RelationSelection> = Vec::new();
    let mut targets: BTreeMap<String, Observed> = BTreeMap::new();
    for path in requested {
        let names: Vec<&str> = path.split('.').collect();
        if names.len() == 1
            && root
                .shape(path)
                .is_none_or(|shape| matches!(shape, Shape::Value { .. }))
        {
            plain.push(path.clone());
            continue;
        }
        for name in &names {
            validate_identifier(name)
                .map_err(|error| anyhow::anyhow!("invalid field name {path:?}: {error}"))?;
        }
        let mut segments = Vec::with_capacity(names.len());
        let mut target_names: Vec<Option<String>> = Vec::with_capacity(names.len());
        let mut owner_key: Option<String> = None;
        let mut observing = true;
        for (index, name) in names.iter().enumerate() {
            if !observing {
                segments.push(Segment {
                    kind: Kind::Unavailable,
                    granted: false,
                });
                target_names.push(None);
                continue;
            }
            let owner = owner_key.as_ref().map_or(root, |key| &targets[key]);
            let (segment, target) = owner.segment(name, scope);
            segments.push(segment);
            target_names.push(target.clone());
            observing = segment.kind == Kind::Relation
                && segment.granted
                && index < MAX_RELATIONS
                && index + 1 < names.len();
            if observing {
                let target = target.context("relationship without target")?;
                if !targets.contains_key(&target) {
                    let observed = observe(access, &target)
                        .await?
                        .with_context(|| format!("relationship target {target} has no schema"))?;
                    targets.insert(target.clone(), observed);
                }
                owner_key = Some(target);
            }
        }
        let (decision, index) = decide(&segments);
        if decision != Decision::Select {
            let owner = if index == 0 {
                root
            } else {
                target_names[index - 1]
                    .as_ref()
                    .and_then(|target| targets.get(target))
                    .unwrap_or(root)
            };
            return Err(refusal(
                decision,
                path,
                owner,
                names[index],
                target_names[index].as_deref(),
                scope,
            )
            .into());
        }
        if names.len() == 1 {
            plain.push(path.clone());
            continue;
        }
        let relation = names[0].to_owned();
        let many = matches!(root.shape(&relation), Some(Shape::Many(_)));
        let field = names[1].to_owned();
        match relations
            .iter_mut()
            .find(|entry| entry.relation == relation)
        {
            Some(entry) if !entry.fields.contains(&field) => entry.fields.push(field),
            Some(_) => {}
            None => relations.push(RelationSelection {
                relation,
                many,
                fields: vec![field],
            }),
        }
    }
    Ok((plain, relations))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolveError {
    #[error(transparent)]
    Refused(#[from] SelectionRefusal),
    #[error(fmt = gents_protocol::error_chain::held)]
    Other(#[from] anyhow::Error),
}

pub(crate) fn render(relations: &[RelationSelection]) -> String {
    relations
        .iter()
        .map(|relation| {
            let arguments = if relation.many {
                format!("(order: {{_docID: ASC}}, limit: {})", LIST_LIMIT + 1)
            } else {
                String::new()
            };
            format!(
                " {}{arguments} {{ {} }}",
                relation.relation,
                relation.fields.join(" ")
            )
        })
        .collect()
}

pub(crate) fn page_related(rows: &mut Value, relations: &[RelationSelection]) -> Vec<Value> {
    let mut truncated = Vec::new();
    for (row, value) in rows.as_array_mut().into_iter().flatten().enumerate() {
        for relation in relations.iter().filter(|relation| relation.many) {
            if let Some(related) = value
                .get_mut(&relation.relation)
                .and_then(Value::as_array_mut)
            {
                if related_page(related) {
                    truncated
                        .push(json!({"row":row,"relation":relation.relation,"shown":LIST_LIMIT}));
                }
            }
        }
    }
    truncated
}

#[cfg(test)]
#[path = "relation_tests.rs"]
mod tests;
