use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use gents::config_client::GraphqlEndpoint;
use gents_protocol::serve_lifecycle::{
    control::{
        RuntimeControlCommand, RuntimeControlDisposition, RuntimeControlReceipt, RuntimeOperation,
        RUNTIME_CONTROL_PATH, RUNTIME_CONTROL_VERSION,
    },
    preparation::{
        RuntimePreparationOutcome, RuntimePreparationRefusal, RuntimePreparationReport,
        RuntimePreparationResult, RUNTIME_PREPARATION_VERSION,
    },
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::{Child as PreparationChild, Command};

use super::{Attachment, Child};

const AFTER_UPDATE: &str = "Continue in the same chat after the runtime update.";

pub async fn refuse_busy(binary: &Path, home: &Path) -> Result<()> {
    let before = tokio::fs::read(home.join("runtime.json")).await?;
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        Command::new(binary)
            .args(["prepare-runtime", "--home"])
            .arg(home)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        !output.status.success(),
        "busy preparation unexpectedly succeeded"
    );
    let report: RuntimePreparationReport =
        serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "busy preparation output: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        })?;
    ensure!(report.version == RUNTIME_PREPARATION_VERSION);
    ensure!(
        matches!(
            report.result,
            RuntimePreparationResult::Refused {
                reason: RuntimePreparationRefusal::Busy,
                ..
            }
        ),
        "{report:?}"
    );
    ensure!(
        tokio::fs::read(home.join("runtime.json")).await? == before,
        "busy preparation changed the saved route"
    );
    Ok(())
}

pub struct Fixture<'a> {
    pub root: &'a Path,
    pub home: &'a Path,
    pub binary: &'a Path,
    pub node_did: &'a str,
    pub agent_id: &'a str,
    pub session: &'a str,
    pub client_did: &'a str,
    pub original_doc_id: &'a str,
    pub access: &'a gents::ConfigAccess,
}

impl Fixture<'_> {
    /// Serving-runtime readiness and the attached client's authorized write route
    /// are separate observations during replacement.
    pub async fn replace_and_verify(
        &self,
        original: &mut Child,
        client: &mut Attachment,
        id: &mut u32,
    ) -> Result<()> {
        let init = tokio::fs::read(self.home.join("init.json")).await?;
        let routing_before: Value =
            serde_json::from_slice(&tokio::fs::read(self.home.join("runtime.json")).await?)?;
        let config: Value = serde_json::from_slice(&init)?;
        let key_path = Path::new(config["key_path"].as_str().context("runtime key path")?);
        let key = tokio::fs::read(key_path).await?;
        let p2p_secret =
            tokio::fs::read(self.home.join(gents::home::P2P_SECRET_KEY_FILE_NAME)).await?;
        let provider_query = format!("{{ InferenceBackend(filter: {{node_did: {{_eq: \"{}\"}}}}) {{ _docID backend_id provider_kind endpoint }} }}", gents::graphql::escape_graphql_string(self.node_did));
        let providers = rows(
            self.access.execute(&provider_query).await?,
            "InferenceBackend",
        )?;
        let session_query = format!("{{ AgentSession(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}) {{ _docID node_did session_id requester_did agent_id created_at }} }}", gents::graphql::escape_graphql_string(self.node_did), gents::graphql::escape_graphql_string(self.session));
        let sessions = rows(self.access.execute(&session_query).await?, "AgentSession")?;
        let (mut replacement, ready) = self.start_replacement(original).await?;
        let RuntimePreparationResult::Ready {
            outcome,
            node_did,
            graphql,
            build,
            home,
        } = ready
        else {
            anyhow::bail!("replacement did not report readiness: {ready:?}");
        };
        ensure!(outcome == RuntimePreparationOutcome::Replaced && node_did == self.node_did);
        ensure!(tokio::fs::canonicalize(home).await? == tokio::fs::canonicalize(self.home).await?);
        ensure!(
            replacement.try_wait()?.is_none(),
            "replacement stopped after its observer closed"
        );
        #[cfg(unix)]
        let stopped = tokio::time::timeout(Duration::from_secs(30), original.wait()).await??;
        #[cfg(windows)]
        let stopped = {
            let stopped =
                tokio::time::timeout(Duration::from_secs(30), original.wait_for_exit()).await??;
            original.finish_output().await?;
            stopped
        };
        ensure!(
            stopped.success(),
            "original runtime did not exit cleanly: {stopped}"
        );
        let retained = super::read_session(client, id, self.node_did, self.session).await?;
        ensure!(
            retained["timelineItems"]
                .as_array()
                .is_some_and(
                    |items| items.iter().any(|item| item["kind"] == "assistantMessage"
                        && item["content"] == "First part and final part.")
                ),
            "the original transcript was not retained: {retained}"
        );
        ensure!(
            rows(self.access.execute(&session_query).await?, "AgentSession")? == sessions,
            "replacement changed the durable session identity"
        );
        ensure!(
            rows(
                self.access.execute(&provider_query).await?,
                "InferenceBackend"
            )? == providers,
            "replacement changed provider settings"
        );
        super::wait_chat_deployment(client, id, self.node_did, self.access).await?;
        let receipt = client
            .call(
                *id,
                "desktop_chat_send",
                json!({"request": {
                    "nodeDid":self.node_did, "sessionId":self.session, "agentId":self.agent_id,
                    "content":AFTER_UPDATE,"delivery":"queue",
                }}),
            )
            .await;
        *id += 1;
        ensure!(
            receipt["sessionId"] == self.session,
            "follow-up started another chat: {receipt}"
        );
        let followup = receipt["requestId"]
            .as_str()
            .context("follow-up request id")?;
        let completion = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let output = client.call(*id, "desktop_request_output", json!({"nodeDid":self.node_did,"sessionId":self.session,"requestId":followup})).await;
                *id += 1;
                if output["output"]["kind"] == "terminalMessage" {
                    ensure!(output["output"]["content"] == "First part and final part.", "follow-up output: {output}");
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }).await;
        if !matches!(&completion, Ok(Ok(()))) {
            let query = format!("{{ AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{ _docID request_id lifecycle_state content admission_signer_did }} }}", gents::graphql::escape_graphql_string(followup));
            let observed = self.access.execute(&query).await;
            *id += 1;
            let snapshot = client.snapshot(*id).await;
            *id += 1;
            let routing_after = tokio::fs::read_to_string(self.home.join("runtime.json")).await?;
            anyhow::bail!("retained follow-up did not complete: {completion:?}; native input: {observed:?}; routing before: {routing_before}; routing after: {routing_after}; client: {snapshot}");
        }
        let requests_query = format!("{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}, purpose: {{_eq: \"normal\"}}}}) {{ {} }} }}", gents::graphql::escape_graphql_string(self.node_did), gents::graphql::escape_graphql_string(self.session), gents::SIGNED_REQUEST_FIELDS);
        let requests = self.access.execute(&requests_query).await?;
        let requests: Vec<gents_protocol::row::AgentRequestRow> =
            serde_json::from_value(requests["data"]["AgentRequest"].clone())?;
        ensure!(
            requests.len() == 2,
            "replacement duplicated submitted input: {requests:?}"
        );
        ensure!(
            requests
                .iter()
                .any(|row| row.doc_id.as_deref() == Some(self.original_doc_id)),
            "replacement lost the original physical request"
        );
        ensure!(
            requests
                .iter()
                .any(|row| row.request_id == followup
                    && row.content.as_deref() == Some(AFTER_UPDATE)),
            "follow-up was not durably admitted"
        );
        for request in &requests {
            ensure!(
                request.admission_signer_did.as_deref() == Some(self.client_did),
                "replacement changed the enrolled requester"
            );
            gents::verify_request_receipt_signature(request)?;
        }
        ensure!(tokio::fs::read(self.home.join("init.json")).await? == init);
        ensure!(tokio::fs::read(key_path).await? == key);
        ensure!(
            tokio::fs::read(self.home.join(gents::home::P2P_SECRET_KEY_FILE_NAME)).await?
                == p2p_secret
        );
        let reused = Command::new(self.binary)
            .args(["prepare-runtime", "--home"])
            .arg(self.home)
            .kill_on_drop(true)
            .output()
            .await?;
        ensure!(
            reused.status.success(),
            "reuse failed: {}",
            String::from_utf8_lossy(&reused.stderr)
        );
        let reused: RuntimePreparationReport = serde_json::from_slice(&reused.stdout)?;
        ensure!(
            matches!(
                reused.result,
                RuntimePreparationResult::Ready {
                    outcome: RuntimePreparationOutcome::Reused,
                    ..
                }
            ),
            "{reused:?}"
        );
        self.retire_replacement(&graphql, &build).await?;
        let status = tokio::time::timeout(Duration::from_secs(60), replacement.wait())
            .await
            .context(
                "replacement did not finish native shutdown after successful retained follow-up",
            )??;
        ensure!(
            status.success(),
            "replacement did not exit cleanly: {status}"
        );
        tokio::fs::write(self.root.join("runtime-replacement.json"), serde_json::to_vec_pretty(&json!({
            "sameEnrolledRequester":true,"originalRequestPreserved":true,"normalRequestCount":requests.len(),
            "followupRequestId":followup,"matchingBuildReused":true,"build":build,
        }))?).await?;
        Ok(())
    }

    async fn start_replacement(
        &self,
        original: &mut Child,
    ) -> Result<(PreparationChild, RuntimePreparationResult)> {
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                ensure!(
                    original.try_wait()?.is_none(),
                    "original runtime exited before authenticated replacement"
                );
                let mut child = Command::new(self.binary)
                    .args(["prepare-runtime", "--home"])
                    .arg(self.home)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::from(
                        std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(self.root.join("runtime-replacement.err"))?,
                    ))
                    .kill_on_drop(true)
                    .spawn()?;
                let stdout = child.stdout.take().context("preparation stdout")?;
                let mut lines = BufReader::new(stdout).lines();
                let frame = lines
                    .next_line()
                    .await?
                    .context("replacement exited without a receipt")?;
                let report: RuntimePreparationReport = serde_json::from_str(&frame)?;
                ensure!(report.version == RUNTIME_PREPARATION_VERSION);
                match report.result {
                    RuntimePreparationResult::Refused {
                        reason: RuntimePreparationRefusal::Busy,
                        ..
                    } => {
                        ensure!(!child.wait().await?.success());
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    ready @ RuntimePreparationResult::Ready { .. } => return Ok((child, ready)),
                    refused => anyhow::bail!("replacement refused: {refused:?}"),
                }
            }
        })
        .await
        .context("runtime did not finish its owned cleanup before replacement")?
    }

    async fn retire_replacement(&self, graphql: &str, build: &str) -> Result<()> {
        let endpoint = GraphqlEndpoint::as_principal(graphql, self.node_did);
        let mut url = reqwest::Url::parse(graphql)?;
        url.set_path(RUNTIME_CONTROL_PATH);
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()?;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let command = RuntimeControlCommand {
                    version: RUNTIME_CONTROL_VERSION,
                    node_did: self.node_did.to_owned(),
                    nonce: uuid::Uuid::new_v4().to_string(),
                    issued_at: chrono::Utc::now().to_rfc3339(),
                    operation: RuntimeOperation::Retire {
                        expected_build: build.to_owned(),
                    },
                };
                let receipt: RuntimeControlReceipt = endpoint
                    .authorize(client.post(url.clone()).json(&command))?
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                gents::runtime_service::verify_control_receipt(&command, &receipt)?;
                match receipt.disposition {
                    RuntimeControlDisposition::Retiring => return Ok::<_, anyhow::Error>(()),
                    RuntimeControlDisposition::Busy => {
                        tokio::time::sleep(Duration::from_millis(50)).await
                    }
                    other => anyhow::bail!("replacement cleanup refused: {other:?}"),
                }
            }
        })
        .await
        .context("replacement did not finish owned work before cleanup")?
    }
}

fn rows(response: Value, collection: &str) -> Result<Vec<Value>> {
    let mut rows = response["data"][collection]
        .as_array()
        .context("native comparison rows")?
        .clone();
    rows.sort_by(|left, right| left["_docID"].as_str().cmp(&right["_docID"].as_str()));
    Ok(rows)
}
