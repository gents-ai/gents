use anyhow::{Context, Result};
use gents::defra_query::{unknown_collection_message, CollectionScope};
use gents::graphql::validate_collection_identifier;
use gents_protocol::graphql::{extract_mutation_doc_id, graphql_input_literal};
use serde_json::{json, Value};

use crate::cli::args::{DocumentCommand, DocumentCreateArgs};
use crate::{print_json, resolve_config_access};

#[derive(Debug, thiserror::Error)]
pub(crate) enum DocumentCommandError {
    #[error("collection {collection:?} is canonical configuration; use its configuration command or pack install")]
    CanonicalConfiguration { collection: String },
    #[error("parsing --json as JSON")]
    ParseJson(#[source] serde_json::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("--json must be a non-empty JSON object of document fields")]
    NotAnObject,
    #[error("--json must not contain an empty array; omit the field instead")]
    EmptyArray,
    #[error("--json integer is outside the exact i64/u64 range; encode it as a string")]
    IntegerOutOfRange,
    #[error("collection {collection:?} has no field {key:?}; check the field name")]
    UnknownField { collection: String, key: String },
}

pub(crate) async fn dispatch(command: DocumentCommand) -> Result<()> {
    match command {
        DocumentCommand::Create(args) => create(args).await,
    }
}

fn parse_fields(collection: &str, fields: &str) -> Result<Value> {
    validate_collection_identifier(collection)?;
    CollectionScope::all().ensure_allowed(collection)?;
    if gents::Collection::ALL
        .iter()
        .any(|owner| owner.graphql_type() == collection)
    {
        return Err(DocumentCommandError::CanonicalConfiguration {
            collection: collection.to_owned(),
        }
        .into());
    }
    let raw: Box<serde_json::value::RawValue> =
        serde_json::from_str(fields).map_err(DocumentCommandError::ParseJson)?;
    ensure_exact_integers(&raw)?;
    let fields: Value = serde_json::from_str(raw.get()).map_err(DocumentCommandError::ParseJson)?;
    if !fields.as_object().is_some_and(|object| !object.is_empty()) {
        return Err(DocumentCommandError::NotAnObject.into());
    }
    if contains_empty_array(&fields) {
        return Err(DocumentCommandError::EmptyArray.into());
    }
    Ok(fields)
}

fn ensure_exact_integers(raw: &serde_json::value::RawValue) -> Result<(), DocumentCommandError> {
    let text = raw.get();
    match text.as_bytes().first() {
        Some(b'{') => {
            let raw_fields: std::collections::BTreeMap<String, Box<serde_json::value::RawValue>> =
                serde_json::from_str(text)?;
            for value in raw_fields.values() {
                ensure_exact_integers(value)?;
            }
        }
        Some(b'[') => {
            let items: Vec<Box<serde_json::value::RawValue>> = serde_json::from_str(text)?;
            for value in &items {
                ensure_exact_integers(value)?;
            }
        }
        Some(b'-' | b'0'..=b'9')
            if !text.contains(['.', 'e', 'E'])
                && text.parse::<i64>().is_err()
                && text.parse::<u64>().is_err() =>
        {
            return Err(DocumentCommandError::IntegerOutOfRange);
        }
        _ => {}
    }
    Ok(())
}

fn ensure_known_fields(
    collection: &str,
    fields: &Value,
    input_fields: &[String],
) -> Result<(), DocumentCommandError> {
    let unknown = fields
        .as_object()
        .into_iter()
        .flat_map(|object| object.keys())
        .find(|key| !input_fields.contains(key));
    match unknown {
        Some(key) => Err(DocumentCommandError::UnknownField {
            collection: collection.to_owned(),
            key: key.clone(),
        }),
        None => Ok(()),
    }
}

fn create_mutation(collection: &str, fields: &Value) -> Result<String> {
    let input = graphql_input_literal(fields)?;
    Ok(format!(
        "mutation {{ create_{collection}(input: {input}) {{ _docID }} }}"
    ))
}

fn contains_empty_array(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.is_empty() || items.iter().any(contains_empty_array),
        Value::Object(map) => map.values().any(contains_empty_array),
        _ => false,
    }
}

async fn create(args: DocumentCreateArgs) -> Result<()> {
    let fields = parse_fields(&args.collection, &args.json)?;
    let mutation = create_mutation(&args.collection, &fields)?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let input_type =
        gents::graphql::escape_graphql_string(&format!("{}MutationInputArg", args.collection));
    let query = format!(r#"{{ __type(name: "{input_type}") {{ inputFields {{ name }} }} }}"#);
    let schema = access.execute(&query).await?;
    let input_fields = schema
        .pointer("/data/__type/inputFields")
        .and_then(Value::as_array)
        .with_context(|| unknown_collection_message(&args.collection))?
        .iter()
        .map(|field| {
            field
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .context("invalid mutation input field in schema introspection")
        })
        .collect::<Result<Vec<_>>>()?;
    ensure_known_fields(&args.collection, &fields, &input_fields)?;
    let response = access.write("cli.document.create", &mutation).await?;
    let doc_id = extract_mutation_doc_id(&response, &args.collection)?;
    Ok(print_json(
        &json!({ "collection": args.collection, "doc_id": doc_id }),
    )?)
}
