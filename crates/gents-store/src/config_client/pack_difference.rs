use serde::Serialize;
use serde_json::Value;

use super::{PackMutation, PackRelationship};

/// At most this many differences are reported for one mutation; the first
/// names where the plans diverge.
pub const MAX_REPORTED_DIFFERENCES: usize = 5;

/// One way the client's plan and the runtime's proposal disagree. Each side
/// is a [`ValueShape`], never a value: install values can be credentials and
/// `${VAR}` renders into any field, so neither `Debug` nor `Serialize` of a
/// difference may carry one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackMutationDifference {
    Operation {
        constructed: PackMutationKind,
        proposed: PackMutationKind,
    },
    Collection {
        constructed: String,
        proposed: String,
    },
    DocId {
        constructed: Option<String>,
        proposed: Option<String>,
    },
    Field {
        path: String,
        constructed: Option<ValueShape>,
        proposed: Option<ValueShape>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackMutationKind {
    Create,
    Update,
    Delete,
}

/// A JSON value by kind and size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValueShape {
    Null,
    Boolean,
    Number,
    Text { chars: usize },
    List { items: usize },
    Map { fields: usize },
}

impl ValueShape {
    pub fn of(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(_) => Self::Boolean,
            Value::Number(_) => Self::Number,
            Value::String(text) => Self::Text {
                chars: text.chars().count(),
            },
            Value::Array(items) => Self::List { items: items.len() },
            Value::Object(fields) => Self::Map {
                fields: fields.len(),
            },
        }
    }
}

impl std::fmt::Display for ValueShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Boolean => f.write_str("a boolean"),
            Self::Number => f.write_str("a number"),
            Self::Text { chars } => write!(f, "a string ({chars} chars)"),
            Self::List { items } => write!(f, "an array ({items} items)"),
            Self::Map { fields } => write!(f, "an object ({fields} fields)"),
        }
    }
}

impl PackMutationKind {
    fn verb(self) -> &'static str {
        match self {
            Self::Create => "creates",
            Self::Update => "updates",
            Self::Delete => "deletes",
        }
    }
}

impl std::fmt::Display for PackMutationDifference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Operation {
                constructed,
                proposed,
            } => write!(
                f,
                "the client's plan {} while the runtime's {}",
                constructed.verb(),
                proposed.verb()
            ),
            Self::Collection {
                constructed,
                proposed,
            } => write!(
                f,
                "the client's plan targets `{constructed}` while the runtime's targets `{proposed}`"
            ),
            Self::DocId {
                constructed,
                proposed,
            } => write!(
                f,
                "the plans name different documents (client {}, runtime {})",
                constructed.as_deref().unwrap_or("a new one"),
                proposed.as_deref().unwrap_or("a new one")
            ),
            Self::Field {
                path,
                constructed: Some(constructed),
                proposed: Some(proposed),
            } => write!(
                f,
                "`{path}` is {constructed} in the client's plan and {proposed} in the runtime's"
            ),
            Self::Field {
                path,
                constructed: Some(shape),
                proposed: None,
            } => write!(f, "only the client's plan carries `{path}` ({shape})"),
            Self::Field {
                path,
                constructed: None,
                proposed: Some(shape),
            } => write!(f, "only the runtime's plan carries `{path}` ({shape})"),
            Self::Field {
                path,
                constructed: None,
                proposed: None,
            } => write!(f, "`{path}` is in neither plan"),
        }
    }
}

impl PackMutation {
    pub fn kind(&self) -> PackMutationKind {
        match self {
            Self::Create { .. } => PackMutationKind::Create,
            Self::Update { .. } => PackMutationKind::Update,
            Self::Delete { .. } => PackMutationKind::Delete,
        }
    }

    pub fn collection(&self) -> &str {
        match self {
            Self::Create { collection, .. }
            | Self::Update { collection, .. }
            | Self::Delete { collection, .. } => collection,
        }
    }

    pub fn doc_id(&self) -> Option<&str> {
        match self {
            Self::Create { .. } => None,
            Self::Update { doc_id, .. } | Self::Delete { doc_id, .. } => Some(doc_id),
        }
    }

    fn input(&self) -> Option<&Value> {
        match self {
            Self::Create { input, .. } | Self::Update { input, .. } => Some(input),
            Self::Delete { .. } => None,
        }
    }

    /// How `self`, the client's plan, differs from the runtime's `proposed`:
    /// operation, collection and document first, then the input's fields in
    /// the client's order.
    pub fn differences(&self, proposed: &Self) -> Vec<PackMutationDifference> {
        if self.kind() != proposed.kind() {
            return vec![PackMutationDifference::Operation {
                constructed: self.kind(),
                proposed: proposed.kind(),
            }];
        }
        let mut differences = Vec::new();
        if self.collection() != proposed.collection() {
            differences.push(PackMutationDifference::Collection {
                constructed: self.collection().to_owned(),
                proposed: proposed.collection().to_owned(),
            });
        }
        if self.doc_id() != proposed.doc_id() {
            differences.push(PackMutationDifference::DocId {
                constructed: self.doc_id().map(str::to_owned),
                proposed: proposed.doc_id().map(str::to_owned),
            });
        }
        if let (Some(constructed), Some(proposed)) = (self.input(), proposed.input()) {
            fields("", constructed, proposed, &mut differences);
        }
        differences.truncate(MAX_REPORTED_DIFFERENCES);
        differences
    }
}

impl PackRelationship {
    pub fn differences(&self, proposed: &Self) -> Vec<PackMutationDifference> {
        let mut differences = Vec::new();
        if let (Ok(constructed), Ok(proposed)) =
            (serde_json::to_value(self), serde_json::to_value(proposed))
        {
            fields("", &constructed, &proposed, &mut differences);
        }
        differences.truncate(MAX_REPORTED_DIFFERENCES);
        differences
    }
}

fn pointer(path: &str, token: &str) -> String {
    format!("{path}/{}", token.replace('~', "~0").replace('/', "~1"))
}

fn fields(
    path: &str,
    constructed: &Value,
    proposed: &Value,
    differences: &mut Vec<PackMutationDifference>,
) {
    let only = |path: String, constructed: Option<&Value>, proposed: Option<&Value>| {
        PackMutationDifference::Field {
            path,
            constructed: constructed.map(ValueShape::of),
            proposed: proposed.map(ValueShape::of),
        }
    };
    match (constructed, proposed) {
        (Value::Object(constructed), Value::Object(proposed)) => {
            for (key, value) in constructed {
                let child = pointer(path, key);
                match proposed.get(key) {
                    Some(other) => fields(&child, value, other, differences),
                    None => differences.push(only(child, Some(value), None)),
                }
            }
            for (key, value) in proposed {
                if !constructed.contains_key(key) {
                    differences.push(only(pointer(path, key), None, Some(value)));
                }
            }
        }
        (Value::Array(constructed), Value::Array(proposed)) => {
            for index in 0..constructed.len().max(proposed.len()) {
                let child = format!("{path}/{index}");
                match (constructed.get(index), proposed.get(index)) {
                    (Some(value), Some(other)) => fields(&child, value, other, differences),
                    (value, other) => differences.push(only(child, value, other)),
                }
            }
        }
        (constructed, proposed) if constructed != proposed => {
            differences.push(only(path.to_owned(), Some(constructed), Some(proposed)));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn create(collection: &str, input: Value) -> PackMutation {
        PackMutation::Create {
            collection: collection.to_owned(),
            input,
        }
    }

    fn field(path: &str, constructed: Option<ValueShape>, proposed: Option<ValueShape>) -> PackMutationDifference {
        PackMutationDifference::Field {
            path: path.to_owned(),
            constructed,
            proposed,
        }
    }

    #[test]
    fn a_differing_field_is_named_by_path_and_shape() {
        let differences = create("Agent", json!({"agent_did": "did:key:client", "name": "Scout"}))
            .differences(&create(
                "Agent",
                json!({"agent_did": "did:key:runtime", "name": "Scout"}),
            ));
        assert_eq!(
            differences,
            vec![field(
                "/agent_did",
                Some(ValueShape::Text { chars: 14 }),
                Some(ValueShape::Text { chars: 15 })
            )]
        );
        let described = differences[0].to_string();
        assert_eq!(
            described,
            "`/agent_did` is a string (14 chars) in the client's plan and a string (15 chars) in the runtime's"
        );
    }

    #[test]
    fn an_absent_key_against_a_null_one_is_named_at_its_path() {
        let differences = create("PackInstallation", json!({"variables": {}})).differences(
            &create(
                "PackInstallation",
                json!({"variables": {"GENTS_PACK_BIND_AGENT_WEB_RESEARCHER": null}}),
            ),
        );
        assert_eq!(
            differences,
            vec![field(
                "/variables/GENTS_PACK_BIND_AGENT_WEB_RESEARCHER",
                None,
                Some(ValueShape::Null)
            )]
        );
        assert_eq!(
            differences[0].to_string(),
            "only the runtime's plan carries `/variables/GENTS_PACK_BIND_AGENT_WEB_RESEARCHER` (null)"
        );
    }

    #[test]
    fn an_operation_or_collection_difference_comes_first() {
        let created = create("Agent", json!({"a": 1}));
        let deleted = PackMutation::Delete {
            collection: "Agent".to_owned(),
            doc_id: "bae-1".to_owned(),
        };
        assert_eq!(
            created.differences(&deleted),
            vec![PackMutationDifference::Operation {
                constructed: PackMutationKind::Create,
                proposed: PackMutationKind::Delete,
            }]
        );
        assert_eq!(
            created.differences(&create("Context", json!({"a": 2}))),
            vec![
                PackMutationDifference::Collection {
                    constructed: "Agent".to_owned(),
                    proposed: "Context".to_owned(),
                },
                field("/a", Some(ValueShape::Number), Some(ValueShape::Number)),
            ]
        );
    }

    #[test]
    fn arrays_and_pointer_tokens_name_exact_paths() {
        let differences = create("Agent", json!({"tags": ["a", "b"], "a/b": 1, "c~d": 2}))
            .differences(&create(
                "Agent",
                json!({"tags": ["a", "c", "d"], "a/b": 9, "c~d": 8}),
            ));
        let paths: Vec<_> = differences
            .iter()
            .map(|difference| match difference {
                PackMutationDifference::Field { path, .. } => path.as_str(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(paths, ["/a~1b", "/c~0d", "/tags/1", "/tags/2"]);
    }

    #[test]
    fn agreeing_plans_have_no_differences_and_many_are_bounded() {
        let input = json!({"a": 1, "b": ["x", {"c": true}]});
        assert!(create("Agent", input.clone())
            .differences(&create("Agent", input))
            .is_empty());
        let wide = |value: Value| {
            create(
                "Agent",
                Value::Object((0..40).map(|i| (format!("f{i}"), value.clone())).collect()),
            )
        };
        assert_eq!(
            wide(json!(1)).differences(&wide(json!("x"))).len(),
            MAX_REPORTED_DIFFERENCES
        );
    }

    #[test]
    fn a_relationship_difference_names_its_field() {
        let relationship = |did: &str| PackRelationship {
            collection: "Agent".into(),
            doc_id: "bae-1".into(),
            relation: "reader".into(),
            target: super::super::PackRelationshipTarget::Actor { did: did.into() },
            delete: false,
        };
        let differences = relationship("did:key:a").differences(&relationship("did:key:bb"));
        assert_eq!(
            differences,
            vec![field(
                "/target/did",
                Some(ValueShape::Text { chars: 9 }),
                Some(ValueShape::Text { chars: 10 })
            )]
        );
        assert!(!differences[0].to_string().contains("did:key"));
    }

    #[test]
    fn the_difference_type_prints_no_value_through_debug_or_serialize() {
        let secret = "sk-live-0123456789abcdef";
        let token = "ghp_0123456789abcdefghij";
        let cases = [
            (
                json!({"variables": {"API_TOKEN": secret}}),
                json!({"variables": {"API_TOKEN": "other"}}),
            ),
            (json!({"variables": {"API_TOKEN": secret}}), json!({"variables": null})),
            (json!({"a": 1}), json!({"a": 1, "variables": {"API_TOKEN": secret}})),
            (
                json!({"headers": {"Authorization": format!("Bearer {token}")}}),
                json!({"headers": {"Authorization": "Bearer other"}}),
            ),
        ];
        for (constructed, proposed) in cases {
            let differences = create("PackInstallation", constructed)
                .differences(&create("PackInstallation", proposed));
            assert!(!differences.is_empty());
            let printed = [
                format!("{differences:?}"),
                serde_json::to_string(&differences).unwrap(),
                differences.iter().map(ToString::to_string).collect(),
            ];
            for text in printed {
                for value in [secret, token, "Bearer", "other"] {
                    assert!(!text.contains(value), "{text}");
                }
            }
        }
    }

    #[test]
    fn every_json_kind_has_a_shape() {
        for (value, expected) in [
            (json!(null), "null"),
            (json!(true), "a boolean"),
            (json!(7), "a number"),
            (json!("abc"), "a string (3 chars)"),
            (json!([1, 2]), "an array (2 items)"),
            (json!({"a": 1}), "an object (1 fields)"),
        ] {
            assert_eq!(ValueShape::of(&value).to_string(), expected, "{value}");
        }
    }
}
