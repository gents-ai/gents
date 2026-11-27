use std::sync::Arc;

use anyhow::{Context as _, Result};
use gents_lean_vocab::tla::replay::{Red, Step, Trace};
use rapidhash::RapidHashMap;
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use super::{mutation_write_gate, WriteOperation, MUTATION_WRITE_GATE_CAPACITY};
use crate::config_client::{
    is_classified_transaction_conflict, ConfigAccess, EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
    TRANSACT_CONFLICT_MAX_RETRIES,
};
use crate::test_support::embedded_conductor::{CommitReceipt, EmbeddedWriteActor};
use crate::test_support::{writer_counter, TestStore, STALL_CEILING};

type Call = (i64, i64);

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Outcome {
    Acknowledged,
    Conflict,
    Ambiguous,
}

fn writer(actor: &Value) -> i64 {
    actor["$model"]
        .as_str()
        .unwrap()
        .strip_prefix('w')
        .unwrap()
        .parse()
        .unwrap()
}

fn actor_value(field: &Value, actor: &Value) -> i64 {
    let entry = field["$function"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row[0] == *actor)
        .expect("actor key");
    entry[1].as_i64().unwrap()
}

fn call(step: &Step, actor: &Value) -> Call {
    (writer(actor), actor_value(&step.before["it"], actor))
}

// Native discard runs outside Tokio. Keep the clock live between the two held phases.
async fn expire_held_phase(duration: std::time::Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::time::resume();
}

struct Running {
    actor: Arc<EmbeddedWriteActor>,
    task: JoinHandle<Result<Outcome>>,
    read: bool,
}

impl Running {
    fn launch(access: ConfigAccess, id: Call, receipt: CommitReceipt) -> Self {
        let actor = Arc::new(EmbeddedWriteActor::default());
        let driver = actor.clone();
        let task = tokio::spawn(async move {
            let write = access.transact_write_with_finality("writer_probe.increment", |txn| {
                Box::pin(driver.callback(async move {
                    let value = writer_counter::read(txn).await?;
                    writer_counter::increment(txn, value, id.0, id.1).await
                }))
            });
            match driver.run(receipt, write).await {
                Ok(()) => Ok(Outcome::Acknowledged),
                Err(fault) if fault.unconfirmed => Ok(Outcome::Ambiguous),
                Err(fault) if is_classified_transaction_conflict(&fault.error) => {
                    Ok(Outcome::Conflict)
                }
                Err(fault) => Err(fault.error),
            }
        });
        Self {
            actor,
            task,
            read: false,
        }
    }

    async fn read(&mut self) {
        assert!(!self.read);
        self.actor.begun.release();
        self.actor
            .mutated
            .reached("buffered counter and receipt")
            .await;
        self.actor.mutated.release();
        self.read = true;
    }

    async fn commit(&mut self, receipt: CommitReceipt) {
        if !self.read {
            self.read().await;
        }
        self.actor.committing.reached("commit").await;
        if receipt == CommitReceipt::TimeoutBeforeCommit {
            expire_held_phase(EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT).await;
            self.actor.discarded.reached("unconfirmed discard").await;
            expire_held_phase(super::EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT).await;
        } else {
            self.actor.committing.release();
        }
    }

    async fn settle(self) -> Result<(Outcome, usize)> {
        let result = tokio::time::timeout(STALL_CEILING, self.task)
            .await
            .context("writer did not settle")???;
        Ok((result, self.actor.attempts()))
    }
}

#[derive(Default)]
struct Observation {
    outcomes: RapidHashMap<Call, (Outcome, usize)>,
    active_at_deadline: bool,
}

impl Observation {
    async fn check(
        &self,
        access: &ConfigAccess,
        trace: &Trace,
        receipt: CommitReceipt,
    ) -> Result<()> {
        let data = access.execute(writer_counter::OBSERVE).await?;
        let value = data["data"]["WriterCounter"][0]["value"]
            .as_u64()
            .context("counter missing")?;
        let rows = data["data"]["WriterReceipt"]
            .as_array()
            .context("receipts missing")?;
        let mut applied = RapidHashMap::<Call, usize>::default();
        for row in rows {
            let id = (
                row["writer"].as_i64().unwrap(),
                row["iteration"].as_i64().unwrap(),
            );
            assert!(
                self.outcomes.contains_key(&id),
                "unscheduled receipt {id:?}"
            );
            *applied.entry(id).or_default() += 1;
        }
        let accepted = self
            .outcomes
            .iter()
            .filter(|(_, (result, _))| *result == Outcome::Acknowledged);
        let ambiguous = self
            .outcomes
            .iter()
            .filter(|(_, (result, _))| *result == Outcome::Ambiguous);
        let attempts = trace.constants["Attempts"].as_u64().unwrap() as usize;
        assert!(self
            .outcomes
            .values()
            .all(|(_, tries)| (1..=attempts).contains(tries)));
        if receipt != CommitReceipt::Delivered {
            let (id, (_, tries)) = ambiguous
                .clone()
                .next()
                .context("expected ambiguous finality")?;
            assert_eq!(ambiguous.clone().count(), 1);
            assert_eq!(*tries, 1);
            assert_eq!(
                applied.contains_key(id),
                receipt == CommitReceipt::LostAfterCommit
            );
        }
        if trace.constants["StaleRetry"] == true || trace.constants["ConflictAsSuccess"] == true {
            assert!(
                self.outcomes.values().any(|(_, tries)| *tries > 1),
                "overlap must cause a real rebuild"
            );
        }
        let all_done = trace.constants["Writers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|actor| {
                let id = writer(actor);
                (1..=trace.constants["Iters"].as_i64().unwrap())
                    .all(|i| self.outcomes.contains_key(&(id, i)))
                    || ambiguous.clone().any(|((w, _), _)| *w == id)
            });
        let deadline = trace.constants["CheckDeadline"] == true;
        trace.check(
            if deadline { Red::Premise } else { Red::Defect },
            &[
                ("NoLostUpdate", value == rows.len() as u64),
                ("NoDoubleApply", applied.values().all(|n| *n == 1)),
                (
                    "AckedApplied",
                    accepted.clone().all(|(id, _)| applied.contains_key(id)),
                ),
                (
                    "ConflictNotApplied",
                    self.outcomes
                        .iter()
                        .filter(|(_, (o, _))| *o == Outcome::Conflict)
                        .all(|(id, _)| !applied.contains_key(id)),
                ),
                (
                    "TestAccounting",
                    !all_done
                        || ambiguous.count() > 0
                        || value == accepted.clone().count() as u64
                            && accepted.clone().count() == applied.len(),
                ),
                ("EveryCallSettles", all_done),
                ("RoundProgress", !all_done || accepted.count() > 0),
                ("WaveDeadline", !self.active_at_deadline),
            ],
        );
        Ok(())
    }
}

async fn native_round(store: &TestStore, trace: &Trace, observed: &mut Observation) -> Result<()> {
    let mut open = RapidHashMap::default();
    for step in &trace.steps {
        let actor = step.actor.as_ref().unwrap();
        match step.action.as_str() {
            "Begin" => {
                let guard = store.node.begin_transaction_guard(false).await?;
                assert!(open.insert(call(step, actor), guard).is_none());
            }
            "Read" => {
                let id = call(step, actor);
                let txn = &open[&id];
                let row = txn.execute("{ WriterCounter {value} }").await;
                anyhow::ensure!(!row.has_errors(), "read failed: {:?}", row.errors);
                let value = row.data.unwrap()["WriterCounter"][0]["value"]
                    .as_i64()
                    .unwrap();
                let request =
                    query::QueryRequest::new(writer_counter::INCREMENT).with_variables(json!({
                        "value": value + 1, "writer": id.0, "iteration": id.1,
                    }));
                let response = txn.execute_request(request).await;
                anyhow::ensure!(
                    !response.has_errors(),
                    "native mutation failed: {:?}",
                    response.errors
                );
            }
            "MutualAbort" => {
                for writer in actor.as_array().unwrap() {
                    let id = call(step, writer);
                    let outcome = match open.remove(&id).unwrap().commit().await {
                        Ok(()) => Outcome::Acknowledged,
                        Err(query::TransactionError::Conflict(_)) => Outcome::Conflict,
                        Err(error) => return Err(error.into()),
                    };
                    observed.outcomes.insert(id, (outcome, 1));
                }
            }
            action => panic!("unmapped native action {action}"),
        }
    }
    assert!(open.is_empty());
    Ok(())
}

async fn owned_wave(
    access: &ConfigAccess,
    trace: &Trace,
    observed: &mut Observation,
    receipt: CommitReceipt,
) -> Result<()> {
    let writers = trace.constants["Writers"].as_array().unwrap();
    let iterations = trace.constants["Iters"].as_i64().unwrap();
    let mut active = RapidHashMap::<Call, Running>::default();
    for step in &trace.steps {
        if step.action == "Tick" {
            if active.is_empty() {
                let id = (writer(&writers[0]), 1);
                let running = Running::launch(access.clone(), id, receipt);
                running
                    .actor
                    .begun
                    .reached("opened at logical clock start")
                    .await;
                active.insert(id, running);
            }
            if step.after["clock"].as_u64() > trace.constants["Deadline"].as_u64() {
                observed.active_at_deadline = active.values().any(|r| !r.task.is_finished());
            }
            continue;
        }
        if step.action == "Stutter" {
            continue;
        }
        let id = call(step, step.actor.as_ref().unwrap());
        if observed.outcomes.contains_key(&id) {
            continue;
        }
        match step.action.as_str() {
            "Begin" | "Park" => {
                let running = Running::launch(access.clone(), id, receipt);
                if step.action == "Park" {
                    running.actor.queued().await;
                    running.actor.begun.release();
                    running.actor.mutated.release();
                    running.actor.committing.release();
                } else {
                    running.actor.begun.reached("opened transaction").await;
                }
                assert!(active.insert(id, running).is_none());
            }
            "Read" => active.get_mut(&id).unwrap().read().await,
            "Commit" | "CommitTimeout" => {
                let mut running = active.remove(&id).unwrap();
                running.commit(receipt).await;
                observed.outcomes.insert(id, running.settle().await?);
            }
            "ReadLatest" => {
                let data = access.execute(writer_counter::OBSERVE).await?;
                assert_eq!(
                    data["data"]["WriterCounter"][0]["value"],
                    actor_value(&step.after["rv"], step.actor.as_ref().unwrap()),
                    "counter visible to the external reader before the transaction"
                );
            }
            "Retry" => {}
            action => panic!("unmapped store action {action}"),
        }
    }
    for (id, mut running) in active {
        if trace.constants["LostWakeup"] != true {
            running.commit(receipt).await;
        }
        observed.outcomes.insert(id, running.settle().await?);
    }
    for actor in writers {
        let w = writer(actor);
        for i in 1..=iterations {
            let id = (w, i);
            if observed.outcomes.contains_key(&id)
                || observed
                    .outcomes
                    .iter()
                    .any(|((prior, _), (result, _))| *prior == w && *result == Outcome::Ambiguous)
            {
                continue;
            }
            let mut running = Running::launch(access.clone(), id, CommitReceipt::Delivered);
            running.actor.begun.reached("remaining call opened").await;
            running.commit(CommitReceipt::Delivered).await;
            observed.outcomes.insert(id, running.settle().await?);
        }
    }
    Ok(())
}

async fn replay(trace: &Trace, receipt: CommitReceipt) -> Result<()> {
    let store = TestStore::start("shared-owner-trace", Default::default()).await;
    let access = ConfigAccess::local_as(store.node.clone(), store.node_identity.did().parse()?);
    access
        .add_collection(
            &gents_protocol::schemas::HomePolicyIds::node_store().render(writer_counter::SCHEMA),
        )
        .await?;
    writer_counter::seed(&access).await?;
    let mut observed = Observation::default();
    if trace.constants["Attempts"] == 1 {
        native_round(&store, trace, &mut observed).await?;
    } else {
        let gate = mutation_write_gate(&store.node);
        let capacity = trace.constants["Capacity"].as_u64().unwrap() as usize;
        assert!(capacity <= MUTATION_WRITE_GATE_CAPACITY);
        let mut permits = Vec::new();
        for _ in capacity..MUTATION_WRITE_GATE_CAPACITY {
            permits.push(
                gate.acquire(WriteOperation::new("writer_probe.occupied").unwrap())
                    .await?,
            );
        }
        assert_eq!(gate.permits.available_permits(), capacity);
        owned_wave(&access, trace, &mut observed, receipt).await?;
    }
    observed.check(&access, trace, receipt).await?;
    Ok(())
}

#[tokio::test]
async fn shared_owner_hot_counter_traces() -> Result<()> {
    for json in [
        include_str!("../../../../gents/proofs/tla/generated/traces/MCSharedOwnerHotCounterStaleRetry.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCSharedOwnerHotCounterReadOutsideTxn.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCSharedOwnerHotCounterConflictAsSuccess.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCSharedOwnerHotCounterRetryAmbiguous.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCSharedOwnerHotCounterLostWakeup.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCSharedOwnerHotCounterAbortOnOpenWriter.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCSharedOwnerHotCounterWaveDeadline.json"),
    ] {
        let trace = Trace::load(json, "SharedOwnerHotCounter", &[("Attempts", TRANSACT_CONFLICT_MAX_RETRIES + 1)]);
        if trace.constants["CanTimeout"] == true {
            replay(&trace, CommitReceipt::LostAfterCommit).await.with_context(|| format!("{} landed", trace.config))?;
            replay(&trace, CommitReceipt::TimeoutBeforeCommit).await.with_context(|| format!("{} unlanded", trace.config))?;
        } else {
            replay(&trace, CommitReceipt::Delivered).await.with_context(|| trace.config.clone())?;
        }
    }
    let native = Trace::load(include_str!("../../../../gents/proofs/tla/generated/traces/MCSharedOwnerHotCounterNativeAbortOnOpenWriter.json"), "SharedOwnerHotCounter", &[("Attempts", 1)]);
    replay(&native, CommitReceipt::Delivered).await
}
