use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::eval::checks::captured_fields_match::Expectation;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Params {
    #[serde(default)]
    pub(super) present: Vec<Present>,
    #[serde(default)]
    pub(super) rows: Vec<RowSpec>,
    #[serde(default)]
    pub(super) links: Vec<LinkSpec>,
    #[serde(default)]
    pub(super) agents: Option<Agents>,
    #[serde(default)]
    pub(super) templates: Vec<TemplateSpec>,
    #[serde(default)]
    pub(super) receipt: Option<Receipt>,
    #[serde(default)]
    pub(super) continuation: Option<Continuation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Continuation {
    pub(super) max: u32,
    #[serde(default)]
    pub(super) category: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Present {
    pub(super) capture: String,
    #[serde(default)]
    pub(super) min: Option<usize>,
    #[serde(default)]
    pub(super) max: Option<usize>,
    #[serde(default)]
    pub(super) category: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RowSpec {
    pub(super) capture: String,
    pub(super) key: String,
    pub(super) id: String,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) expect: Vec<Expectation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LinkSpec {
    pub(super) capture: String,
    #[serde(default)]
    pub(super) key: String,
    #[serde(default)]
    pub(super) id: String,
    #[serde(default)]
    pub(super) source_match: Option<Expectation>,
    pub(super) field: String,
    pub(super) target_capture: String,
    pub(super) target_key: String,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) expect: Vec<Expectation>,
    #[serde(default)]
    pub(super) via: Vec<LinkHop>,
    #[serde(default)]
    pub(super) reported_fields: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LinkHop {
    pub(super) field: String,
    pub(super) target_capture: String,
    pub(super) target_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Agents {
    pub(super) agents: String,
    pub(super) contexts: String,
    pub(super) tools: String,
    pub(super) expect: Vec<AgentSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentSpec {
    pub(super) agent_id: String,
    #[serde(default)]
    pub(super) source_match: Option<Expectation>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) agent: Vec<Expectation>,
    #[serde(default)]
    pub(super) context: Vec<Expectation>,
    #[serde(default)]
    pub(super) tools: Vec<Expectation>,
    #[serde(default)]
    pub(super) datastore: Option<DatastoreSpec>,
    #[serde(default)]
    pub(super) delegates: Option<DelegateSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DatastoreSpec {
    pub(super) surfaces: String,
    pub(super) create: BTreeSet<String>,
    pub(super) query: BTreeSet<String>,
    #[serde(default)]
    pub(super) caller_fields: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub(super) required_fields: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub(super) query_fields: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub(super) lookup_by: Option<String>,
    #[serde(default)]
    pub(super) called_create: BTreeSet<String>,
    #[serde(default)]
    pub(super) called_query: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DelegateSpec {
    pub(super) targets: String,
    pub(super) agents: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TemplateSpec {
    pub(super) capture: String,
    pub(super) key: String,
    pub(super) id: String,
    pub(super) fields: Vec<String>,
    pub(super) allowed: Vec<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub(super) capture: String,
    pub(super) fields: Vec<String>,
    pub(super) ids: Vec<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
}
