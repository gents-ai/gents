use super::*;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};

const CHILD_PHASE: &str = "GENTS_DELIVERY_CHILD_PHASE";
const CHILD_ARTIFACTS: &str = "GENTS_DELIVERY_CHILD_ARTIFACTS";
const CHILD_READY: &str = "GENTS_DELIVERY_CHILD_READY";

struct ChildGuard {
    child: Child,
    ready: TcpListener,
}

async fn spawn_child(artifacts: &Path, phase: &str) -> Result<ChildGuard> {
    let path = artifacts.join(format!("{phase}.log"));
    let output = tokio::task::spawn_blocking(move || std::fs::File::create(path)).await??;
    let ready = TcpListener::bind(("127.0.0.1", 0)).await?;
    let child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "trigger_delivery_live::process::delivery_crash_child",
            "--ignored",
            "--nocapture",
        ])
        .env(CHILD_PHASE, phase)
        .env(CHILD_ARTIFACTS, artifacts)
        .env(CHILD_READY, ready.local_addr()?.to_string())
        .stdout(Stdio::from(output.try_clone()?))
        .stderr(Stdio::from(output))
        .kill_on_drop(true)
        .spawn()?;
    Ok(ChildGuard { child, ready })
}

async fn wait_ready(child: &mut ChildGuard, artifacts: &Path, phase: &str) -> Result<()> {
    let mut stream = tokio::time::timeout(Duration::from_secs(1800), async {
        tokio::select! {
            accepted = child.ready.accept() => Ok::<_, anyhow::Error>(accepted?.0),
            status = child.child.wait() => {
                let log = tokio::fs::read_to_string(artifacts.join(format!("{phase}.log"))).await?;
                anyhow::bail!("child {phase} exited before readiness ({}): {log}", status?);
            }
        }
    })
    .await
    .context("child readiness deadline")??;
    let mut marker = [0];
    tokio::time::timeout(Duration::from_secs(30), stream.read_exact(&mut marker))
        .await
        .context("child readiness marker deadline")??;
    ensure!(marker == [b'r'], "child sent an invalid readiness marker");
    Ok(())
}

async fn ready(artifacts: &Path, phase: &str) -> Result<()> {
    tokio::fs::write(
        artifacts.join(format!("{phase}.ready")),
        std::process::id().to_string(),
    )
    .await?;
    let address = std::env::var(CHILD_READY).context("child readiness address is required")?;
    let mut stream = TcpStream::connect(address).await?;
    stream.write_all(b"r").await?;
    Ok(())
}

async fn configure_worker(access: &ConfigAccess, owner: &str, enabled: bool) -> Result<()> {
    apply(access, owner, vec![(Collection::Trigger, json!({
        "trigger_id":"delivery-worker-0", "task_id":"delivery-worker-0", "enabled":enabled,
        "source":{"kind":"event", "event_source_id":"delivery-worker-0"}, "concurrency":"queued_serial"
    }))]).await
}

async fn assignment(access: &ConfigAccess, label: &str) -> Result<()> {
    create_source(
        access,
        "DeliveryWork",
        WORK_SDL,
        json!({"handoff_id": label, "lane": 0, "shard_id": label, "attempt": 1, "tags": ["test"]}),
    )
    .await?;
    Ok(())
}

async fn evidence(access: &ConfigAccess, owner: &str) -> Result<Value> {
    Ok(json!({
        "owner_did":owner,
        "requests":rows(access, "AgentRequest", "_docID request_id session_id agent_did lifecycle_state content failure_reason", owner).await?,
        "fires":rows(access, "TriggerFire", "fire_key owner_did trigger_id source_collection source_doc_id request_id session_id queued_serial emit_outcome", owner).await?,
        "outcomes":rows(access, "FireOutcome", "handoff_id fire_key owner_did trigger_id source_collection source_doc_id request_id session_id source_handoff_id terminal_state", owner).await?,
    }))
}

async fn wait_deliveries(access: &ConfigAccess, owner: &str, labels: &[&str]) -> Result<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1800);
    let mut changes = DeliveryChanges::new(access).await;
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
        changes
            .recv(deadline)
            .await
            .with_context(|| format!("process recovery did not converge: {state}"))?;
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
        let key = string(fire, "fire_key")?;
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
            output["request_id"] == fire["request_id"]
                && output["source_doc_id"] == fire["source_doc_id"]
                && output["terminal_state"] == "completed",
            "terminal/outcome identity drift: {output}"
        );
    }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "uses both local targets; set GENTS_LIVE_TARGET and GENTS_DELIVERY_SOURCE_SHA"]
async fn real_process_crashes_recover_disabled_and_offline_arrivals_exactly_once() -> Result<()> {
    let artifacts = tokio::task::spawn_blocking(|| artifact_directory("process")).await??;
    let mut killed = Vec::new();
    for phase in ["seed", "recover"] {
        let mut child = spawn_child(&artifacts, phase).await?;
        wait_ready(&mut child, &artifacts, phase).await?;
        killed.push(child.child.id().context("crash child has a PID")?);
        child.child.kill().await?;
        let status = child.child.wait().await?;
        ensure!(
            !status.success(),
            "crash worker unexpectedly exited successfully"
        );
    }
    for phase in ["offline", "audit"] {
        let mut child = spawn_child(&artifacts, phase).await?;
        wait_ready(&mut child, &artifacts, phase).await?;
        let status = tokio::time::timeout(Duration::from_secs(120), child.child.wait())
            .await
            .with_context(|| format!("restart child {phase} did not exit"))??;
        ensure!(
            status.success(),
            "restart child {phase} failed: {}",
            tokio::fs::read_to_string(artifacts.join(format!("{phase}.log"))).await?
        );
    }
    let final_state: Value =
        serde_json::from_slice(&tokio::fs::read(artifacts.join("audit.evidence.json")).await?)?;
    ensure!(
        final_state["outcomes"]
            .as_array()
            .context("final outcomes")?
            .len()
            == 4,
        "final recovery outcome count"
    );
    tokio::fs::write(
        artifacts.join("process.evidence.json"),
        serde_json::to_vec_pretty(&json!({
            "killed_owned_pids":killed, "crash_boundaries":["disabled_unadmitted", "terminal_committed"],
            "subprocess_starts":4, "final":final_state,
        }))?,
    ).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "internal subprocess entry; launched by the process qualification"]
async fn delivery_crash_child() -> Result<()> {
    let phase = std::env::var(CHILD_PHASE)
        .context("delivery_crash_child is launched by the parent process test")?;
    ensure!(
        matches!(phase.as_str(), "seed" | "recover" | "offline" | "audit"),
        "unknown child phase {phase}"
    );
    let artifacts =
        PathBuf::from(std::env::var_os(CHILD_ARTIFACTS).context("child artifact path")?);
    let home_path = artifacts.join("home");
    let home = if phase == "seed" {
        gents::runtime::embedded::EmbeddedHome::create_retained(&home_path).await?
    } else {
        gents::runtime::embedded::EmbeddedHome::open_retained(&home_path).await?
    };
    let db = support::test_db_from_home(home);
    let owner = db.node_identity.did().to_owned();
    let access = if phase == "seed" {
        let lanes = delivery_lanes().await?;
        let (fixture, _) = configure(&db, &lanes).await?;
        configure_worker(&fixture.access, &owner, false).await?;
        apply(&fixture.access, &owner, vec![(Collection::Trigger, json!({
            "trigger_id":"delivery-inbox", "task_id":"delivery-inbox", "enabled":false,
            "source":{"kind":"event", "event_source_id":"delivery-inbox"}, "concurrency":"parallel",
            "session_id_template":"{{ doc.reply_session_id }}"
        }))]).await?;
        fixture.access
    } else {
        user_access(&db).await
    };
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
        tokio::fs::write(
            artifacts.join("seed.evidence.json"),
            serde_json::to_vec_pretty(&state)?,
        )
        .await?;
        ready(&artifacts, &phase).await?;
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
        tokio::fs::write(
            artifacts.join("recover.evidence.json"),
            serde_json::to_vec_pretty(&state)?,
        )
        .await?;
        ready(&artifacts, &phase).await?;
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
        let prior: Value = serde_json::from_slice(
            &tokio::fs::read(artifacts.join("recover.evidence.json")).await?,
        )?;
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
            let answer = terminal_assistant_answer(&access, string(request, "request_id")?).await;
            ensure!(
                !answer.trim().is_empty(),
                "recovered request lacks real provider answer: {request}"
            );
        }
        tokio::fs::write(
            artifacts.join(format!("{phase}.evidence.json")),
            serde_json::to_vec_pretty(&state)?,
        )
        .await?;
        runtime.shutdown().await;
        db.node.shutdown().await?;
        ready(&artifacts, &phase).await?;
    }
    Ok(())
}
