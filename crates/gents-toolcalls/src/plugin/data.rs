use anyhow::{bail, Result};
use gents_protocol::job::{
    DataCall, DataOp, DataQuery, DataQueryResponse, DataWrite, PluginDataDeclaration, RoundResult,
    CALLER_DID_FILTER,
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

pub use gents_protocol::job::OnConflict;
pub use gents_protocol::job::StampedField as Stamp;

use crate::plugin::application_write::Observation;
use gents_pack::plugin::store::PluginData;

const MAX_WRITES: usize = 100;

/// Lean `PluginData.Invocation`: the context a plugin's writes run in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Invocation {
    ToolCall,
    JobStep,
    Callback,
    Ux,
}

impl Invocation {
    /// Lean `Stamp.available`.
    pub fn stamp_available(self, stamp: Stamp) -> bool {
        match self {
            Self::Callback | Self::Ux => stamp == Stamp::CallerDid,
            Self::ToolCall => !matches!(stamp, Stamp::JobRowDocId | Stamp::ChainRootDocId),
            Self::JobStep => true,
        }
    }
}

impl fmt::Display for Invocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ToolCall => "tool",
            Self::JobStep => "job step",
            Self::Callback => "callback",
            Self::Ux => "ux",
        })
    }
}

/// The host facts a stamped field is filled from, by call context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StampContext {
    ToolOutput {
        caller_did: String,
        session_id: String,
        request_doc_id: String,
        tool_call_doc_id: String,
    },
    JobStep {
        caller_did: String,
        session_id: String,
        request_doc_id: String,
        tool_call_doc_id: String,
        job_row_doc_id: String,
        chain_root_doc_id: String,
    },
    Callback {
        caller_did: String,
    },
    Ux {
        caller_did: String,
    },
}

impl StampContext {
    pub fn invocation(&self) -> Invocation {
        match self {
            Self::ToolOutput { .. } => Invocation::ToolCall,
            Self::JobStep { .. } => Invocation::JobStep,
            Self::Callback { .. } => Invocation::Callback,
            Self::Ux { .. } => Invocation::Ux,
        }
    }

    /// The context's value for `stamp`, exactly when the invocation has one.
    pub fn value(&self, stamp: Stamp) -> Option<&str> {
        if !self.invocation().stamp_available(stamp) {
            return None;
        }
        match (self, stamp) {
            (
                Self::ToolOutput { caller_did, .. }
                | Self::JobStep { caller_did, .. }
                | Self::Callback { caller_did }
                | Self::Ux { caller_did },
                Stamp::CallerDid,
            ) => Some(caller_did),
            (
                Self::ToolOutput { session_id, .. } | Self::JobStep { session_id, .. },
                Stamp::SessionId,
            ) => Some(session_id),
            (
                Self::ToolOutput { request_doc_id, .. } | Self::JobStep { request_doc_id, .. },
                Stamp::RequestDocId,
            ) => Some(request_doc_id),
            (
                Self::ToolOutput {
                    tool_call_doc_id, ..
                }
                | Self::JobStep {
                    tool_call_doc_id, ..
                },
                Stamp::ToolCallDocId,
            ) => Some(tool_call_doc_id),
            (Self::JobStep { job_row_doc_id, .. }, Stamp::JobRowDocId) => Some(job_row_doc_id),
            (
                Self::JobStep {
                    chain_root_doc_id, ..
                },
                Stamp::ChainRootDocId,
            ) => Some(chain_root_doc_id),
            _ => None,
        }
    }
}

fn stamp_name(stamp: Stamp) -> &'static str {
    match stamp {
        Stamp::CallerDid => "caller_did",
        Stamp::SessionId => "session_id",
        Stamp::RequestDocId => "request_doc_id",
        Stamp::ToolCallDocId => "tool_call_doc_id",
        Stamp::JobRowDocId => "job_row_doc_id",
        Stamp::ChainRootDocId => "chain_root_doc_id",
    }
}

/// Lean `PluginData.Refusal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteRefusal {
    StampUnavailable {
        stamp: Stamp,
        invocation: Invocation,
    },
    Other,
}

impl fmt::Display for WriteRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StampUnavailable { stamp, invocation } => write!(
                f,
                "stamp {} has no value in a {invocation} call; stamp a field the call context \
                 provides or drop it from data.stamped",
                stamp_name(*stamp)
            ),
            Self::Other => f.write_str(
                "the write is outside the plugin's declared data grant; write only the declared \
                 data.collections with the declared data.ops, at most 100 per output, leave \
                 stamped fields to the host, and use on_conflict only on insert",
            ),
        }
    }
}

impl std::error::Error for WriteRefusal {}

/// Lean `PluginData.Observation`.
#[derive(Debug)]
pub struct WriteObservation {
    pub in_own_sdl: bool,
    pub runtime_or_protected: bool,
    pub actor_is_invoker: bool,
    pub op_granted: bool,
    pub stamped_supplied_by_guest: bool,
    pub invocation: Invocation,
    pub applicable: Vec<(String, Stamp)>,
    pub write: Observation,
}

/// Lean `applicableStamps`: a declared stamp applies only to a field of the
/// written collection.
pub fn applicable_stamps(
    declared: &[(String, Stamp)],
    collection_fields: &[String],
) -> Vec<(String, Stamp)> {
    declared
        .iter()
        .filter(|(field, _)| collection_fields.contains(field))
        .cloned()
        .collect()
}

/// Lean `PluginData.admitted` and `refusal`.
pub fn admit_write_observation(observation: &WriteObservation) -> Result<(), WriteRefusal> {
    if let Some((_, stamp)) = observation
        .applicable
        .iter()
        .find(|(_, stamp)| !observation.invocation.stamp_available(*stamp))
    {
        return Err(WriteRefusal::StampUnavailable {
            stamp: *stamp,
            invocation: observation.invocation,
        });
    }
    if observation.in_own_sdl
        && !observation.runtime_or_protected
        && observation.actor_is_invoker
        && observation.op_granted
        && !observation.stamped_supplied_by_guest
        && observation.write.admitted()
    {
        Ok(())
    } else {
        Err(WriteRefusal::Other)
    }
}

/// Lean `PluginData.InsertOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertOutcome {
    Inserted,
    Unchanged,
    AbortStep,
}

/// Lean `PluginData.insertNote`: what an insert with `on_conflict` does given
/// the fingerprint of the row already stored under its unique key. The unique
/// index is only the backstop behind this decision.
pub fn insert_note(existing: Option<&str>, new: &str, on_conflict: OnConflict) -> InsertOutcome {
    match existing {
        None => InsertOutcome::Inserted,
        Some(old) if old == new => InsertOutcome::Unchanged,
        Some(_) => match on_conflict {
            OnConflict::Ignore => InsertOutcome::Unchanged,
            OnConflict::Fail => InsertOutcome::AbortStep,
        },
    }
}

/// A write that passed admission, with every applicable stamp filled from the
/// call context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedWrite {
    pub write: DataWrite,
}

fn op_name(op: DataOp) -> &'static str {
    match op {
        DataOp::Insert => "insert",
        DataOp::Update => "update",
        DataOp::Delete => "delete",
    }
}

/// Admits `writes` against `grant`, returning the first refusal.
///
/// Precondition: the caller's transaction is opened as the invoker. Every
/// caller applies the admitted writes in a transaction opened as the invoking
/// principal, which is why each observation sets `actor_is_invoker: true`.
pub fn admit_write(
    grant: &PluginData,
    stamps: &StampContext,
    writes: Vec<DataWrite>,
) -> Result<Vec<AdmittedWrite>, WriteRefusal> {
    if writes.len() > MAX_WRITES
        || writes
            .iter()
            .any(|w| w.on_conflict.is_some() && w.op != DataOp::Insert)
    {
        return Err(WriteRefusal::Other);
    }
    let data = &grant.declaration;
    let declared: Vec<(String, Stamp)> = data
        .stamped
        .iter()
        .map(|(field, stamp)| (field.clone(), *stamp))
        .collect();
    let mut admitted = Vec::with_capacity(writes.len());
    for mut write in writes {
        let collection = write.collection.as_str();
        let granted = data.collections.iter().any(|c| c == collection);
        let targeted = write.op == DataOp::Insert || write.doc_id.is_some();
        // Without the collection's field names no stamp could be matched, so
        // the write would be admitted unstamped: refuse instead.
        let Some(collection_fields) = grant.collection_fields.get(collection) else {
            return Err(WriteRefusal::Other);
        };
        let applicable = applicable_stamps(&declared, collection_fields);
        admit_write_observation(&WriteObservation {
            in_own_sdl: granted,
            runtime_or_protected:
                gents_documents::callback_plugin_handler::writable_output_collection(collection)
                    .is_err(),
            actor_is_invoker: true,
            op_granted: data.ops.iter().any(|op| op == op_name(write.op)),
            stamped_supplied_by_guest: write
                .fields
                .keys()
                .any(|field| data.stamped.contains_key(field)),
            invocation: stamps.invocation(),
            applicable: applicable.clone(),
            write: Observation {
                operation: match write.op {
                    DataOp::Insert => "create",
                    DataOp::Update => "update",
                    DataOp::Delete => "delete",
                }
                .into(),
                granted,
                application: !gents_store::schema::migration::DEFAULT_REGISTRY
                    .managed_names()
                    .any(|name| name == collection),
                protected: gents_documents::PROTECTED_DATASTORE_COLLECTIONS.contains(&collection),
                credential: gents_protocol::schemas::is_credential_collection(collection),
                bounded: targeted,
                targets: usize::from(targeted),
                limit: MAX_WRITES,
                preview: false,
                digest_matches: true,
            },
        })?;
        if write.op != DataOp::Delete {
            for (field, stamp) in applicable {
                if let Some(value) = stamps.value(stamp) {
                    write.fields.insert(field, Value::String(value.to_owned()));
                }
            }
        }
        admitted.push(AdmittedWrite { write });
    }
    Ok(admitted)
}

/// Largest `limit` a `data_calls.query` may ask for (Lean `queryAdmitted`).
const MAX_QUERY_LIMIT: u32 = 256;
/// Queries one round may carry.
const MAX_QUERIES_PER_ROUND: usize = 16;

/// Lean `PluginData.queryAdmitted`.
pub fn query_admitted(
    limit: u32,
    in_own_sdl: bool,
    runtime_or_protected: bool,
    query_granted: bool,
) -> bool {
    in_own_sdl && !runtime_or_protected && query_granted && 0 < limit && limit <= MAX_QUERY_LIMIT
}

fn caller_value<'a>(value: &'a str, caller_did: &'a str) -> &'a str {
    if value == CALLER_DID_FILTER {
        caller_did
    } else {
        value
    }
}

/// Lean `PluginData.substituteCallerDid`.
pub fn substitute_caller_did(
    filter: &BTreeMap<String, String>,
    caller_did: &str,
) -> BTreeMap<String, String> {
    filter
        .iter()
        .map(|(key, value)| (key.clone(), caller_value(value, caller_did).to_owned()))
        .collect()
}

fn substitute_json(value: &mut Value, caller_did: &str) {
    match value {
        Value::String(text) => *text = caller_value(text, caller_did).to_owned(),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| substitute_json(item, caller_did)),
        Value::Object(map) => map
            .values_mut()
            .for_each(|item| substitute_json(item, caller_did)),
        _ => {}
    }
}

/// Plugin authority and `$caller.did` provenance name the same authenticated
/// invoker. Agent calls retain the selected agent DID throughout execution.
#[derive(Clone)]
pub struct DataCaller {
    access: DataAccess,
    caller_did: String,
}

#[derive(Clone)]
enum DataAccess {
    Node(Arc<defra_node::Node>, ::identity::Did),
    Operator(Box<gents_store::config_client::ConfigAccess>),
}

impl DataCaller {
    pub(crate) fn model_actor(&self) -> Result<::identity::Did> {
        Ok(::identity::Did::new(&self.caller_did)?)
    }

    pub fn actor(&self) -> Option<&::identity::Did> {
        match &self.access {
            DataAccess::Node(_, actor) => Some(actor),
            DataAccess::Operator(_) => None,
        }
    }

    pub fn for_agent(node: Arc<defra_node::Node>, node_did: &str, agent_did: &str) -> Result<Self> {
        anyhow::ensure!(
            agent_did != node_did && node.node_identity_did() != Some(agent_did),
            "plugin data requires a distinct agent identity"
        );
        ::identity::Did::new(node_did.to_owned())?;
        Ok(Self {
            access: DataAccess::Node(node, ::identity::Did::new(agent_did.to_owned())?),
            caller_did: agent_did.to_owned(),
        })
    }

    pub fn operator(
        access: gents_store::config_client::ConfigAccess,
        node_did: &str,
    ) -> Result<Self> {
        ::identity::Did::new(node_did.to_owned())?;
        let caller_did = access.actor_did().ok_or_else(|| {
            anyhow::anyhow!("plugin data requires an explicitly authenticated caller")
        })?;
        anyhow::ensure!(
            caller_did != node_did,
            "client plugin work requires the user's identity"
        );
        Ok(Self {
            access: DataAccess::Operator(Box::new(access)),
            caller_did,
        })
    }
}

/// The requests of one `data_calls` round.
pub struct Batch {
    pub requests: Vec<DataCall>,
    pub state: Option<Value>,
}

/// The output's `data_calls.requests` array of [`DataCall`] and its optional
/// sibling `data_calls.state`, the form of every service; a `data_calls` that is
/// not an object (an echoed `true`) is no request.
pub fn parse_batch(output: &Value) -> Result<Option<Batch>, String> {
    let Some(calls) = output.get("data_calls").and_then(Value::as_object) else {
        return Ok(None);
    };
    let requests = calls
        .get("requests")
        .filter(|requests| requests.is_array())
        .ok_or("data_calls.requests must be an array")?;
    let requests: Vec<DataCall> = serde_json::from_value(requests.clone()).map_err(|error| {
        format!(
            "data_calls.requests needs DataCall requests {{id, query: {{collection, filter, order?, limit, offset?, fields}}}}: {error}"
        )
    })?;
    let mut ids = BTreeSet::new();
    if requests.is_empty()
        || requests.len() > MAX_QUERIES_PER_ROUND
        || requests
            .iter()
            .any(|request| request.id.is_empty() || !ids.insert(request.id.as_str()))
    {
        return Err(format!(
            "data_calls.requests needs 1 to {MAX_QUERIES_PER_ROUND} requests with distinct nonempty ids"
        ));
    }
    Ok(Some(Batch {
        requests,
        state: calls.get("state").filter(|state| !state.is_null()).cloned(),
    }))
}

/// `data_calls.query` for one call of a plugin that declares data.
pub struct Session {
    caller: DataCaller,
    declaration: PluginDataDeclaration,
}

impl Session {
    /// Offered only when the plugin declares data and the caller supplied its context.
    pub fn for_call(
        declaration: Option<&PluginDataDeclaration>,
        caller: Option<DataCaller>,
    ) -> Option<Self> {
        Some(Self {
            caller: caller?,
            declaration: declaration?.clone(),
        })
    }

    /// Answers by request id; a refused or failed query is that request's
    /// error result, never the call's.
    pub async fn serve(
        &self,
        requests: Vec<DataCall>,
        max_output_bytes: usize,
    ) -> Map<String, Value> {
        let mut results = Map::new();
        for DataCall { id, query } in requests {
            let answer = match self.query(&query, max_output_bytes).await {
                Ok(rows) => RoundResult::Result {
                    result: DataQueryResponse { rows },
                },
                Err(error) => RoundResult::Error {
                    error: format!("{error:#}"),
                },
            };
            results.insert(
                id,
                serde_json::to_value(answer).expect("a round result has a JSON encoding"),
            );
        }
        results
    }

    async fn query(
        &self,
        query: &DataQuery,
        max_output_bytes: usize,
    ) -> Result<Vec<Map<String, Value>>> {
        use gents_tools::builtin::defra_query::{
            build_paged_query, CollectionScope, DefraQueryParams,
        };
        let data = &self.declaration;
        let collection = query.collection.as_str();
        let in_own_sdl = data.collections.iter().any(|c| c == collection);
        let runtime_or_protected =
            gents_documents::callback_plugin_handler::writable_output_collection(collection)
                .is_err();
        let query_granted = data.ops.iter().any(|op| op == "query");
        if !query_admitted(query.limit, in_own_sdl, runtime_or_protected, query_granted) {
            if !in_own_sdl || runtime_or_protected {
                bail!(
                    "collection {collection:?} is outside the plugin's declared data grant; query only the declared data.collections"
                );
            }
            if !query_granted {
                bail!("the plugin did not declare query in data.ops; declare it to read its collections");
            }
            bail!(
                "limit {} is out of bounds; ask for 1 to {MAX_QUERY_LIMIT} rows and page with offset",
                query.limit
            );
        }
        let mut filter = Value::Object(query.filter.clone());
        substitute_json(&mut filter, &self.caller.caller_did);
        let params = DefraQueryParams {
            collection: collection.to_owned(),
            fields: query.fields.clone(),
            filter: Some(filter.clone()),
            limit: Some(query.limit),
        };
        let order = query.order.clone().map(Value::Object);
        let text = build_paged_query(
            &params,
            &CollectionScope::restricted(vec![collection.to_owned()]),
            order.as_ref(),
            query.offset.unwrap_or(0),
        )?;
        let response = match &self.caller.access {
            DataAccess::Node(node, actor) => {
                gents_store::config_client::ConfigAccess::transact_local_readonly(
                    node,
                    Some(actor.clone()),
                    "plugin_data_calls.query",
                    |txn| {
                        let (filter, text) = (&filter, &text);
                        Box::pin(async move {
                            gents_tools::builtin::defra_query::validate_filter(
                                txn, collection, filter,
                            )
                            .await?;
                            Ok::<_, anyhow::Error>(txn.execute(text).await?)
                        })
                    },
                )
                .await?
            }
            DataAccess::Operator(access) => {
                access
                    .transact_readonly("plugin_data_calls.query", |txn| {
                        let (filter, text) = (&filter, &text);
                        Box::pin(async move {
                            gents_tools::builtin::defra_query::validate_filter(
                                txn, collection, filter,
                            )
                            .await?;
                            Ok::<_, anyhow::Error>(txn.execute(text).await?)
                        })
                    })
                    .await?
            }
        };
        let mut used = 0;
        // ponytail: rows past the output budget are dropped from the end; the plugin pages with offset.
        Ok(response["data"][collection]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_object)
            .take_while(|row| {
                used += serde_json::to_string(row).map_or(usize::MAX, |text| text.len());
                used <= max_output_bytes
            })
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::application_write::Observation as WriteFacts;
    use gents_protocol::job::{DataOp, PluginDataDeclaration};
    use serde_json::{json, Map, Value};
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn agent_data_caller_never_uses_a_node_identity() {
        let node_db = gents_inference::test_support::test_db(
            "agent-data-caller-never-uses-a-node-iden",
            gents_session::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = node_db.node.clone();
        let runtime = node.node_identity_did().unwrap().to_owned();
        let routing = "did:key:routing";
        assert!(DataCaller::for_agent(node.clone(), routing, &runtime).is_err());
        assert!(DataCaller::for_agent(node.clone(), routing, routing).is_err());
        let agent = "did:key:agent";
        let caller = DataCaller::for_agent(node.clone(), routing, agent).unwrap();
        assert_eq!(caller.actor().unwrap().as_str(), agent);
        assert_eq!(caller.caller_did, agent);
        node.shutdown().await.expect("node shutdown");
    }

    fn grant() -> PluginData {
        PluginData {
            declaration: PluginDataDeclaration {
                collections: vec!["Note".into()],
                ops: vec!["insert".into(), "update".into()],
                stamped: BTreeMap::from([("owner".to_owned(), Stamp::CallerDid)]),
            },
            collection_fields: BTreeMap::from([(
                "Note".to_owned(),
                vec!["owner".into(), "body".into()],
            )]),
            unique_keys: BTreeMap::new(),
        }
    }

    fn tool_output() -> StampContext {
        StampContext::ToolOutput {
            caller_did: "did:key:node".into(),
            session_id: "s".into(),
            request_doc_id: "r".into(),
            tool_call_doc_id: "t".into(),
        }
    }

    fn fields(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    fn insert(collection: &str, body: Value) -> DataWrite {
        DataWrite {
            op: DataOp::Insert,
            collection: collection.into(),
            doc_id: None,
            fields: fields(body),
            on_conflict: None,
        }
    }

    fn admit(
        data: &PluginData,
        ctx: &StampContext,
        writes: Vec<DataWrite>,
    ) -> Result<Vec<AdmittedWrite>, WriteRefusal> {
        admit_write(data, ctx, writes)
    }

    fn clear_observation(invocation: Invocation) -> WriteObservation {
        WriteObservation {
            in_own_sdl: true,
            runtime_or_protected: false,
            actor_is_invoker: true,
            op_granted: true,
            stamped_supplied_by_guest: false,
            invocation,
            applicable: Vec::new(),
            write: crate::plugin::application_write::Observation {
                operation: "create".into(),
                granted: true,
                application: true,
                protected: false,
                credential: false,
                bounded: true,
                targets: 1,
                limit: 100,
                preview: false,
                digest_matches: true,
            },
        }
    }

    #[test]
    fn write_outside_data_collections_is_refused() {
        let refused = admit(&grant(), &tool_output(), vec![insert("Other", json!({}))]);
        assert_eq!(refused.unwrap_err(), WriteRefusal::Other);
        let runtime = admit(&grant(), &tool_output(), vec![insert("Request", json!({}))]);
        assert_eq!(runtime.unwrap_err(), WriteRefusal::Other);
    }

    #[test]
    fn write_op_outside_data_ops_is_refused() {
        let delete = DataWrite {
            op: DataOp::Delete,
            doc_id: Some("bae-1".into()),
            ..insert("Note", json!({}))
        };
        assert_eq!(
            admit(&grant(), &tool_output(), vec![delete]).unwrap_err(),
            WriteRefusal::Other
        );
    }

    #[test]
    fn update_and_delete_need_a_doc_id() {
        let mut data = grant();
        data.declaration.ops.push("delete".into());
        for op in [DataOp::Update, DataOp::Delete] {
            let write = DataWrite {
                op,
                ..insert("Note", json!({"body": "x"}))
            };
            assert_eq!(
                admit(&data, &tool_output(), vec![write.clone()]).unwrap_err(),
                WriteRefusal::Other
            );
            let with_id = DataWrite {
                doc_id: Some("bae-1".into()),
                ..write
            };
            assert!(admit(&data, &tool_output(), vec![with_id]).is_ok());
        }
    }

    #[test]
    fn guest_value_for_a_stamped_field_is_refused() {
        let write = insert("Note", json!({"owner": "did:key:forged"}));
        assert_eq!(
            admit(&grant(), &tool_output(), vec![write]).unwrap_err(),
            WriteRefusal::Other
        );
    }

    fn invocation_wire(invocation: Invocation) -> String {
        match invocation {
            Invocation::ToolCall => "tool_call",
            Invocation::JobStep => "job_step",
            Invocation::Callback => "callback",
            Invocation::Ux => "ux",
        }
        .to_owned()
    }

    fn context_of(invocation: &str) -> StampContext {
        let caller_did = "did:key:node".to_owned();
        match invocation {
            "tool_call" => tool_output(),
            "job_step" => StampContext::JobStep {
                caller_did,
                session_id: "s".into(),
                request_doc_id: "r".into(),
                tool_call_doc_id: "t".into(),
                job_row_doc_id: "j".into(),
                chain_root_doc_id: "c".into(),
            },
            "callback" => StampContext::Callback { caller_did },
            "ux" => StampContext::Ux { caller_did },
            other => panic!("unmodelled invocation {other}"),
        }
    }

    #[test]
    fn granted_writes_are_admitted_as_the_lean_model() {
        use gents_lean_vocab::families::plugin_data_writes_admit::{AdmitOutput, RefusalWire};
        gents_lean_vocab::families::plugin_data_writes_admit::replay::<DataOp, String, String, Stamp>(
            |i| {
                let collection = if i.collection_granted {
                    "Note"
                } else {
                    "Other"
                };
                let mut data = PluginData {
                    declaration: PluginDataDeclaration {
                        collections: vec!["Note".into()],
                        ops: if i.op_granted {
                            vec![op_name(i.op).into()]
                        } else {
                            Vec::new()
                        },
                        stamped: BTreeMap::from([("owner".to_owned(), i.stamp)]),
                    },
                    collection_fields: BTreeMap::new(),
                    unique_keys: BTreeMap::new(),
                };
                if i.fields_known {
                    data.collection_fields
                        .insert(collection.into(), vec!["owner".into(), "body".into()]);
                }
                let mut body = json!({"body": "x"});
                if i.guest_stamped {
                    body["owner"] = json!("did:key:forged");
                }
                let write = DataWrite {
                    op: i.op,
                    collection: collection.into(),
                    doc_id: i.doc_id.then(|| "bae-1".to_owned()),
                    fields: fields(body),
                    on_conflict: i.on_conflict.then_some(OnConflict::Ignore),
                };
                let verdict = admit(
                    &data,
                    &context_of(&i.invocation),
                    vec![write; i.count as usize],
                );
                AdmitOutput {
                    refusal: verdict.as_ref().err().map(|refusal| match refusal {
                        WriteRefusal::StampUnavailable { stamp, invocation } => RefusalWire {
                            kind: "stamp_unavailable".to_owned(),
                            stamp: Some(*stamp),
                            invocation: Some(invocation_wire(*invocation)),
                        },
                        WriteRefusal::Other => RefusalWire {
                            kind: "other".to_owned(),
                            stamp: None,
                            invocation: None,
                        },
                    }),
                    stamped_fields: verdict
                        .map(|admitted| {
                            data.declaration
                                .stamped
                                .keys()
                                .filter(|field| admitted[0].write.fields.contains_key(*field))
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default(),
                }
            },
        );
    }

    #[test]
    fn stamps_apply_only_to_fields_of_the_written_collection() {
        let mut data = grant();
        data.declaration.collections.push("Log".into());
        data.collection_fields
            .insert("Log".into(), vec!["line".into()]);
        let admitted = admit(
            &data,
            &tool_output(),
            vec![
                insert("Note", json!({"body": "a"})),
                insert("Log", json!({"line": "b"})),
            ],
        )
        .unwrap();
        assert_eq!(admitted[0].write.fields["owner"], json!("did:key:node"));
        assert!(!admitted[1].write.fields.contains_key("owner"));
    }

    #[test]
    fn stamp_without_a_value_in_the_context_refuses_the_output() {
        let mut data = grant();
        data.declaration
            .stamped
            .insert("owner".into(), Stamp::JobRowDocId);
        let refusal = admit(
            &data,
            &tool_output(),
            vec![insert("Note", json!({"body": "a"}))],
        )
        .unwrap_err();
        assert_eq!(
            refusal,
            WriteRefusal::StampUnavailable {
                stamp: Stamp::JobRowDocId,
                invocation: Invocation::ToolCall
            }
        );
        let message = refusal.to_string();
        assert!(message.contains("job_row_doc_id") && message.contains("tool"));

        let callback = StampContext::Callback {
            caller_did: "did:key:node".into(),
        };
        data.declaration
            .stamped
            .insert("owner".into(), Stamp::SessionId);
        assert_eq!(
            admit(&data, &callback, vec![insert("Note", json!({}))]).unwrap_err(),
            WriteRefusal::StampUnavailable {
                stamp: Stamp::SessionId,
                invocation: Invocation::Callback
            }
        );
        assert_eq!(callback.value(Stamp::CallerDid), Some("did:key:node"));

        let step = StampContext::JobStep {
            caller_did: "n".into(),
            session_id: "s".into(),
            request_doc_id: "r".into(),
            tool_call_doc_id: "t".into(),
            job_row_doc_id: "j".into(),
            chain_root_doc_id: "c".into(),
        };
        assert_eq!(step.value(Stamp::JobRowDocId), Some("j"));
        assert_eq!(step.value(Stamp::ChainRootDocId), Some("c"));
        assert_eq!(tool_output().value(Stamp::ChainRootDocId), None);
    }

    #[test]
    fn more_than_100_targets_are_refused() {
        let writes = |count| vec![insert("Note", json!({"body": "x"})); count];
        assert_eq!(
            admit(&grant(), &tool_output(), writes(100)).unwrap().len(),
            100
        );
        assert_eq!(
            admit(&grant(), &tool_output(), writes(101)).unwrap_err(),
            WriteRefusal::Other
        );
    }

    #[test]
    fn on_conflict_is_allowed_only_on_insert() {
        let with_conflict = |op, doc_id: Option<&str>| DataWrite {
            op,
            doc_id: doc_id.map(str::to_owned),
            on_conflict: Some(OnConflict::Ignore),
            ..insert("Note", json!({"body": "x"}))
        };
        assert!(admit(
            &grant(),
            &tool_output(),
            vec![with_conflict(DataOp::Insert, None)]
        )
        .is_ok());
        assert_eq!(
            admit(
                &grant(),
                &tool_output(),
                vec![with_conflict(DataOp::Update, Some("bae-1"))]
            )
            .unwrap_err(),
            WriteRefusal::Other
        );
    }

    #[test]
    fn applicable_stamps_keep_only_fields_of_the_written_collection() {
        let declared = vec![
            ("job_row_doc_id".to_owned(), Stamp::JobRowDocId),
            ("node_did".to_owned(), Stamp::CallerDid),
        ];
        assert_eq!(
            applicable_stamps(&declared, &["node_did".to_owned(), "name".to_owned()]),
            vec![("node_did".to_owned(), Stamp::CallerDid)]
        );
    }

    #[test]
    fn admit_write_observation_reports_the_first_unavailable_stamp_before_other_refusals() {
        let mut callback = clear_observation(Invocation::Callback);
        callback.applicable = vec![("job".into(), Stamp::JobRowDocId)];
        callback.op_granted = false;
        assert_eq!(
            admit_write_observation(&callback),
            Err(WriteRefusal::StampUnavailable {
                stamp: Stamp::JobRowDocId,
                invocation: Invocation::Callback
            })
        );

        assert_eq!(
            admit_write_observation(&clear_observation(Invocation::JobStep)),
            Ok(())
        );
        let breaks: [fn(&mut WriteObservation); 7] = [
            |o| o.in_own_sdl = false,
            |o| o.runtime_or_protected = true,
            |o| o.actor_is_invoker = false,
            |o| o.op_granted = false,
            |o| o.stamped_supplied_by_guest = true,
            |o| o.write.granted = false,
            |o| o.write.credential = true,
        ];
        for (index, apply) in breaks.into_iter().enumerate() {
            let mut observation = clear_observation(Invocation::JobStep);
            apply(&mut observation);
            assert_eq!(
                admit_write_observation(&observation),
                Err(WriteRefusal::Other),
                "break {index}"
            );
        }
    }

    #[test]
    fn substitute_caller_did_replaces_only_the_placeholder() {
        let filter = BTreeMap::from([
            ("owner".to_owned(), "$caller.did".to_owned()),
            ("name".to_owned(), "x".to_owned()),
            ("note".to_owned(), "has $caller.did inside".to_owned()),
        ]);
        assert_eq!(
            substitute_caller_did(&filter, "did:key:n"),
            BTreeMap::from([
                ("owner".to_owned(), "did:key:n".to_owned()),
                ("name".to_owned(), "x".to_owned()),
                ("note".to_owned(), "has $caller.did inside".to_owned()),
            ])
        );
    }

    /// `ToolPolicy.ApplicationWrite.Observation` as the native write observation
    /// that `application_write` already owns; Lean's `blocked` is its `protected`.
    fn write_facts(
        op: &str,
        w: &gents_lean_vocab::families::plugin_resources_data_writes::Observation,
    ) -> WriteFacts {
        WriteFacts {
            operation: op.to_owned(),
            granted: w.granted,
            application: w.application,
            protected: w.blocked,
            credential: w.credential,
            bounded: w.bounded,
            targets: usize::try_from(w.targets).unwrap(),
            limit: usize::try_from(w.limit).unwrap(),
            preview: w.preview,
            digest_matches: w.digest_matches,
        }
    }

    /// Lean `PluginData.applicableStamps`, `admitted` and `refusal` through the
    /// pure core `plugin::data::admit_write_observation`, which
    /// `plugin::application_write::apply_plugin_writes_in_txn` calls inside the
    /// output's transaction.
    #[test]
    fn generated_plugin_data_writes_drive_admit_write() {
        gents_lean_vocab::families::plugin_resources_data_writes::replay::<Invocation, String, Stamp>(
            |i| {
                let declared: Vec<(String, Stamp)> = i
                    .declared_stamps
                    .iter()
                    .map(|entry| (entry.field.clone(), entry.stamp))
                    .collect();
                let applicable = applicable_stamps(&declared, &i.collection_fields);
                let observation = WriteObservation {
                    in_own_sdl: i.in_own_sdl,
                    runtime_or_protected: i.runtime_or_protected,
                    actor_is_invoker: i.actor_is_invoker,
                    op_granted: i.op_granted,
                    stamped_supplied_by_guest: i.stamped_supplied_by_guest,
                    invocation: i.invocation,
                    applicable: applicable.clone(),
                    write: write_facts(&i.op, &i.write),
                };
                let verdict = admit_write_observation(&observation);
                gents_lean_vocab::families::plugin_resources_data_writes::WriteOutput {
                    applicable: applicable
                        .into_iter()
                        .map(|(field, stamp)| {
                            gents_lean_vocab::families::plugin_resources_data_writes::StampField {
                                field,
                                stamp,
                            }
                        })
                        .collect(),
                    admitted: verdict.is_ok(),
                    refusal: verdict.err().map(|refusal| match refusal {
                        WriteRefusal::StampUnavailable { stamp, invocation } => {
                            gents_lean_vocab::families::plugin_resources_data_writes::RefusalWire {
                                kind: "stamp_unavailable".to_owned(),
                                stamp: Some(stamp),
                                invocation: Some(invocation),
                            }
                        }
                        WriteRefusal::Other => {
                            gents_lean_vocab::families::plugin_resources_data_writes::RefusalWire {
                                kind: "other".to_owned(),
                                stamp: None,
                                invocation: None,
                            }
                        }
                    }),
                }
            },
        );
    }

    #[test]
    fn generated_plugin_data_queries_drive_query_admitted() {
        gents_lean_vocab::families::plugin_resources_data_queries::replay(|i| {
            query_admitted(
                u32::try_from(i.limit).unwrap(),
                i.in_own_sdl,
                i.runtime_or_protected,
                i.query_granted,
            )
        });
    }

    #[test]
    fn generated_plugin_data_filters_drive_substitution() {
        gents_lean_vocab::families::plugin_resources_data_filters::replay(|i| {
            let filter: BTreeMap<String, String> = i
                .filter
                .iter()
                .map(|entry| (entry.field.clone(), entry.value.clone()))
                .collect();
            assert_eq!(filter.len(), i.filter.len(), "filter fields are distinct");
            let order: Vec<&str> = i.filter.iter().map(|entry| entry.field.as_str()).collect();
            let mut substituted = substitute_caller_did(&filter, &i.caller_did);
            order
                .into_iter()
                .map(|field| {
                    gents_lean_vocab::families::plugin_resources_data_filters::FilterEntry {
                        field: field.to_owned(),
                        value: substituted.remove(field).unwrap(),
                    }
                })
                .collect()
        });
    }

    /// The read-then-insert decision the transaction applies to the row it read.
    #[test]
    fn generated_plugin_data_notes_drive_insert_note() {
        gents_lean_vocab::families::plugin_resources_data_notes::replay::<InsertOutcome, OnConflict>(
            |i| insert_note(i.existing.as_deref(), &i.new, i.on_conflict),
        );
    }
}
