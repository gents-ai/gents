use crate::graphql::escape_graphql_string;
use crate::Collection;
use anyhow::{Context, Result};
use serde::{
    de::{DeserializeOwned, Visitor},
    Serialize,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Default)]
struct RootFields(Option<&'static [&'static str]>);
impl<'de> serde::Deserializer<'de> for &mut RootFields {
    type Error = serde::de::value::Error;
    fn deserialize_any<V>(self, _: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Err(serde::de::Error::custom(
            "configuration must derive struct deserialization",
        ))
    }
    fn deserialize_struct<V>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        _: V,
    ) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0 = Some(fields);
        Err(serde::de::Error::custom(
            "configuration field inspection complete",
        ))
    }
    serde::forward_to_deserialize_any! { bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string bytes byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map enum identifier ignored_any }
}

pub(crate) fn canonical_struct_fields<T>() -> Result<&'static [&'static str]>
where
    T: DeserializeOwned,
{
    let mut fields = RootFields::default();
    let _ = T::deserialize(&mut fields);
    fields
        .0
        .filter(|fields| !fields.is_empty())
        .context("canonical configuration root has no struct fields")
}

fn project<T>(value: Option<&Value>) -> Result<(&'static [&'static str], Option<Value>)>
where
    T: DeserializeOwned + Serialize,
{
    let fields = canonical_struct_fields::<T>()?;
    let normalized = value
        .map(|value| -> Result<Value> {
            let config: T = serde_path_to_error::deserialize(value.clone())
                .context("decode canonical desired configuration (field path)")?;
            let mut config = serde_json::to_value(config)?;
            let object = config
                .as_object_mut()
                .context("canonical configuration did not serialize as an object")?;
            for field in fields {
                object.entry((*field).to_owned()).or_insert_with(|| {
                    if *field == "enabled" {
                        Value::Bool(true)
                    } else {
                        Value::Null
                    }
                });
            }
            object.remove("updated_at");
            let _: T = serde_json::from_value(config.clone())
                .context("canonical replacement defaults do not roundtrip")?;
            Ok(config)
        })
        .transpose()?;
    Ok((fields, normalized))
}

pub fn config_projection(
    collection: Collection,
    value: Option<&Value>,
) -> Result<(&'static [&'static str], Option<Value>)> {
    use crate::document_config::*;
    match collection {
        Collection::Node => project::<Node>(value),
        Collection::Agent => project::<Agent>(value),
        Collection::AgentContext => project::<AgentContext>(value),
        Collection::Compaction => project::<CompactionConfig>(value),
        Collection::Tools => project::<Tools>(value),
        Collection::AgentTarget => project::<AgentTargetDocument>(value),
        Collection::Skill => project::<SkillDocument>(value),
        Collection::DatastoreToolSurface => project::<DatastoreToolSurfaceDocument>(value),
        Collection::ChainKeyBinding => project::<ChainKeyBindingDocument>(value),
        Collection::EthTool => project::<EthToolDocument>(value),
        Collection::InferenceBackend => project::<InferenceBackend>(value),
        Collection::InferenceProfile => project::<InferenceProfile>(value),
        Collection::InferenceSampling => project::<InferenceSampling>(value),
        Collection::InferenceExecution => project::<InferenceExecution>(value),
        Collection::InferenceRetryPolicy => project::<InferenceRetryPolicy>(value),
        Collection::ToolServiceRegistry => project::<ToolServiceRegistry>(value),
        Collection::ProjectionAcpBinding => project::<ProjectionAcpBinding>(value),
        Collection::Task => project::<Task>(value),
        Collection::Trigger => project::<Trigger>(value),
        Collection::Schedule => project::<Schedule>(value),
        Collection::EventSource => project::<EventSource>(value),
        Collection::Callback => project::<Callback>(value),
        Collection::CallbackBinding => project::<CallbackBinding>(value),
        Collection::CallbackModule => project::<CallbackModule>(value),
        Collection::RepositoryPlacement => project::<RepositoryPlacement>(value),
        Collection::EvalDefinition => project::<EvalDefinition>(value),
    }
}

pub fn desired_state_document_digest(value: &Value) -> Result<String> {
    let mut value = value.clone();
    let root = value
        .as_object_mut()
        .context("desired commitment must be a document object")?;
    root.remove("updated_at");
    root.retain(|_, value| !value.is_null() && !value.as_array().is_some_and(Vec::is_empty));
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&value)?)
    ))
}

pub(super) fn reference_filter(collection: Collection, owner: &str, id: &str) -> Result<String> {
    anyhow::ensure!(
        !owner.trim().is_empty() && !id.trim().is_empty(),
        "configuration reference requires owner and ID"
    );
    if collection == Collection::Node {
        anyhow::ensure!(id == owner, "node reference must match owner");
        Ok(format!(
            r#"{{ node_did: {{ _eq: "{}" }} }}"#,
            escape_graphql_string(owner)
        ))
    } else {
        Ok(format!(
            r#"{{ node_did: {{ _eq: "{}" }}, {}: {{ _eq: "{}" }} }}"#,
            escape_graphql_string(owner),
            collection.unique_field(),
            escape_graphql_string(id)
        ))
    }
}
