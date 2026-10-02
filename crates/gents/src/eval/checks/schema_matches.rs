use serde::Deserialize;
use serde_json::{json, Value};

use super::{graded, graded_reason_codes, grader, Check, CheckDescription, CheckVerdict};
use crate::eval::runner::{CaptureResult, StageEvidence};

/// Grades persisted schema requirements derived by the pinned native SDL parser.
/// Additional fields/indexes and generated identifiers are not requirements.
/// This checks deployment, not query execution or index performance.
pub struct SchemaMatches;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    name: String,
    sdl: String,
    #[serde(default)]
    exact_nullability: bool,
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn kind(field: &Value, owner: &Value, versions: &[Value], exact_nullability: bool) -> Value {
    let kind = &field["Kind"];
    if !kind.is_object() {
        return serde_json::from_value::<schema::FieldKind>(kind.clone())
            .map(|k| {
                let name = k.graphql_type_name();
                json!(if exact_nullability {
                    name.to_owned()
                } else {
                    name.replace('!', "")
                })
            })
            .unwrap_or_else(|_| kind.clone());
    }
    let target = kind
        .get("Name")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            let id = kind.get("CollectionID")?.as_str()?;
            versions
                .iter()
                .find(|v| v["CollectionID"].as_str() == Some(id))?
                .get("Name")?
                .as_str()
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            let relative = kind.get("RelativeID")?.as_str()?;
            let set = owner.get("CollectionSet")?.get("CollectionSetID")?;
            versions
                .iter()
                .find(|v| {
                    &v["CollectionSet"]["CollectionSetID"] == set
                        && v["CollectionSet"]["RelativeID"]
                            .as_i64()
                            .map(|x| x.to_string())
                            .as_deref()
                            == Some(relative)
                })?
                .get("Name")?
                .as_str()
                .map(ToOwned::to_owned)
        });
    target
        .map(|name| json!({"Name":name,"Array":kind["Array"]}))
        .unwrap_or_else(|| kind.clone())
}

fn field_contract(
    field: &Value,
    owner: &Value,
    versions: &[Value],
    exact_nullability: bool,
) -> Value {
    let mut contract = json!({"Kind":kind(field, owner, versions, exact_nullability),
        "IsPrimary": field["IsPrimary"].as_bool().unwrap_or(false)});
    if !field["DefaultValue"].is_null() {
        contract["DefaultValue"] = field["DefaultValue"].clone();
    }
    if field["Immutable"].as_bool() == Some(true) {
        contract["Immutable"] = json!(true);
    }
    if field["Size"].as_u64().unwrap_or(0) > 0 {
        contract["Size"] = field["Size"].clone();
    }
    contract
}

fn index_contract(index: &Value, category: &str) -> Value {
    if category == "FullTextIndexes" {
        return json!({"FieldName":index["FieldName"],"Language":index["Language"]});
    }
    json!({"Fields":index["Fields"],"Unique":index["Unique"],
        "Kind":index["Kind"],"KindDescription":index["KindDescription"]})
}

impl Check for SchemaMatches {
    fn name(&self) -> &'static str {
        "schema_matches"
    }
    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        let params: Params = match serde_json::from_value(params.clone()) {
            Ok(params) => params,
            Err(error) => return grader("bad_params", error.to_string()),
        };
        let expected: Vec<Value> = match query::parse_sdl(&params.sdl)
            .map_err(anyhow::Error::from)
            .and_then(|versions| {
                Ok(versions
                    .iter()
                    .map(serde_json::to_value)
                    .collect::<Result<Vec<Value>, _>>()?)
            }) {
            Ok(versions) if !versions.is_empty() => versions,
            Ok(_) => return grader("bad_params", "SDL must declare a collection"),
            Err(error) => return grader("bad_params", error.to_string()),
        };
        let actual = match stage.captures.get(&params.name) {
            Some(CaptureResult::Schema { collections }) => collections,
            _ => {
                return grader(
                    "missing_capture",
                    format!("no schema capture named {}", params.name),
                )
            }
        };
        let mut satisfied = 0;
        let mut total = 0;
        let mut unmet = Vec::new();
        let mut categories = std::collections::BTreeMap::<&str, (usize, usize)>::new();
        let mut requirement = |category: &'static str, holds: bool, text: String| {
            let count = categories.entry(category).or_default();
            count.0 += usize::from(holds);
            count.1 += 1;
            total += 1;
            if holds {
                satisfied += 1;
            } else {
                unmet.push(text);
            }
        };
        for collection in &expected {
            let name = collection["Name"].as_str().unwrap_or_default();
            let live = actual.iter().find(|v| v["Name"] == collection["Name"]);
            requirement(
                "collections",
                live.is_some(),
                format!("collection {name} is missing"),
            );
            for field in array(collection, "Fields")
                .iter()
                .filter(|f| f["Name"] != "_docID")
            {
                let observed = live.and_then(|v| {
                    array(v, "Fields")
                        .iter()
                        .find(|f| f["Name"] == field["Name"])
                        .map(|f| (v, f))
                });
                let wanted = field_contract(field, collection, &expected, params.exact_nullability);
                requirement(
                    "fields",
                    observed.is_some_and(|(v, f)| {
                        wanted.as_object().unwrap().iter().all(|(k, value)| {
                            field_contract(f, v, actual, params.exact_nullability).get(k)
                                == Some(value)
                        })
                    }),
                    format!(
                        "{name}.{} has a missing or different field contract",
                        field["Name"].as_str().unwrap_or_default()
                    ),
                );
            }
            for category in ["Indexes", "FullTextIndexes"] {
                for index in array(collection, category) {
                    let wanted = index_contract(index, category);
                    requirement(
                        "indexes",
                        live.is_some_and(|v| {
                            array(v, category)
                                .iter()
                                .any(|i| index_contract(i, category) == wanted)
                        }),
                        format!("{name} is missing {category} {wanted}"),
                    );
                }
            }
        }
        let feedback = (!unmet.is_empty()).then(|| unmet.join("; "));
        let mut verdict = graded(satisfied, total, feedback);
        verdict.raw["unmet"] = json!(unmet);
        verdict.raw["capture"] = json!(params.name);
        verdict.raw["categories"] = categories
            .into_iter()
            .map(|(name, (satisfied, total))| {
                (
                    name.to_owned(),
                    json!({"satisfied":satisfied,"total":total}),
                )
            })
            .collect::<serde_json::Map<_, _>>()
            .into();
        verdict
    }

    fn describe(&self) -> CheckDescription {
        CheckDescription {
            name: self.name().into(), version: self.version().into(),
            summary: "Checks active saved fields, relationships and indexes against native-parsed SDL. Allows extra fields/indexes, defaults not explicitly required, and arbitrary generated IDs or index names. Nullability is ignored unless exact_nullability is true. Does not prove data access or search execution.".into(),
            params_schema: json!({"type":"object","additionalProperties":false,"required":["name","sdl"],"properties":{"name":{"type":"string","minLength":1},"sdl":{"type":"string","minLength":1},"exact_nullability":{"type":"boolean","default":false}}}),
            reads: vec!["capture:schema".into()],
            reason_codes: graded_reason_codes(&[("missing_capture","schema observation unavailable")]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_client::ConfigAccess;
    use crate::defra_node::EmbeddedNode;
    use crate::eval::runner::ScriptedExecutor;
    use crate::eval::OutcomeKind;
    use std::sync::Arc;

    #[test]
    fn unspecified_nullability_defaults_and_extra_fields_are_not_requirements() {
        let expected = "type Customer { name: String }";
        let actual = "type Customer { name: String! notes: String subscribed: Boolean @default(value: false) }";
        let mut stage = ScriptedExecutor::passed_evidence("did:test", "schema", "unused", vec![])
            .stages
            .remove(0);
        stage.captures.insert(
            "schema".into(),
            CaptureResult::Schema {
                collections: query::parse_sdl(actual)
                    .unwrap()
                    .iter()
                    .map(|v| serde_json::to_value(v).unwrap())
                    .collect(),
            },
        );
        let params = json!({"name":"schema", "sdl":expected});
        assert_eq!(
            SchemaMatches.evaluate(&params, &stage).kind,
            OutcomeKind::Passed
        );
        let strict = json!({"name":"schema", "sdl":expected, "exact_nullability":true});
        assert_eq!(
            SchemaMatches.evaluate(&strict, &stage).kind,
            OutcomeKind::ModelAcceptance
        );
    }

    #[tokio::test]
    async fn native_schema_evidence_grades_indexes_relations_and_wrong_contracts() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        let access = ConfigAccess::Local(node.clone());
        let sdl = r#"
          type Author { name: String books: [Book] }
          type Book {
            title: String @fulltext(language: "english")
            isbn: String @index(ordered: {unique:true})
            author: Author
            coordinates: [Float32!] @index(vector: {dimensions:3})
          }
        "#;
        access.add_schema(sdl).await.unwrap();
        let mut stage = ScriptedExecutor::passed_evidence("did:test", "schema", "unused", vec![])
            .stages
            .remove(0);
        let mut versions = Vec::new();
        for name in ["Author", "Book"] {
            versions.push(access.collection_version(name).await.unwrap().unwrap());
        }
        stage.captures.insert(
            "schema".into(),
            CaptureResult::Schema {
                collections: versions.clone(),
            },
        );
        let check = |sdl: &str, evidence: &StageEvidence| {
            SchemaMatches.evaluate(&json!({"name":"schema","sdl":sdl}), evidence)
        };
        let verdict = check(sdl, &stage);
        assert_eq!(verdict.kind, OutcomeKind::Passed, "{}", verdict.raw);
        for wrong in [
            sdl.replace("dimensions:3", "dimensions:4"),
            sdl.replace("language: \"english\"", "language: \"spanish\""),
            sdl.replace("name: String", "name: Int"),
            sdl.replace("books: [Book]", "books: Book @primary"),
        ] {
            let verdict = check(&wrong, &stage);
            assert_eq!(
                verdict.kind,
                OutcomeKind::ModelAcceptance,
                "{}",
                verdict.raw
            );
        }
        let mut renamed = versions.clone();
        for version in &mut renamed {
            if let Some(indexes) = version.get_mut("Indexes").and_then(Value::as_array_mut) {
                for index in indexes {
                    index["Name"] = json!("different_name");
                    index["ID"] = json!(999);
                }
            }
        }
        stage.captures.insert(
            "schema".into(),
            CaptureResult::Schema {
                collections: renamed,
            },
        );
        assert_eq!(check(sdl, &stage).kind, OutcomeKind::Passed);
        stage.captures.insert(
            "schema".into(),
            CaptureResult::Schema {
                collections: vec![],
            },
        );
        assert_eq!(check(sdl, &stage).score_bp, Some(0));
        stage.captures.clear();
        assert_eq!(check(sdl, &stage).kind, OutcomeKind::Grader);
        assert_eq!(check("this is not SDL", &stage).kind, OutcomeKind::Grader);
    }
}
