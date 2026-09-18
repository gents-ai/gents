use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use gents::{Collection, ConfigAccess};
use serde_json::Value;

use super::{reporting, stages};

pub(super) struct Host {
    id: String,
    pub access: ConfigAccess,
    evidence: PathBuf,
}

async fn control(args: &[&str]) -> Result<Value> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/evals/host-control.mjs");
    let output = tokio::process::Command::new("node")
        .arg(script)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .context("launching isolated host controller")?;
    ensure!(
        output.status.success(),
        "host controller failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).context("invalid host controller receipt")
}

impl Host {
    /// Wait for the hosted runtime to acknowledge the exact configuration it
    /// currently resolves.  The runtime owns both resolution and activation;
    /// this controller only consumes its read-only fence.
    pub async fn wait_for_activation(&self) -> Result<()> {
        let ConfigAccess::Graphql(graphql) = &self.access else {
            anyhow::bail!("host activation fence requires GraphQL access");
        };
        let endpoint = graphql
            .strip_suffix("/api/v0/graphql")
            .context("host GraphQL endpoint has unexpected path")?;
        let response = reqwest::Client::new()
            .get(format!("{endpoint}/activation"))
            .timeout(std::time::Duration::from_secs(35))
            .send()
            .await
            .context("waiting for hosted runtime activation")?;
        ensure!(
            response.status().is_success(),
            "hosted runtime did not activate configuration: status={} body={}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
        Ok(())
    }

    pub async fn restore(&self) -> Result<()> {
        let receipt = control(&["restore", &self.id]).await?;
        reporting::write_json_new(&self.evidence.join("host-restoration.json"), &receipt)
    }

    pub async fn dismiss(&self, doc_id: &str) -> Result<()> {
        control(&["dismiss", &self.id, doc_id]).await?;
        Ok(())
    }

    pub async fn observation(&self, stage: &str) -> Result<Value> {
        let receipt: Value = serde_json::from_slice(&std::fs::read(
            self.evidence.join(format!("{stage}-input-receipt.json")),
        )?)?;
        let correlation = receipt["correlation"]
            .as_str()
            .context("check correlation missing")?;
        self.observation_for_correlation(stage, correlation).await
    }

    pub async fn observation_for_correlation(
        &self,
        stage: &str,
        correlation: &str,
    ) -> Result<Value> {
        let escaped = gents::graphql::escape_graphql_string(correlation);
        let response = self.access.execute(&format!("{{ HostObservation(filter: {{correlation: {{_eq: \"{escaped}\"}}}}) {{ correlation disk_used_percent backup_mtime backup_matches api_status dashboard_status }} }}")).await?;
        reporting::write_json_new(
            &self.evidence.join(format!("{stage}-observation.json")),
            &response,
        )?;
        let rows = response["data"]["HostObservation"]
            .as_array()
            .context("observation rows missing")?;
        ensure!(
            rows.len() == 1,
            "expected exactly one observation for this cycle, got {}",
            rows.len()
        );
        Ok(rows[0].clone())
    }

    pub async fn scheduled_check(
        &self,
        trigger: &str,
        behavior: &str,
        stage: &str,
    ) -> Result<stages::StageResult> {
        let escaped = gents::graphql::escape_graphql_string(trigger);
        let query = format!("{{ AgentRequest(filter: {{ caused_by_trigger_id: {{_eq: \"{escaped}\"}} }}) {{request_id behavior_id}} }}");
        let prior = self.access.execute(&query).await?;
        let prior_ids: std::collections::BTreeSet<_> = prior["data"]["AgentRequest"]
            .as_array()
            .context("scheduled request rows missing")?
            .iter()
            .filter_map(|row| row["request_id"].as_str())
            .map(str::to_owned)
            .collect();
        let started = std::time::Instant::now();
        let now = chrono::Utc::now().to_rfc3339();
        let due = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
        let due = gents::graphql::escape_graphql_string(&due);
        let receipt = self.access.write("eval.host.schedule", &format!("mutation {{ update_Trigger(filter: {{trigger_id: {{_eq: \"{escaped}\"}}}}, input: {{next_run_at: \"{due}\"}}) {{_docID}} }}")).await?;
        reporting::write_json_new(
            &self.evidence.join(format!("{stage}-schedule-receipt.json")),
            &receipt,
        )?;
        let request = loop {
            let response = self.access.execute(&query).await?;
            let rows = response["data"]["AgentRequest"]
                .as_array()
                .context("scheduled request rows missing")?
                .iter()
                .filter(|row| !prior_ids.contains(row["request_id"].as_str().unwrap_or_default()))
                .collect::<Vec<_>>();
            ensure!(
                rows.len() <= 1,
                "schedule created multiple requests for this checkpoint"
            );
            if let Some(row) = rows.first() {
                ensure!(
                    row["behavior_id"] == behavior,
                    "schedule selected wrong behavior"
                );
                break row["request_id"]
                    .as_str()
                    .context("scheduled request ID missing")?
                    .to_owned();
            }
            if started.elapsed() > std::time::Duration::from_secs(60) {
                return Err(stages::EvaluationFailure::Runtime(
                    "due schedule produced no request".into(),
                )
                .into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        stages::observe_request(
            (&self.access).into(),
            request,
            stage,
            &self.evidence,
            started,
            now,
        )
        .await
    }
    pub async fn trigger_check(
        &self,
        collection: &str,
        behavior: &str,
        stage: &str,
    ) -> Result<stages::StageResult> {
        gents::graphql::validate_collection_identifier(collection)?;
        ensure!(
            self.access
                .collection_fields(collection)
                .await?
                .is_some_and(|fields| fields.contains("correlation")),
            "monitor input lacks correlation field"
        );
        let started = std::time::Instant::now();
        let now = chrono::Utc::now().to_rfc3339();
        let correlation = uuid::Uuid::new_v4().to_string();
        let escaped_correlation = gents::graphql::escape_graphql_string(&correlation);
        let mutation_name = format!("add_{collection}");
        let receipt = self.access.write("eval.host.check", &format!(
            "mutation {{ {mutation_name}(input: {{ correlation: \"{escaped_correlation}\" }}) {{ _docID }} }}"
        )).await?;
        reporting::write_json_new(
            &self.evidence.join(format!("{stage}-input-receipt.json")),
            &serde_json::json!({"correlation": correlation,"receipt":receipt}),
        )?;
        let source = input_document_id(&receipt, collection).map_err(stages::grader)?;
        let escaped = gents::graphql::escape_graphql_string(source);
        let query = format!("{{ AgentRequest(filter: {{ caused_by_source_doc_id: {{ _eq: \"{escaped}\" }} }} ) {{ request_id behavior_id }} }}");
        let request = loop {
            let response = self.access.execute(&query).await?;
            let rows = response["data"]["AgentRequest"]
                .as_array()
                .context("trigger request rows missing")?;
            ensure!(
                rows.len() <= 1,
                "one input created duplicate monitoring requests"
            );
            if let Some(row) = rows.first() {
                ensure!(
                    row["behavior_id"] == behavior,
                    "trigger selected the wrong behavior"
                );
                break row["request_id"]
                    .as_str()
                    .context("trigger request ID missing")?
                    .to_owned();
            }
            if started.elapsed() > std::time::Duration::from_secs(60) {
                return Err(stages::EvaluationFailure::Runtime(
                    "input document did not produce a monitoring request".into(),
                )
                .into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        stages::observe_request(
            (&self.access).into(),
            request,
            stage,
            &self.evidence,
            started,
            now,
        )
        .await
    }

    pub async fn request(
        &self,
        behavior: &str,
        stage: &str,
        prompt: &str,
    ) -> Result<stages::StageResult> {
        self.request_in_session(behavior, stage, prompt, None).await
    }

    pub async fn request_in_session(
        &self,
        behavior: &str,
        stage: &str,
        prompt: &str,
        session: Option<&str>,
    ) -> Result<stages::StageResult> {
        let identity = self
            .access
            .execute("{ AgentPrincipal { agent_did } }")
            .await?;
        let owner = identity["data"]["AgentPrincipal"][0]["agent_did"]
            .as_str()
            .context("host owner missing")?;
        let session_id = session
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let started = std::time::Instant::now();
        let now = chrono::Utc::now().to_rfc3339();
        reporting::write_json_new(
            &self.evidence.join(format!("{stage}-input.json")),
            &serde_json::json!({"session_id": session_id, "behavior_id": behavior, "prompt": prompt, "owner": owner}),
        )?;
        let timeout = (stages::stage_timeout()?.as_secs() + 30).to_string();
        let session = gents::graphql::escape_graphql_string(&session_id);
        let query = format!("{{ AgentRequest(filter: {{ session_id: {{ _eq: \"{session}\" }} }} ) {{ request_id }} }}");
        let prior = self.access.execute(&query).await?;
        let prior_ids: std::collections::BTreeSet<_> = prior["data"]["AgentRequest"]
            .as_array()
            .context("prior session requests missing")?
            .iter()
            .filter_map(|row| row["request_id"].as_str())
            .map(str::to_owned)
            .collect();
        control(&["submit", &self.id, behavior, &session_id, prompt, &timeout]).await?;
        let request_id = loop {
            let response = self.access.execute(&query).await?;
            let rows = response["data"]["AgentRequest"]
                .as_array()
                .context("submitted request rows missing")?
                .iter()
                .filter(|row| !prior_ids.contains(row["request_id"].as_str().unwrap_or_default()))
                .collect::<Vec<_>>();
            ensure!(
                rows.len() <= 1,
                "one CLI submission created multiple requests"
            );
            if let Some(row) = rows.first() {
                break row["request_id"]
                    .as_str()
                    .context("request ID missing")?
                    .to_owned();
            }
            ensure!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "CLI did not materialize request; inspect retained chat stderr"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        stages::observe_request(
            (&self.access).into(),
            request_id,
            stage,
            &self.evidence,
            started,
            now,
        )
        .await
    }

    pub async fn configure_trial(&self) -> Result<()> {
        use gents::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        use gents::document_config::{InferenceProfile, InferenceSampling};
        let snapshot = configuration_snapshot(&self.access).await?;
        let owner = snapshot["AgentPrincipal"][0]["agent_did"]
            .as_str()
            .context("host owner missing")?;
        let sampling = InferenceSampling {
            agent_did: owner.into(),
            sampling_id: "engineer-eval-sampling".into(),
            temperature: Some(1.0),
            top_p: Some(0.95),
            ..Default::default()
        };
        let mut documents = vec![(
            Collection::InferenceSampling,
            serde_json::to_value(sampling)?,
        )];
        for row in snapshot["InferenceProfile"]
            .as_array()
            .context("profiles missing")?
        {
            let mut document = row.clone();
            document
                .as_object_mut()
                .context("profile is not an object")?
                .remove("_docID");
            let (_, projected) = gents::config_client::config_projection(
                Collection::InferenceProfile,
                Some(&document),
            )?;
            let mut profile: InferenceProfile =
                serde_json::from_value(projected.context("profile projection missing")?)?;
            profile.sampling_id = Some("engineer-eval-sampling".into());
            profile.reasoning_effort = super::eval_reasoning_effort()?;
            documents.push((Collection::InferenceProfile, serde_json::to_value(profile)?));
        }
        let plan = DesiredStateApplyPlan::new(
            documents
                .into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
        )?;
        self.access
            .transact("eval.host.configuration", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    gents::config_client::apply_desired_state_plan(txn, plan)
                        .await
                        .map(|_| ())
                })
            })
            .await
    }

    pub async fn start(evidence: &Path) -> Result<Self> {
        let receipt = control(&["start"]).await?;
        Self::from_start_receipt(evidence, receipt).await
    }

    pub async fn fork(&self, evidence: &Path) -> Result<Self> {
        std::fs::create_dir_all(evidence)?;
        let snapshot = evidence.join("original-home");
        let receipt = control(&[
            "fork",
            &self.id,
            snapshot
                .to_str()
                .context("non-UTF8 candidate snapshot path")?,
        ])
        .await?;
        Self::from_start_receipt(evidence, receipt).await
    }

    async fn from_start_receipt(evidence: &Path, receipt: Value) -> Result<Self> {
        let id = receipt["container_id"]
            .as_str()
            .context("container ID missing")?
            .to_owned();
        let endpoint = receipt["graphql"]
            .as_str()
            .context("runtime endpoint missing")?
            .to_owned();
        if let Err(error) = reporting::write_json_new(&evidence.join("host-start.json"), &receipt) {
            control(&["close", &id]).await?;
            return Err(error);
        }
        Ok(Self {
            id,
            access: ConfigAccess::Graphql(endpoint),
            evidence: evidence.into(),
        })
    }

    pub async fn snapshot(&self, stage: &str) -> Result<Value> {
        let snapshot = control(&["snapshot", &self.id]).await?;
        reporting::write_json_new(&self.evidence.join(format!("{stage}-host.json")), &snapshot)?;
        Ok(snapshot)
    }

    pub async fn fault(&self, fault: &str, stage: &str) -> Result<()> {
        let snapshot = control(&["fault", &self.id, fault]).await?;
        reporting::write_json_new(
            &self.evidence.join(format!("{stage}-injection.json")),
            &snapshot,
        )
    }

    pub async fn restart(&mut self, stage: &str) -> Result<()> {
        self.resume_or_restart("restart", stage).await
    }

    pub async fn resume(&mut self, stage: &str) -> Result<()> {
        self.resume_or_restart("resume", stage).await
    }

    async fn resume_or_restart(&mut self, operation: &str, stage: &str) -> Result<()> {
        let receipt = control(&[operation, &self.id]).await?;
        let endpoint = receipt["graphql"]
            .as_str()
            .context("restart endpoint missing")?;
        self.access = ConfigAccess::Graphql(endpoint.into());
        reporting::write_json_new(
            &self.evidence.join(format!("{stage}-restart.json")),
            &receipt,
        )
    }

    pub async fn close(self) -> Result<()> {
        let directory = self.evidence.join("runtime");
        let archive = control(&[
            "archive",
            &self.id,
            directory.to_str().context("non-UTF8 evidence path")?,
        ])
        .await;
        control(&["close", &self.id]).await?;
        archive.context("retaining stopped runtime home")?;
        Ok(())
    }
}

pub(super) fn input_document_id<'a>(receipt: &'a Value, collection: &str) -> Result<&'a str> {
    let rows = receipt["data"][format!("add_{collection}")]
        .as_array()
        .context("check input write receipt rows missing")?;
    ensure!(
        rows.len() == 1,
        "check input write must return exactly one document"
    );
    rows[0]["_docID"]
        .as_str()
        .context("check input document ID missing")
}

#[test]
fn input_receipt_uses_the_canonical_add_response() {
    let receipt = serde_json::json!({"data":{"add_HostCheck":[{"_docID":"input-1"}]}});
    assert_eq!(input_document_id(&receipt, "HostCheck").unwrap(), "input-1");
    for rows in [
        serde_json::json!([]),
        serde_json::json!([{}]),
        serde_json::json!([{"_docID":"a"},{"_docID":"b"}]),
    ] {
        assert!(input_document_id(
            &serde_json::json!({"data":{"add_HostCheck":rows}}),
            "HostCheck"
        )
        .is_err());
    }
}

pub(super) async fn configuration_snapshot(access: &ConfigAccess) -> Result<Value> {
    let mut query = String::from("{");
    for collection in Collection::ALL {
        let (fields, _) = gents::config_client::config_projection(collection, None)?;
        query.push_str(&format!(
            " {} {{ _docID {} }}",
            collection.graphql_type(),
            fields.join(" ")
        ));
    }
    query.push('}');
    let response = access.execute(&query).await?;
    let mut snapshot = serde_json::Map::new();
    for collection in Collection::ALL {
        let name = collection.graphql_type();
        let mut rows = response["data"][name]
            .as_array()
            .context("configuration rows missing")?
            .clone();
        rows.sort_by(|a, b| a["_docID"].as_str().cmp(&b["_docID"].as_str()));
        snapshot.insert(name.into(), Value::Array(rows));
    }
    Ok(Value::Object(snapshot))
}

#[tokio::test]
async fn batched_configuration_snapshot_matches_individual_reads() -> Result<()> {
    let db = crate::support::test_db("batched-config-snapshot").await;
    let access = ConfigAccess::Local(db.node.clone());
    let snapshot = configuration_snapshot(&access).await?;
    for collection in Collection::ALL {
        let (fields, _) = gents::config_client::config_projection(collection, None)?;
        let name = collection.graphql_type();
        let response = access
            .execute(&format!("{{ {name} {{ _docID {} }} }}", fields.join(" ")))
            .await?;
        let mut rows = response["data"][name]
            .as_array()
            .context("missing rows")?
            .clone();
        rows.sort_by(|a, b| a["_docID"].as_str().cmp(&b["_docID"].as_str()));
        assert_eq!(snapshot[name], Value::Array(rows));
    }
    db.node.shutdown().await;
    Ok(())
}

#[tokio::test]
#[ignore = "container: requires source-built gents-eval-runtime image and explicit inference settings"]
async fn isolated_host_runtime_survives_restart_without_changing_configuration() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut host = Host::start(root.path()).await?;
    let result: Result<()> = async {
        host.configure_trial().await?;
        host.wait_for_activation().await?;
        let before = configuration_snapshot(&host.access).await?;
        ensure!(before["AgentPrincipal"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1));
        host.restart("restart").await?;
        host.wait_for_activation().await?;
        let after = configuration_snapshot(&host.access).await?;
        ensure!(
            before == after,
            "runtime restart changed persisted configuration"
        );
        ensure!(host.snapshot("restarted").await?["api"]["exit_code"] == 0);
        host.fault("api-permission", "fault-after-restart").await?;
        ensure!(host.snapshot("faulted").await?["api"]["exit_code"] == 1);
        let candidate = host.fork(&root.path().join("candidate")).await?;
        let candidate_check: Result<()> = async {
            candidate.wait_for_activation().await?;
            ensure!(
                configuration_snapshot(&candidate.access).await? == before,
                "candidate fork changed canonical configuration"
            );
            ensure!(
                candidate.snapshot("fresh-host").await?["api"]["exit_code"] == 0,
                "candidate copied original host faults"
            );
            Ok(())
        }
        .await;
        let retired = candidate.close().await;
        retired?;
        host.resume("after-candidate").await?;
        host.wait_for_activation().await?;
        candidate_check?;
        ensure!(
            configuration_snapshot(&host.access).await? == before,
            "candidate changed original canonical configuration"
        );
        ensure!(
            host.snapshot("original-after-candidate").await?["api"]["exit_code"] == 1,
            "candidate changed original host effects"
        );
        Ok(())
    }
    .await;
    host.close().await?;
    result
}
