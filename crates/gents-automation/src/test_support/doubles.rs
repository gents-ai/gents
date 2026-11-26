use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use events::{Bus, ChannelBus, EventName, Message, Subscription, Update};
use gents_documents::task_delivery::TriggerKind;
use gents_documents::CommandRun;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::{ConfigApplyTxn, ConfigRead};
use gents_store::subscription_source::{ChangeSubscription, UpdateSubscriptionSource};
use gents_store::test_support::STALL_CEILING;
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Notify};

use crate::goal::history::{GoalCompositeCommit, ReuseFacts};
use crate::triggers::engine::command::CommandHost;
use crate::triggers::engine::{FireMaterialization, MaterializeError, MaterializerHandle};
use crate::triggers::snapshot_observer::RuntimeSnapshotObserver;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone)]
pub(crate) struct MockUpdateSubscriptionSource {
    bus: Arc<ChannelBus>,
    pub(crate) subscriber_count: Arc<AtomicUsize>,
    pub(crate) subscriber_notify: Arc<Notify>,
}

impl MockUpdateSubscriptionSource {
    pub(crate) fn new() -> Self {
        Self {
            bus: Arc::new(ChannelBus::new()),
            subscriber_count: Arc::new(AtomicUsize::new(0)),
            subscriber_notify: Arc::new(Notify::new()),
        }
    }

    pub(crate) fn close(&self) {
        self.bus.close();
    }

    pub(crate) fn publish_message(&self, message: Message) {
        self.bus.publish(message);
    }

    pub(crate) fn publish_update(
        &self,
        collection_id: impl Into<String>,
        doc_id: impl Into<String>,
    ) {
        let collection_id = collection_id.into();
        let doc_id = doc_id.into();
        let block = format!("{collection_id}:{doc_id}").into_bytes();
        let cid = defra_core::block::generate_cid_from_bytes(&block)
            .expect("synthetic update block bytes must produce a CID");
        let update = Update::new(doc_id, cid, collection_id, block, false, true);
        self.bus.publish(Message::update(update));
    }
}

impl Default for MockUpdateSubscriptionSource {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdateSubscriptionSource for MockUpdateSubscriptionSource {
    fn subscribe_updates(&self) -> Subscription {
        let subscription = self.bus.subscribe(&[EventName::Update]);
        self.subscriber_count.fetch_add(1, Ordering::SeqCst);
        self.subscriber_notify.notify_waiters();
        subscription
    }

    fn subscribe_changes(&self, collection_ids: Vec<String>) -> ChangeSubscription {
        let subscription = self.bus.subscribe_collection_changes(collection_ids);
        self.subscriber_count.fetch_add(1, Ordering::SeqCst);
        self.subscriber_notify.notify_waiters();
        subscription.into()
    }
}

pub(crate) struct ReadScope {
    pub(crate) rows: Vec<AgentRequestRow>,
    pub(crate) refuse_history: bool,
    pub(crate) history_reads: AtomicUsize,
}

#[async_trait::async_trait]
impl ConfigRead for ReadScope {
    async fn execute_read(
        &self,
        query: &str,
    ) -> Result<Value, gents_store::config_client::ConfigClientError> {
        if query.contains("AgentRequest(") {
            let rows = self
                .rows
                .iter()
                .map(|row| {
                    let mut value = serde_json::to_value(row).unwrap();
                    value["_docID"] = json!(row.doc_id);
                    value
                })
                .collect::<Vec<_>>();
            return Ok(json!({"data":{"AgentRequest":rows}}));
        }
        self.history_reads.fetch_add(1, Ordering::Relaxed);
        if self.refuse_history {
            return Err(gents_store::graphql::GraphqlResponseError::from(
                gents_protocol::database_error::GraphqlFailure::new(
                    "Goal",
                    json!([{"message": "retained Goal history is unavailable"}]),
                ),
            )
            .into());
        }
        Ok(json!({"data":{"Goal":[]}}))
    }
}

pub(crate) struct HistoryReader {
    pub(crate) calls: Mutex<Vec<Vec<String>>>,
    pub(crate) fault: Option<&'static str>,
}

#[async_trait::async_trait]
impl ConfigRead for HistoryReader {
    async fn execute_read(
        &self,
        query: &str,
    ) -> Result<Value, gents_store::config_client::ConfigClientError> {
        if query.contains("_commits(") {
            return Ok(json!({"data":{"_commits":(0..67).rev().map(|index| json!({
                "cid":format!("commit-{index:03}"), "height":index + 1, "fieldName":"_C",
                "heads":if index == 0 {vec![]} else {vec![json!({"cid":format!("commit-{:03}",index-1),"fieldName":"_C"})]}
            })).collect::<Vec<_>>()}}));
        }
        let Some((_, suffix)) = query.split_once("cid: [") else {
            return Ok(json!({"data":{"Goal":[{"_docID":"goal-document"}]}}));
        };
        let (raw, _) = suffix.split_once(']').unwrap();
        let cids: Vec<String> = serde_json::from_str(&format!("[{raw}]"))?;
        let batch = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(cids.clone());
            calls.len()
        };
        if batch == 2 && self.fault == Some("unavailable") {
            return Err(gents_store::config_client::ConfigClientError::Json(
                serde_json::Error::io(std::io::Error::other("retained block unavailable")),
            ));
        }
        let mut rows = cids.iter().map(|cid| json!({
            "_docID":"goal-document", "node_did":"owner", "session_id":"chat", "goal_id":"goal",
            "status":"active", "objective":cid, "assignment_root_request_doc_id":"physical-root"
        })).collect::<Vec<_>>();
        if batch == 2 && self.fault == Some("omitted") {
            rows.pop();
        }
        if batch == 2 && self.fault == Some("foreign") {
            rows[0]["node_did"] = json!("other-owner");
        }
        if batch == 2 && self.fault == Some("projection") {
            rows[0]["status"] = json!("paused");
        }
        Ok(json!({"data":{"Goal":rows}}))
    }
}

pub(crate) struct UnchangedAssignmentReader {
    pub(crate) inner: HistoryReader,
    pub(crate) metadata_fault: Option<&'static str>,
}

#[async_trait::async_trait]
impl ConfigRead for UnchangedAssignmentReader {
    async fn execute_read(
        &self,
        query: &str,
    ) -> Result<Value, gents_store::config_client::ConfigClientError> {
        let mut response = self.inner.execute_read(query).await?;
        if let Some(commits) = response
            .pointer_mut("/data/_commits")
            .and_then(Value::as_array_mut)
        {
            for commit in commits {
                let height = commit["height"].as_u64().unwrap();
                commit["links"] = json!([{
                    "cid":format!("field-{height}"),
                    "height":height,
                    "fieldName":if height == 1 {"goal_id"} else {"tokens_used"}
                }]);
                for head in commit["heads"].as_array_mut().unwrap() {
                    head["height"] = json!(height - 1);
                }
                if height == 21 {
                    match self.metadata_fault {
                        Some("missing_link") => {
                            commit["links"][0].as_object_mut().unwrap().remove("height");
                        }
                        Some("unknown_link") => {
                            commit["links"][0]["fieldName"] = json!("future_field")
                        }
                        Some("immutable") => commit["links"][0]["fieldName"] = json!("_I"),
                        Some("assignment") => commit["links"][0]["fieldName"] = json!("status"),
                        Some("merge") => commit["heads"]
                            .as_array_mut()
                            .unwrap()
                            .push(json!({"cid":"commit-000", "height":1,"fieldName":"_C"})),
                        Some("missing_parent") => commit["heads"][0]["cid"] = json!("unavailable"),
                        Some("parent_height") => commit["heads"][0]["height"] = json!(1),
                        _ => {}
                    }
                }
            }
        }
        Ok(response)
    }
}

pub(crate) struct NativeReader<'txn, 'node> {
    txn: &'txn ConfigApplyTxn<'node>,
    full: bool,
    pub(crate) cids: Mutex<Vec<String>>,
    pub(crate) facts: Mutex<Vec<ReuseFacts>>,
}

impl<'txn, 'node> NativeReader<'txn, 'node> {
    pub(crate) fn new(txn: &'txn ConfigApplyTxn<'node>, full: bool) -> Self {
        Self {
            txn,
            full,
            cids: Mutex::new(Vec::new()),
            facts: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl ConfigRead for NativeReader<'_, '_> {
    async fn execute_read(
        &self,
        query: &str,
    ) -> Result<Value, gents_store::config_client::ConfigClientError> {
        let mut response = self.txn.execute(query).await?;
        if let Some((_, rest)) = query.split_once("cid: [") {
            let raw = rest.split_once(']').unwrap().0;
            let cids: Vec<String> = serde_json::from_str(&format!("[{raw}]"))?;
            self.cids.lock().unwrap().extend(cids);
        }
        if let Some(commits) = response
            .pointer_mut("/data/_commits")
            .and_then(Value::as_array_mut)
        {
            for commit in commits {
                let decoded: GoalCompositeCommit = serde_json::from_value(commit.clone())?;
                self.facts.lock().unwrap().push(decoded.reuse_facts());
                if self.full {
                    commit.as_object_mut().unwrap().remove("links");
                }
            }
        }
        Ok(response)
    }
}

pub(crate) type MaterializeCall = (Option<String>, TriggerKind, String);

pub(crate) type SupersedeCall = String;

pub(crate) type NonterminalRequests = Arc<Mutex<RapidHashMap<String, Vec<String>>>>;

fn remove_except<K>(
    requests: &mut RapidHashMap<K, Vec<String>>,
    key: &K,
    excluded_request_id: Option<&str>,
) -> Vec<String>
where
    K: Clone + Eq + std::hash::Hash,
{
    let Some(request_ids) = requests.remove(key) else {
        return Vec::new();
    };
    let (retained, removed): (Vec<_>, Vec<_>) = request_ids
        .into_iter()
        .partition(|request_id| Some(request_id.as_str()) == excluded_request_id);
    if !retained.is_empty() {
        requests.insert(key.clone(), retained);
    }
    removed
}

#[derive(Clone)]
struct MaterializeGate {
    entered_tx: mpsc::UnboundedSender<()>,
    release: Arc<Notify>,
}

pub(crate) struct SpyMaterializer {
    pub(crate) materialize_calls: Arc<Mutex<Vec<MaterializeCall>>>,
    pub(crate) materialize_goal_objectives: Arc<Mutex<Vec<Option<String>>>>,
    pub(crate) materialized_request_ids: Arc<Mutex<RapidHashSet<String>>>,
    pub(crate) materialize_source_doc_ids: Arc<Mutex<Vec<Option<String>>>>,
    pub(crate) next_request_id: AtomicUsize,
    pub(crate) nonterminal_for: NonterminalRequests,
    pub(crate) gate_dids: Arc<Mutex<Vec<String>>>,
    pub(crate) supersede_dids: Arc<Mutex<Vec<String>>>,
    pub(crate) supersede_calls: Arc<Mutex<Vec<SupersedeCall>>>,
    pub(crate) superseded_request_ids: Arc<Mutex<Vec<String>>>,
    materialize_gate: Mutex<Option<MaterializeGate>>,
    pub(crate) group_markers: Arc<Mutex<RapidHashSet<(String, String, String)>>>,
    persist_group_markers_for_did: Mutex<Option<String>>,
    track_materialized_nonterminal: AtomicBool,
    panic_on_materialize: bool,
}

impl SpyMaterializer {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            materialize_calls: Arc::new(Mutex::new(Vec::new())),
            materialize_goal_objectives: Arc::new(Mutex::new(Vec::new())),
            materialized_request_ids: Arc::new(Mutex::new(RapidHashSet::new())),
            materialize_source_doc_ids: Arc::new(Mutex::new(Vec::new())),
            next_request_id: AtomicUsize::new(0),
            nonterminal_for: Arc::new(Mutex::new(RapidHashMap::new())),
            gate_dids: Arc::new(Mutex::new(Vec::new())),
            supersede_dids: Arc::new(Mutex::new(Vec::new())),
            supersede_calls: Arc::new(Mutex::new(Vec::new())),
            superseded_request_ids: Arc::new(Mutex::new(Vec::new())),
            materialize_gate: Mutex::new(None),
            group_markers: Arc::new(Mutex::new(RapidHashSet::new())),
            persist_group_markers_for_did: Mutex::new(None),
            track_materialized_nonterminal: AtomicBool::new(false),
            panic_on_materialize: false,
        })
    }

    pub(crate) fn panicking() -> Arc<Self> {
        let mut inner = Arc::try_unwrap(Self::new()).unwrap_or_else(|_| unreachable!());
        inner.panic_on_materialize = true;
        Arc::new(inner)
    }

    pub(crate) fn calls(&self) -> Vec<MaterializeCall> {
        self.materialize_calls.lock().unwrap().clone()
    }

    pub(crate) fn goal_objectives(&self) -> Vec<Option<String>> {
        self.materialize_goal_objectives.lock().unwrap().clone()
    }

    pub(crate) fn source_doc_ids(&self) -> Vec<Option<String>> {
        self.materialize_source_doc_ids.lock().unwrap().clone()
    }

    pub(crate) fn supersede_calls(&self) -> Vec<SupersedeCall> {
        self.supersede_calls.lock().unwrap().clone()
    }

    pub(crate) fn gate_dids(&self) -> Vec<String> {
        self.gate_dids.lock().unwrap().clone()
    }

    pub(crate) fn supersede_dids(&self) -> Vec<String> {
        self.supersede_dids.lock().unwrap().clone()
    }

    pub(crate) fn superseded_request_ids(&self) -> Vec<String> {
        self.superseded_request_ids.lock().unwrap().clone()
    }

    pub(crate) fn nonterminal_count_for(
        &self,
        trigger_id: &str,
        _trigger_kind: TriggerKind,
    ) -> usize {
        self.nonterminal_for
            .lock()
            .unwrap()
            .get(trigger_id)
            .map(Vec::len)
            .unwrap_or(0)
    }

    pub(crate) fn mark_nonterminal(&self, trigger_id: &str, trigger_kind: TriggerKind) {
        let next_index = self.nonterminal_count_for(trigger_id, trigger_kind);
        let request_id = format!("spy-prior-{}-{next_index}", trigger_kind.as_str());
        self.mark_nonterminal_request(trigger_id, trigger_kind, request_id);
    }

    pub(crate) fn mark_nonterminal_request(
        &self,
        trigger_id: &str,
        _trigger_kind: TriggerKind,
        request_id: impl Into<String>,
    ) {
        self.nonterminal_for
            .lock()
            .unwrap()
            .entry(trigger_id.to_owned())
            .or_default()
            .push(request_id.into());
    }

    pub(crate) fn track_materialized_nonterminal(&self) {
        self.track_materialized_nonterminal
            .store(true, Ordering::SeqCst);
    }

    pub(crate) fn set_materialize_gate(
        &self,
        entered_tx: mpsc::UnboundedSender<()>,
        release: Arc<Notify>,
    ) {
        *self.materialize_gate.lock().unwrap() = Some(MaterializeGate {
            entered_tx,
            release,
        });
    }

    pub(crate) fn mark_group_materialized(
        &self,
        node_did: &str,
        trigger_id: &str,
        durable_fire_key: &str,
    ) {
        self.group_markers.lock().unwrap().insert((
            node_did.to_string(),
            trigger_id.to_string(),
            durable_fire_key.to_string(),
        ));
    }

    pub(crate) fn persist_materialized_group_markers(&self, node_did: &str) {
        *self.persist_group_markers_for_did.lock().unwrap() = Some(node_did.to_string());
    }
}

impl MaterializerHandle for SpyMaterializer {
    fn materialize(
        &self,
        fire: FireMaterialization<'_>,
    ) -> Pin<Box<dyn Future<Output = Result<String, MaterializeError>> + Send + '_>> {
        if self.panic_on_materialize {
            return Box::pin(async { panic!("injected materializer panic") });
        }
        let FireMaterialization {
            task,
            trigger_id,
            trigger_kind,
            source_doc_id,
            rendered_prompt,
            rendered_goal_objective,
            durable_fire_key,
            delivery,
            ..
        } = fire;
        let entry = (
            trigger_id.map(str::to_owned),
            trigger_kind,
            rendered_prompt.to_owned(),
        );
        let calls = self.materialize_calls.clone();
        let materialized_request_ids = self.materialized_request_ids.clone();
        let goal_objectives = self.materialize_goal_objectives.clone();
        let rendered_goal_objective = rendered_goal_objective.map(str::to_owned);
        let source_doc_ids = self.materialize_source_doc_ids.clone();
        let source_doc_id = source_doc_id.map(str::to_owned);
        let nonterminal_for = self.nonterminal_for.clone();
        let nonterminal_key = trigger_id.map(str::to_owned);
        let track_materialized_nonterminal =
            self.track_materialized_nonterminal.load(Ordering::SeqCst);
        let id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        let request_id = rendered_goal_objective
            .as_ref()
            .map(|_| {
                crate::goal::task_goal_fire_identity(
                    "did:test:spy-materializer",
                    &task.task_id,
                    durable_fire_key,
                )
                .request_id
            })
            .unwrap_or_else(|| format!("req-{id}"));
        let gate = self.materialize_gate.lock().unwrap().clone();
        let marker_did = self.persist_group_markers_for_did.lock().unwrap().clone();
        let group_markers = self.group_markers.clone();
        let marker_trigger_id = trigger_id.map(str::to_owned);
        let marker_fire_key = durable_fire_key.to_owned();
        let replacement = delivery
            .filter(|prepared| prepared.replace_prior)
            .map(|prepared| {
                (
                    prepared.receipt.identity.owner_did.clone(),
                    prepared.receipt.identity.trigger_id.clone(),
                    prepared.receipt.request_id.clone(),
                )
            });
        Box::pin(async move {
            if let Some(gate) = gate {
                let _ = gate.entered_tx.send(());
                gate.release.notified().await;
            }
            if let Some((owner, trigger, request)) = replacement {
                self.supersede_active_runtime_requests_for_trigger(
                    &owner,
                    &trigger,
                    Some(&request),
                )
                .await?;
            }
            calls.lock().unwrap().push(entry);
            materialized_request_ids
                .lock()
                .unwrap()
                .insert(request_id.clone());
            goal_objectives
                .lock()
                .unwrap()
                .push(rendered_goal_objective);
            source_doc_ids.lock().unwrap().push(source_doc_id);
            if let (Some(node_did), Some(trigger_id)) = (marker_did, marker_trigger_id) {
                group_markers
                    .lock()
                    .unwrap()
                    .insert((node_did, trigger_id, marker_fire_key));
            }
            if let (true, Some(key)) = (track_materialized_nonterminal, nonterminal_key) {
                nonterminal_for
                    .lock()
                    .unwrap()
                    .entry(key)
                    .or_default()
                    .push(request_id.clone());
            }
            Ok(request_id)
        })
    }

    fn has_active_runtime_request_for_trigger(
        &self,
        node_did: &str,
        trigger_id: &str,
        excluded_request_id: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + '_>> {
        let set = self.nonterminal_for.clone();
        let dids = self.gate_dids.clone();
        let owner = node_did.to_owned();
        let trigger = trigger_id.to_owned();
        let excluded = excluded_request_id.map(str::to_owned);
        Box::pin(async move {
            dids.lock().unwrap().push(owner);
            Ok(set
                .lock()
                .unwrap()
                .get(&trigger)
                .is_some_and(|ids| ids.iter().any(|id| Some(id) != excluded.as_ref())))
        })
    }

    fn supersede_active_runtime_requests_for_trigger(
        &self,
        node_did: &str,
        trigger_id: &str,
        excluded_request_id: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<usize>> + Send + '_>> {
        let set = self.nonterminal_for.clone();
        let dids = self.supersede_dids.clone();
        let calls = self.supersede_calls.clone();
        let removed_ids = self.superseded_request_ids.clone();
        let owner = node_did.to_owned();
        let trigger = trigger_id.to_owned();
        let excluded = excluded_request_id.map(str::to_owned);
        Box::pin(async move {
            dids.lock().unwrap().push(owner);
            calls.lock().unwrap().push(trigger.clone());
            let removed = remove_except(&mut set.lock().unwrap(), &trigger, excluded.as_deref());
            let count = removed.len();
            removed_ids.lock().unwrap().extend(removed);
            Ok(count)
        })
    }

    fn recover_goal_task_fire(
        &self,
        task: &gents_documents::automation::ResolvedTask,
        durable_fire_key: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<String>>> + Send + '_>> {
        let request_ids = self.materialized_request_ids.clone();
        let request_id = crate::goal::task_goal_fire_identity(
            "did:test:spy-materializer",
            &task.task_id,
            durable_fire_key,
        )
        .request_id;
        Box::pin(async move {
            Ok(request_ids
                .lock()
                .unwrap()
                .contains(&request_id)
                .then_some(request_id))
        })
    }

    fn has_materialized_group_request(
        &self,
        node_did: &str,
        trigger_id: &str,
        durable_fire_key: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + '_>> {
        let markers = self.group_markers.clone();
        let key = (
            node_did.to_string(),
            trigger_id.to_string(),
            durable_fire_key.to_string(),
        );
        Box::pin(async move { Ok(markers.lock().unwrap().contains(&key)) })
    }
}

/// Records every run write and budget stop, waking waiters on each.
pub(crate) struct SpyCommandHost {
    pub(crate) root: tempfile::TempDir,
    pub(crate) runs: Mutex<Vec<(String, CommandRun)>>,
    pub(crate) stops: Mutex<Vec<(String, String)>>,
    changed: Notify,
}

impl SpyCommandHost {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            root: tempfile::tempdir().unwrap(),
            runs: Mutex::new(Vec::new()),
            stops: Mutex::new(Vec::new()),
            changed: Notify::new(),
        })
    }

    /// Waits for the recorded runs to satisfy `done`, failing with the last
    /// records seen once none arrive for [`STALL_CEILING`].
    pub(crate) async fn runs_when(
        &self,
        done: impl Fn(&[(String, CommandRun)]) -> bool,
    ) -> Vec<CommandRun> {
        loop {
            let changed = self.changed.notified();
            {
                let runs = self.runs.lock().unwrap();
                if done(&runs) {
                    return runs.iter().map(|(_, run)| run.clone()).collect();
                }
            }
            if tokio::time::timeout(STALL_CEILING, changed).await.is_err() {
                panic!(
                    "command runs stalled: {:#?}",
                    self.runs.lock().unwrap().clone()
                );
            }
        }
    }

    pub(crate) fn finished(runs: &[(String, CommandRun)], run_id: &str) -> bool {
        runs.iter()
            .any(|(_, run)| run.run_id == run_id && run.finished_at.is_some())
    }
}

impl CommandHost for SpyCommandHost {
    fn command_root(&self) -> BoxFuture<'_, anyhow::Result<PathBuf>> {
        let root = self.root.path().to_path_buf();
        Box::pin(async move { Ok(root) })
    }

    fn record_run<'a>(
        &'a self,
        _node_did: &'a str,
        trigger_id: &'a str,
        run: &'a CommandRun,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        self.runs
            .lock()
            .unwrap()
            .push((trigger_id.to_owned(), run.clone()));
        self.changed.notify_waiters();
        Box::pin(async { Ok(()) })
    }

    fn stop_for_budget<'a>(
        &'a self,
        _node_did: &'a str,
        trigger_id: &'a str,
        reason: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        self.stops
            .lock()
            .unwrap()
            .push((trigger_id.to_owned(), reason.to_owned()));
        Box::pin(async { Ok(()) })
    }
}

pub(crate) type ReconciliationRecord = (u64, String, Result<(), String>);

#[derive(Default)]
pub(crate) struct SubscriptionObserver(pub(crate) Mutex<Vec<ReconciliationRecord>>);

impl RuntimeSnapshotObserver for SubscriptionObserver {
    fn on_generation_published(&self, _generation: u64, _fingerprint: &str, _behaviors: &[String]) {
    }

    fn on_event_sources_reconciled(
        &self,
        generation: u64,
        fingerprint: &str,
        result: Result<(), &str>,
    ) {
        self.0.lock().unwrap().push((
            generation,
            fingerprint.to_owned(),
            result.map_err(str::to_owned),
        ));
    }
}
