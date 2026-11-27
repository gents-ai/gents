//! Substitutes an authored pack config's `${NAME}` and `${NAME:-default}`
//! references. Where the authored schema marks a field as holding a number or
//! a boolean, the substituted text is read as that scalar, and text that is
//! not one is refused naming the field and its variables; everywhere else a
//! substitution stays text. The field's type, never the spelling of a value,
//! decides.

use std::sync::LazyLock;

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::authored_schema::{authored_pack_config_schema, SubstitutionKind, SUBSTITUTION_KIND};
use crate::pack::interpolate::{self, interpolate_with_uses, UnsetVariables};

/// Deeper than any authored document nests; a self-referential schema stops.
const DEPTH_LIMIT: usize = 32;

/// Where the authored schema describes nothing, so a value keeps string
/// substitution.
static UNKNOWN_SCHEMA: Value = Value::Null;

/// The authored schema, generated once: the same one
/// [`authored_pack_config_schema`] exports, so the field types this loader
/// reads are the ones a reader validates against.
static SCHEMA: LazyLock<Value> = LazyLock::new(|| authored_pack_config_schema().as_value().clone());

pub(super) fn interpolate_values(
    value: &mut Value,
    environment: &dyn Fn(&str) -> Option<String>,
) -> Result<()> {
    Substituter {
        root: &SCHEMA,
        environment,
    }
    .apply(value, &SCHEMA, "")
}

struct Substituter<'a> {
    root: &'a Value,
    environment: &'a dyn Fn(&str) -> Option<String>,
}

impl Substituter<'_> {
    fn apply(&self, value: &mut Value, schema: &Value, path: &str) -> Result<()> {
        match value {
            Value::String(text) => {
                let kind = self.kind(schema).filter(|_| interpolate::substitutes(text));
                let (resolved, used) = interpolate_with_uses(text, self.environment)
                    .map_err(|names| UnsetVariables { names })?;
                match kind {
                    Some(kind) => {
                        let Some(scalar) = kind.parse(&resolved) else {
                            return Err(anyhow!(
                                "pack config field {path} takes a {}, but {} resolved to {resolved:?}",
                                kind.annotation(),
                                variables(&used)
                            ));
                        };
                        *value = scalar;
                    }
                    None => *text = resolved,
                }
                Ok(())
            }
            Value::Array(items) => {
                for (index, item) in items.iter_mut().enumerate() {
                    let child = self.index(schema, index).unwrap_or(&UNKNOWN_SCHEMA);
                    self.apply(item, child, &format!("{path}/{index}"))?;
                }
                Ok(())
            }
            Value::Object(fields) => {
                for (key, field) in fields.iter_mut() {
                    let child = self.property(schema, key).unwrap_or(&UNKNOWN_SCHEMA);
                    let escaped = key.replace('~', "~0").replace('/', "~1");
                    self.apply(field, child, &format!("{path}/{escaped}"))?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Follows `$ref`s up to the definition that describes the field.
    fn resolve<'s>(&'s self, mut schema: &'s Value) -> &'s Value {
        for _ in 0..DEPTH_LIMIT {
            let Some(name) = schema
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|reference| reference.strip_prefix("#/$defs/"))
            else {
                break;
            };
            let Some(definition) = self
                .root
                .get("$defs")
                .and_then(|definitions| definitions.get(name))
            else {
                break;
            };
            schema = definition;
        }
        schema
    }

    /// The scalar branches a schema node is a choice of, so a property or item
    /// inside one branch still types the value written there.
    fn branches<'s>(&'s self, schema: &'s Value) -> impl Iterator<Item = &'s Value> {
        let schema = self.resolve(schema);
        ["anyOf", "oneOf", "allOf"]
            .into_iter()
            .flat_map(move |keyword| schema.get(keyword).and_then(Value::as_array).into_iter())
            .flatten()
    }

    /// The scalar the field holds where it accepts a substitution as one, as
    /// `allow_substitutions` marked it: either on the field, or on the one
    /// scalar branch of a choice the field is narrowed to.
    fn kind(&self, schema: &Value) -> Option<SubstitutionKind> {
        self.kind_at_depth(schema, 0)
    }

    fn kind_at_depth(&self, schema: &Value, depth: usize) -> Option<SubstitutionKind> {
        if depth > DEPTH_LIMIT {
            return None;
        }
        let schema = self.resolve(schema);
        if let Some(kind) = schema
            .get(SUBSTITUTION_KIND)
            .and_then(Value::as_str)
            .and_then(SubstitutionKind::from_annotation)
        {
            return Some(kind);
        }
        self.branches(schema)
            .find_map(|branch| self.kind_at_depth(branch, depth + 1))
    }

    fn property<'s>(&'s self, schema: &'s Value, key: &str) -> Option<&'s Value> {
        self.property_at_depth(schema, key, 0)
    }

    fn property_at_depth<'s>(
        &'s self,
        schema: &'s Value,
        key: &str,
        depth: usize,
    ) -> Option<&'s Value> {
        if depth > DEPTH_LIMIT {
            return None;
        }
        let schema = self.resolve(schema);
        if let Some(child) = schema
            .get("properties")
            .and_then(|properties| properties.get(key))
        {
            return Some(child);
        }
        if let Some(child) = self
            .branches(schema)
            .find_map(|branch| self.property_at_depth(branch, key, depth + 1))
        {
            return Some(child);
        }
        schema
            .get("additionalProperties")
            .filter(|value| value.is_object())
    }

    fn index<'s>(&'s self, schema: &'s Value, index: usize) -> Option<&'s Value> {
        self.index_at_depth(schema, index, 0)
    }

    fn index_at_depth<'s>(
        &'s self,
        schema: &'s Value,
        index: usize,
        depth: usize,
    ) -> Option<&'s Value> {
        if depth > DEPTH_LIMIT {
            return None;
        }
        let schema = self.resolve(schema);
        if let Some(child) = schema.get("items").filter(|items| items.is_object()) {
            return Some(child);
        }
        if let Some(child) = schema
            .get("prefixItems")
            .and_then(Value::as_array)
            .and_then(|items| items.get(index))
        {
            return Some(child);
        }
        self.branches(schema)
            .find_map(|branch| self.index_at_depth(branch, index, depth + 1))
    }
}

fn variables(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("${{{name}}}"))
        .collect::<Vec<_>>()
        .join(", ")
}
