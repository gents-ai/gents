use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use ts_rs::TS;

/// ts-rs 10 cannot flatten maps. This transparent adapter retains the map's
/// native JSON representation and emits the equivalent index signature.
#[derive(Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct WireObject(BTreeMap<String, Value>);

impl TS for WireObject {
    type WithoutGenerics = Self;
    fn name() -> String {
        <BTreeMap<String, Value>>::name()
    }
    fn inline() -> String {
        <BTreeMap<String, Value>>::inline()
    }
    fn inline_flattened() -> String {
        format!("{{ [key: string]: {} | undefined }}", Value::name())
    }
    fn decl() -> String {
        <BTreeMap<String, Value>>::decl()
    }
    fn decl_concrete() -> String {
        <BTreeMap<String, Value>>::decl_concrete()
    }
    fn visit_dependencies(visitor: &mut impl ts_rs::TypeVisitor) {
        <BTreeMap<String, Value>>::visit_dependencies(visitor);
    }
    fn visit_generics(visitor: &mut impl ts_rs::TypeVisitor) {
        <BTreeMap<String, Value>>::visit_generics(visitor);
    }
}

pub(super) fn present_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}
