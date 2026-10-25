use std::collections::BTreeMap;

use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config_client::{ConfigClientError, ConfigRead};

#[derive(Debug, thiserror::Error)]
pub enum NativeFilterError {
    #[error("filter must be a JSON object")]
    NotObject,
    #[error(transparent)]
    Depth(#[from] ::query::QueryError),
    #[error("invalid filter for {collection}; call query with argv:[help,find] for filter syntax")]
    Invalid {
        collection: String,
        #[source]
        source: Box<NativeFilterError>,
    },
    #[error("filter contains null for a non-null native input")]
    NullForNonNull,
    #[error("native non-null type lacks its inner type")]
    NonNullInner,
    #[error("native list type lacks its element type")]
    ListElement,
    #[error("native input type lacks its name")]
    InputName,
    #[error(transparent)]
    ScalarKind(#[from] serde_json::Error),
    #[error("filter operand for native {name} has the wrong value type")]
    OperandType { name: String },
    #[error("unsupported native filter input kind {kind}")]
    UnsupportedKind { kind: String },
    #[error(transparent)]
    Access(#[from] ConfigClientError),
    #[error("native filter definition {name} is unavailable")]
    DefinitionUnavailable {
        name: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("filter condition must be an object")]
    ConditionNotObject,
    #[error(transparent)]
    Protected(Box<crate::document_config::DocumentConfigError>),
    #[error("unknown filter field/operator {key:?} in {name}; available: [{available}]")]
    UnknownField {
        key: String,
        name: String,
        available: String,
    },
    #[error("filtering on restricted credential field {collection}.{key} is not allowed")]
    RestrictedField { collection: String, key: String },
}

impl crate::config_client::TransactionFailure for NativeFilterError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::Invalid { source, .. } => source.config_client_error(),
            _ => None,
        }
    }
}

type Result<T, E = NativeFilterError> = std::result::Result<T, E>;

#[derive(Clone, Deserialize)]
struct TypeRef {
    kind: String,
    name: Option<String>,
    #[serde(rename = "ofType")]
    of_type: Option<Box<TypeRef>>,
}

#[derive(Clone, Deserialize)]
struct InputField {
    name: String,
    #[serde(rename = "type")]
    type_ref: TypeRef,
}

pub(crate) async fn validate_filter(
    access: &dyn ConfigRead,
    collection: &str,
    filter: &Value,
) -> Result<()> {
    if filter.is_null() {
        return Ok(());
    }
    let conditions = filter
        .as_object()
        .ok_or(NativeFilterError::NotObject)?
        .clone();
    ::query::mapper::Filter::from_conditions(conditions).validate_depth()?;
    let root = TypeRef {
        kind: "INPUT_OBJECT".into(),
        name: Some(format!("{collection}FilterArg")),
        of_type: None,
    };
    validate_value(access, &root, filter, &mut BTreeMap::new())
        .await
        .map_err(|source| NativeFilterError::Invalid {
            collection: collection.to_owned(),
            source: Box::new(source),
        })
}

fn validate_value<'a>(
    access: &'a dyn ConfigRead,
    ty: &'a TypeRef,
    value: &'a Value,
    cache: &'a mut BTreeMap<String, Vec<InputField>>,
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        if ty.kind == "NON_NULL" {
            if value.is_null() {
                return Err(NativeFilterError::NullForNonNull);
            }
            return validate_value(
                access,
                ty.of_type
                    .as_deref()
                    .ok_or(NativeFilterError::NonNullInner)?,
                value,
                cache,
            )
            .await;
        }
        if value.is_null() {
            return Ok(());
        }
        if ty.kind == "LIST" {
            let inner = ty
                .of_type
                .as_deref()
                .ok_or(NativeFilterError::ListElement)?;
            if let Some(values) = value.as_array() {
                for item in values {
                    validate_value(access, inner, item, cache).await?;
                }
            } else {
                validate_value(access, inner, value, cache).await?;
            }
            return Ok(());
        }
        let name = ty.name.as_deref().ok_or(NativeFilterError::InputName)?;
        if ty.kind == "SCALAR" {
            let kind: ::schema::FieldKind = serde_json::from_value(json!(name))?;
            if !kind.accepts_filter_value(value) {
                return Err(NativeFilterError::OperandType {
                    name: name.to_owned(),
                });
            }
            return Ok(());
        }
        if ty.kind != "INPUT_OBJECT" {
            return Err(NativeFilterError::UnsupportedKind {
                kind: ty.kind.clone(),
            });
        }
        if !cache.contains_key(name) {
            let query = format!("{{ __type(name: \"{}\") {{ inputFields {{ name type {{ kind name ofType {{ kind name ofType {{ kind name ofType {{ kind name }} }} }} }} }} }} }}", crate::graphql::escape_graphql_string(name));
            let response = access.execute_read(&query).await?;
            let fields: Vec<InputField> = serde_json::from_value(
                response["data"]["__type"]["inputFields"].clone(),
            )
            .map_err(|source| NativeFilterError::DefinitionUnavailable {
                name: name.to_owned(),
                source,
            })?;
            cache.insert(name.to_owned(), fields);
        }
        let fields = cache[name].clone();
        if let Some(equality) = fields.iter().find(|field| field.name == "_eq") {
            let literal = !value.is_object()
                || (equality.type_ref.name.as_deref() == Some("JSON")
                    && value
                        .as_object()
                        .is_some_and(|object| object.keys().all(|key| !key.starts_with('_'))));
            if literal {
                return validate_value(access, &equality.type_ref, value, cache).await;
            }
        }
        let object = value
            .as_object()
            .ok_or(NativeFilterError::ConditionNotObject)?;
        if !fields.iter().any(|field| field.name == "_eq") {
            if let Some(collection) = name.strip_suffix("FilterArg") {
                crate::document_config::reject_protected_collection_name(collection)
                    .map_err(|error| NativeFilterError::Protected(Box::new(error)))?;
            }
        }
        for (key, operand) in object {
            let canonical = ::query::mapper::FilterOp::parse(key)
                .map(|op| op.as_str())
                .unwrap_or(key);
            let Some(field) = fields.iter().find(|field| field.name == canonical) else {
                let available = fields
                    .iter()
                    .filter(|field| {
                        name.strip_suffix("FilterArg").is_none_or(|collection| {
                            !super::query::is_restricted_field(collection, &field.name)
                        })
                    })
                    .map(|field| field.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(NativeFilterError::UnknownField {
                    key: key.clone(),
                    name: name.to_owned(),
                    available,
                });
            };
            if let Some(collection) = name.strip_suffix("FilterArg") {
                if super::query::is_restricted_field(collection, key) {
                    return Err(NativeFilterError::RestrictedField {
                        collection: collection.to_owned(),
                        key: key.clone(),
                    });
                }
            }
            validate_value(access, &field.type_ref, operand, cache).await?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config_client::ConfigAccess;
    use gents_protocol::error_chain::ErrorChain;

    #[tokio::test]
    async fn native_input_shapes_preserve_compounds_relationships_literals_and_aliases() {
        let node = Arc::new(
            crate::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        node.add_schema("type FilterAuthor { name: String } type FilterBook { title: String score: Int tags: [String] payload: JSON author: FilterAuthor }").await.unwrap();
        let access = ConfigAccess::local(node.clone());
        for filter in [
            json!({"_and":[{"score":{"_geq":0}},{"author":{"name":{"_eq":"Ada"}}}]}),
            json!({"_or":[{"title":{"_like":"A%"}},{"title":{"_neq":"B"}}]}),
            json!({"score":{"_ge":0,"_le":10}}),
            json!({"title":"A"}),
            json!({"payload":{"_eq":{"nested":[]}}}),
            json!({"tags":{"_any":{"_like":"A%"}}}),
        ] {
            validate_filter(&access, "FilterBook", &filter)
                .await
                .unwrap_or_else(|error| {
                    panic!("valid native filter {filter}: {}", ErrorChain(&error))
                });
        }
        for filter in [
            json!({"and":[{"eq":{"title":"A"}}]}),
            json!({"title":{"_typo":"A"}}),
            json!({"score":{"_eq":"A"}}),
            json!({"author":{"wrong_field":{"_eq":"Ada"}}}),
        ] {
            assert!(
                validate_filter(&access, "FilterBook", &filter)
                    .await
                    .is_err(),
                "invalid filter was accepted: {filter}"
            );
        }
        node.add_schema("type OAuthCredential { credential_id: String access_token: String refresh_token: String id_token: String }").await.unwrap();
        let error = validate_filter(&access, "OAuthCredential", &json!({"unknown_field":"x"}))
            .await
            .unwrap_err();
        let diagnostic = format!("{}", ErrorChain(&error));
        assert!(diagnostic.contains("credential_id"));
        for field in ["access_token", "refresh_token", "id_token"] {
            assert!(!diagnostic.contains(field), "{diagnostic}");
        }
        node.shutdown().await;
    }

    #[tokio::test]
    async fn malformed_count_filter_is_refused_before_a_plausible_native_zero() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("gents::defra_query::native_filter=info")
            .try_init();
        let node = Arc::new(
            crate::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        node.add_schema("type CountFilterProbe { name: String score: Int }")
            .await
            .unwrap();
        ConfigAccess::write_local(&node,"seed_count_filter_probe","mutation { add_CountFilterProbe(input:[{name:\"ready\",score:0},{name:\"ready\",score:1}]){_docID} }").await.unwrap();
        let access = ConfigAccess::local(node.clone());
        let native = access
            .execute("{ COUNT(CountFilterProbe:{filter:{and:[{eq:{name:\"ready\"}}]}}) }")
            .await;
        tracing::info!(native_response=?native, "native COUNT malformed-filter reproduction");
        let args: super::super::QueryParams = serde_json::from_value(json!({"argv":["count"],"collection":"CountFilterProbe","options":{"filter":{"and":[{"eq":{"name":"ready"}}]}}})).unwrap();
        let scope = super::super::CollectionScope::restricted(vec!["CountFilterProbe".into()]);
        let error = super::super::execute_command(&access, &args, &scope)
            .await
            .unwrap_err();
        assert!(format!("{}", ErrorChain(error.as_ref())).contains("unknown filter field/operator"));
        let mut corrected = args;
        corrected.options.insert(
            "filter".into(),
            json!({"_and":[{"name":{"_eq":"ready"}},{"score":{"_geq":0}}]}),
        );
        let result = super::super::execute_command(&access, &corrected, &scope)
            .await
            .unwrap();
        assert_eq!(result["total_count"], 2);
        node.shutdown().await;
    }
}
