use super::*;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const CHILD_PHASE: &str = "GENTS_DELIVERY_CHILD_PHASE";
const CHILD_ARTIFACTS: &str = "GENTS_DELIVERY_CHILD_ARTIFACTS";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn spawn_child(artifacts: &Path, phase: &str) -> Result<ChildGuard> {
    let output = std::fs::File::create(artifacts.join(format!("{phase}.log")))?;
    let child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "process::delivery_crash_child",
            "--ignored",
            "--nocapture",
        ])
        .env(CHILD_PHASE, phase)
        .env(CHILD_ARTIFACTS, artifacts)
        .stdout(Stdio::from(output.try_clone()?))
        .stderr(Stdio::from(output))
        .spawn()?;
    Ok(ChildGuard(child))
}

async fn wait_ready(child: &mut ChildGuard, artifacts: &Path, phase: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1800);
    let ready = artifacts.join(format!("{phase}.ready"));
    loop {
        if ready.exists() {
            return Ok(());
        }
        if let Some(status) = child.0.try_wait()? {
            anyhow::bail!(
                "child {phase} exited before readiness ({status}): {}",
                std::fs::read_to_string(artifacts.join(format!("{phase}.log")))?
            );
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "{phase} child readiness timed out; artifacts={}",
            artifacts.display()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn ready(artifacts: &Path, phase: &str) -> Result<()> {
    let temporary = artifacts.join(format!("{phase}.ready.tmp"));
    std::fs::write(&temporary, std::process::id().to_string())?;
    std::fs::rename(temporary, artifacts.join(format!("{phase}.ready")))?;
    Ok(())
}

async fn configure_worker(access: &ConfigAccess, owner: &str, enabled: bool) -> Result<()> {
    apply(access, owner, vec![(Collection::Trigger, json!({
        "trigger_id":"delivery-worker-0", "task_id":"delivery-worker-0", "enabled":enabled,
        "source":{"kind":"event", "event_source_id":"delivery-worker-0"}, "concurrency":"queued_serial"
    }))]).await
}

async fn assignment(access: &ConfigAccess, label: &str) -> Result<()> {
    access.write("test.delivery_process.assignment", &format!(
        "mutation {{ create_DeliveryWork(input: {{ handoff_id: \"{}\", lane: 0, shard_id: \"{}\", attempt: 1, tags: [\"test\"] }}) {{ _docID }} }}",
        escape_graphql_string(label), escape_graphql_string(label),
    )).await?;
    Ok(())
}

async fn evidence(access: &ConfigAccess, owner: &str) -> Result<Value> {
    Ok(json!({
        "owner_did":owner,
        "requests":rows(access, "AgentRequest", "_docID request_id session_id agent_id lifecycle_state content failure_reason", owner).await?,
        "fires":rows(access, "TriggerFire", "fire_key owner_did trigger_id source_collection source_doc_id request_id session_id queued_serial emit_outcome", owner).await?,
        "outcomes":rows(access, "FireOutcome", "handoff_id fire_key owner_did trigger_id source_collection source_doc_id request_id session_id source_handoff_id terminal_state", owner).await?,
    }))
}

async fn wait_deliveries(access: &ConfigAccess, owner: &str, labels: &[&str]) -> Result<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1800);
    let state = loop {
        let state = evidence(access, owner).await?;
        ensure!(
            state["requests"].as_array().unwrap().iter().all(|row| {
                !gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal_str(
                    row["lifecycle_state"].as_str(),
                ) || row["lifecycle_state"] == "completed"
            }),
            "process recovery request failed: {state}"
        );
        for collection in ["requests", "fires", "outcomes"] {
            ensure!(
                state[collection].as_array().context("evidence rows")?.len() <= labels.len(),
                "duplicate or chained delivery: {state}"
            );
        }
        if ["requests", "fires", "outcomes"]
            .iter()
            .all(|key| state[key].as_array().unwrap().len() == labels.len())
            && state["requests"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["lifecycle_state"] == "completed")
        {
            break state;
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "process recovery did not converge: {state}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let requests = state["requests"].as_array().unwrap();
    let fires = state["fires"].as_array().unwrap();
    let outcomes = state["outcomes"].as_array().unwrap();
    for (collection, key) in [
        ("requests", "request_id"),
        ("fires", "fire_key"),
        ("outcomes", "handoff_id"),
    ] {
        let records = state[collection].as_array().unwrap();
        let keys = records
            .iter()
            .map(|row| string(row, key))
            .collect::<Result<BTreeSet<_>>>()?;
        ensure!(keys.len() == labels.len(), "duplicate {collection} key");
    }
    let emitted = outcomes
        .iter()
        .map(|row| string(row, "source_handoff_id"))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        emitted == labels.iter().copied().collect(),
        "lost source handoffs: {state}"
    );
    for fire in fires {
        let identity = gents_protocol::trigger_delivery::FireIdentity {
            owner_did: owner.into(),
            trigger_id: string(fire, "trigger_id")?.into(),
            source_collection: string(fire, "source_collection")?.into(),
            source_doc_id: string(fire, "source_doc_id")?.into(),
        };
        let key = identity.fire_key();
        ensure!(
            fire["fire_key"] == key && fire["request_id"] == identity.request_id(),
            "noncanonical admitted identity: {fire}"
        );
        ensure!(
            fire["queued_serial"] == true && fire["emit_outcome"] == true,
            "wrong durable fire policy: {fire}"
        );
        ensure!(
            requests
                .iter()
                .filter(|row| row["request_id"] == fire["request_id"])
                .count()
                == 1,
            "receipt/request atomicity: {fire}"
        );
        let output = outcomes
            .iter()
            .find(|row| row["fire_key"] == key)
            .context("missing outcome")?;
        ensure!(
            output["handoff_id"] == identity.outcome_id()
                && output["request_id"] == fire["request_id"]
                && output["source_doc_id"] == fire["source_doc_id"]
                && output["terminal_state"] == "completed",
            "terminal/outcome identity drift: {output}"
        );
    }
    let arrivals = access.execute("{ _documentArrivals(collection: \"AgentRequest\", after: \"0\", limit: 128) { entries { cursor docID } } }").await?;
    let order = arrivals["data"]["_documentArrivals"]["entries"]
        .as_array()
        .context("request arrival journal")?
        .iter()
        .map(|entry| {
            let request = requests
                .iter()
                .find(|row| row["_docID"] == entry["docID"])
                .context("unknown request arrival")?;
            let outcome = outcomes
                .iter()
                .find(|row| row["request_id"] == request["request_id"])
                .context("request outcome missing")?;
            string(outcome, "source_handoff_id")
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        order == labels,
        "queued serial receiving-node order changed: {order:?}"
    );
    Ok(state)
}

fn retained_keys(state: &Value) -> Result<BTreeMap<String, String>> {
    state["outcomes"]
        .as_array()
        .context("outcomes")?
        .iter()
        .map(|row| {
            Ok((
                string(row, "source_handoff_id")?.into(),
                string(row, "fire_key")?.into(),
            ))
        })
        .collect()
}

/// The parent kills only Child handles it just spawned. Stores and evidence
/// remain in a retained temporary directory and source documents carry test tags.
/// Crashes here occur before admission (disabled cursor) and after confirmed
/// terminal commit. In-transaction fault boundaries belong to L3 owner tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real inference/process crash: GENTS_TRIGGER_DELIVERY_LIVE=1 and both delivery endpoints"]
async fn real_process_crashes_recover_disabled_and_offline_arrivals_exactly_once() -> Result<()> {
    ensure!(
        std::env::var("GENTS_TRIGGER_DELIVERY_LIVE").as_deref() == Ok("1"),
        "explicit live opt-in required"
    );
    let artifacts = artifact_directory("process")?;
    let mut killed = Vec::new();
    for phase in ["seed", "recover"] {
        let mut child = spawn_child(&artifacts, phase)?;
        wait_ready(&mut child, &artifacts, phase).await?;
        killed.push(child.0.id());
        child.0.kill()?;
        let status = child.0.wait()?;
        ensure!(
            !status.success(),
            "crash worker unexpectedly exited successfully"
        );
    }
    for phase in ["offline", "audit"] {
        let mut child = spawn_child(&artifacts, phase)?;
        wait_ready(&mut child, &artifacts, phase).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        loop {
            if let Some(status) = child.0.try_wait()? {
                ensure!(
                    status.success(),
                    "restart child {phase} failed: {}",
                    std::fs::read_to_string(artifacts.join(format!("{phase}.log")))?
                );
                break;
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "restart child {phase} did not exit"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let final_state: Value =
        serde_json::from_slice(&std::fs::read(artifacts.join("audit.evidence.json"))?)?;
    ensure!(
        final_state["outcomes"]
            .as_array()
            .context("final outcomes")?
            .len()
            == 4,
        "final recovery outcome count"
    );
    std::fs::write(
        artifacts.join("process.evidence.json"),
        serde_json::to_vec_pretty(&json!({
            "killed_owned_pids":killed, "crash_boundaries":["disabled_unadmitted", "terminal_committed"],
            "subprocess_starts":4, "final":final_state,
        }))?,
    )?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "internal subprocess entry; launched by the process qualification"]
async fn delivery_crash_child() -> Result<()> {
    let Ok(phase) = std::env::var(CHILD_PHASE) else {
        return Ok(());
    };
    ensure!(
        std::env::var("GENTS_TRIGGER_DELIVERY_LIVE").as_deref() == Ok("1"),
        "explicit live opt-in required"
    );
    let artifacts =
        PathBuf::from(std::env::var_os(CHILD_ARTIFACTS).context("child artifact path")?);
    let home_path = artifacts.join("home");
    let home = if phase == "seed" {
        gents::eval::runner::embedded::EmbeddedHome::create_retained(&home_path).await?
    } else {
        gents::eval::runner::embedded::EmbeddedHome::open_retained(&home_path).await?
    };
    let db = support::test_db_from_home(home);
    let owner = db.node_identity.did().to_owned();
    let access = ConfigAccess::Local(db.node.clone());
    if phase == "seed" {
        let endpoints = [
            std::env::var("GENTS_DELIVERY_ENDPOINT_1")?,
            std::env::var("GENTS_DELIVERY_ENDPOINT_2")?,
        ];
        access.add_schema(SCHEMA).await?;
        configure(&access, &db.node, &owner, &endpoints).await?;
        configure_worker(&access, &owner, false).await?;
        apply(&access, &owner, vec![(Collection::Trigger, json!({
            "trigger_id":"delivery-inbox", "task_id":"delivery-inbox", "enabled":false,
            "source":{"kind":"event", "event_source_id":"delivery-inbox"}, "concurrency":"parallel",
            "session_id_template":"{{ doc.reply_session_id }}"
        }))]).await?;
    }
    if phase == "offline" {
        let prior = evidence(&access, &owner).await?;
        ensure!(
            prior["outcomes"].as_array().unwrap().len() == 3,
            "terminal outcomes lost after SIGKILL: {prior}"
        );
        assignment(&access, "offline-3").await?;
    }
    let runtime = boot_live_agent(&db, db.node_identity.clone()).await?;
    if phase == "seed" {
        assignment(&access, "disabled-0").await?;
        assignment(&access, "disabled-1").await?;
        let state = evidence(&access, &owner).await?;
        ensure!(
            state["fires"].as_array().unwrap().is_empty(),
            "disabled trigger admitted work"
        );
        std::fs::write(
            artifacts.join("seed.evidence.json"),
            serde_json::to_vec_pretty(&state)?,
        )?;
        ready(&artifacts, &phase)?;
        std::future::pending::<()>().await;
    } else if phase == "recover" {
        let state = evidence(&access, &owner).await?;
        ensure!(
            state["fires"].as_array().unwrap().is_empty(),
            "restart fired disabled work"
        );
        configure_worker(&access, &owner, true).await?;
        wait_deliveries(&access, &owner, &["disabled-0", "disabled-1"]).await?;
        assignment(&access, "live-2").await?;
        let state =
            wait_deliveries(&access, &owner, &["disabled-0", "disabled-1", "live-2"]).await?;
        std::fs::write(
            artifacts.join("recover.evidence.json"),
            serde_json::to_vec_pretty(&state)?,
        )?;
        ready(&artifacts, &phase)?;
        std::future::pending::<()>().await;
    } else {
        ensure!(
            matches!(phase.as_str(), "offline" | "audit"),
            "unknown child phase"
        );
        let state = wait_deliveries(
            &access,
            &owner,
            &["disabled-0", "disabled-1", "live-2", "offline-3"],
        )
        .await?;
        let prior: Value =
            serde_json::from_slice(&std::fs::read(artifacts.join("recover.evidence.json"))?)?;
        ensure!(
            state["owner_did"] == prior["owner_did"],
            "restart changed node identity"
        );
        let current = retained_keys(&state)?;
        for (handoff, key) in retained_keys(&prior)? {
            ensure!(
                current.get(&handoff) == Some(&key),
                "restart changed durable fire identity"
            );
        }
        for request in state["requests"].as_array().unwrap() {
            let answer = terminal_assistant_answer(&db.node, string(request, "request_id")?).await;
            ensure!(
                answer.contains("WORK"),
                "recovered request lacks real provider answer: {request}"
            );
        }
        std::fs::write(
            artifacts.join(format!("{phase}.evidence.json")),
            serde_json::to_vec_pretty(&state)?,
        )?;
        runtime.shutdown().await;
        db.node.shutdown().await;
        ready(&artifacts, &phase)?;
    }
    Ok(())
}
