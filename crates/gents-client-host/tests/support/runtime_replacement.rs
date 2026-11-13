use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use gents::support::live_inference::wait_for_request_terminal;
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
use gents_store::config_client::GraphqlEndpoint;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::{Child as PreparationChild, Command};

use super::{Attachment, Child};

const AFTER_UPDATE: &str = "Continue in the same chat after the runtime update.";

pub async fn refuse_busy(binary: &Path, home: &Path) -> Result<()> {
    let before = tokio::fs::read(home.join("runtime.json")).await?;
    let homes = command_homes(home.parent().context("runtime home parent")?).await?;
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        Command::new(binary)
            .envs(homes.env())
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
    pub agent_did: &'a str,
    pub session: &'a str,
    pub client_did: &'a str,
    pub original_doc_id: &'a str,
    pub access: &'a gents_store::config_client::ConfigAccess,
}

impl Fixture<'_> {
    /// Serving-runtime readiness and the attached client's authorized write route
    /// are separate observations during replacement.
    pub async fn replace_and_verify(
        &self,
        original: &mut Child,
        client: &mut Attachment,
        updates: &mut Attachment,
        id: &mut u32,
    ) -> Result<()> {
        let init = tokio::fs::read(self.home.join("init.json")).await?;
        let config: Value = serde_json::from_slice(&init)?;
        let key_path = Path::new(config["key_path"].as_str().context("runtime key path")?);
        let key = tokio::fs::read(key_path).await?;
        let p2p_secret = tokio::fs::read(
            self.home
                .join(gents_store::local::home::P2P_SECRET_KEY_FILE_NAME),
        )
        .await?;
        let provider_query = format!("{{ InferenceBackend(filter: {{node_did: {{_eq: \"{}\"}}}}) {{ _docID backend_id provider_kind endpoint }} }}", gents_store::graphql::escape_graphql_string(self.node_did));
        let providers = rows(
            self.access.execute(&provider_query).await?,
            "InferenceBackend",
        )?;
        let session_query = format!("{{ AgentSession(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}) {{ _docID node_did session_id requester_did agent_did created_at }} }}", gents_store::graphql::escape_graphql_string(self.node_did), gents_store::graphql::escape_graphql_string(self.session));
        let sessions = rows(self.access.execute(&session_query).await?, "AgentSession")?;
        let before = super::live_runtime::wait_client(
            client,
            updates,
            id,
            "desktop_session_snapshot",
            json!({"nodeDid":self.node_did,"sessionId":self.session,"timelineLimit":40}),
            |view| view["turnState"] == "completed",
        )
        .await?;
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
        let retained = super::live_runtime::wait_client(
            client,
            updates,
            id,
            "desktop_session_snapshot",
            json!({"nodeDid":self.node_did,"sessionId":self.session,"timelineLimit":40}),
            |view| view["turnState"] == "completed",
        )
        .await?;
        ensure!(
            retained["timelineItems"] == before["timelineItems"],
            "replacement changed the durable transcript"
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
        super::live_runtime::wait_client(
            client,
            updates,
            id,
            "desktop_client_snapshot",
            json!({}),
            |snapshot| {
                snapshot["client"]["deployments"]
                    .as_array()
                    .is_some_and(|rows| {
                        rows.iter()
                            .any(|row| row["nodeDid"] == self.node_did && row["chatSafe"] == true)
                    })
            },
        )
        .await?;
        let receipt = client
            .call(
                *id,
                "desktop_chat_send",
                json!({"request": {
                    "nodeDid":self.node_did, "sessionId":self.session, "agentDid":self.agent_did,
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
        ensure!(
            wait_for_request_terminal(self.access, followup, Duration::from_secs(30)).await
                == "completed"
        );
        let followup_view = super::live_runtime::wait_client(
            client,
            updates,
            id,
            "desktop_session_snapshot",
            json!({"nodeDid":self.node_did,"sessionId":self.session,"timelineLimit":40}),
            |view| view["latestRequestId"] == followup && view["turnState"] == "completed",
        )
        .await?;
        ensure!(followup_view["timelineItems"]
            .as_array()
            .is_some_and(
                |items| items.iter().any(|item| item["kind"] == "assistantMessage"
                    && item["content"]
                        .as_str()
                        .is_some_and(|text| !text.trim().is_empty()))
            ));
        let requests_query = format!("{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}, purpose: {{_eq: \"normal\"}}}}) {{ {} }} }}", gents_store::graphql::escape_graphql_string(self.node_did), gents_store::graphql::escape_graphql_string(self.session), gents::SIGNED_REQUEST_FIELDS);
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
            gents_store::identity::request_receipt::verify_request_receipt_signature(request)?;
        }
        ensure!(tokio::fs::read(self.home.join("init.json")).await? == init);
        ensure!(tokio::fs::read(key_path).await? == key);
        ensure!(
            tokio::fs::read(
                self.home
                    .join(gents_store::local::home::P2P_SECRET_KEY_FILE_NAME)
            )
            .await?
                == p2p_secret
        );
        let homes = command_homes(self.root).await?;
        let reused = tokio::time::timeout(
            Duration::from_secs(30),
            Command::new(self.binary)
                .envs(homes.env())
                .args(["prepare-runtime", "--home"])
                .arg(self.home)
                .kill_on_drop(true)
                .output(),
        )
        .await??;
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
        ensure!(
            original.try_wait()?.is_none(),
            "original runtime exited before replacement"
        );
        let homes = command_homes(self.root).await?;
        let log = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("runtime-replacement.err"))
            .await?
            .into_std()
            .await;
        let mut child = Command::new(self.binary)
            .envs(homes.env())
            .args(["prepare-runtime", "--home"])
            .arg(self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child.stdout.take().context("preparation stdout")?;
        let mut lines = BufReader::new(stdout).lines();
        let frame = tokio::time::timeout(Duration::from_secs(60), lines.next_line())
            .await
            .context("replacement readiness deadline")??
            .context("replacement exited without a receipt")?;
        let report: RuntimePreparationReport = serde_json::from_str(&frame)?;
        ensure!(report.version == RUNTIME_PREPARATION_VERSION);
        ensure!(
            matches!(report.result, RuntimePreparationResult::Ready { .. }),
            "replacement refused: {:?}",
            report.result
        );
        Ok((child, report.result))
    }

    async fn retire_replacement(&self, graphql: &str, build: &str) -> Result<()> {
        let config: Value =
            serde_json::from_slice(&tokio::fs::read(self.home.join("init.json")).await?)?;
        let key_path = config["key_path"]
            .as_str()
            .context("runtime key path")?
            .to_owned();
        let identity = tokio::task::spawn_blocking(move || {
            gents_store::identity::KeyIdentity::load_or_create(key_path, None)
        })
        .await??;
        let endpoint = GraphqlEndpoint::as_principal(graphql, self.node_did)
            .with_client_signing_capability(identity.client_signing_capability()?)?;
        let mut url = reqwest::Url::parse(graphql)?;
        url.set_path(RUNTIME_CONTROL_PATH);
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()?;
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
            .authorize(client.post(url).json(&command))?
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        gents_store::service::verify_control_receipt(&command, &receipt)?;
        ensure!(
            matches!(
                receipt.disposition,
                RuntimeControlDisposition::Retiring | RuntimeControlDisposition::Stopping
            ),
            "replacement cleanup refused: {:?}",
            receipt.disposition
        );
        Ok(())
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

async fn command_homes(root: &Path) -> Result<super::support::homes::ProcessHomes> {
    let root = root.to_owned();
    Ok(
        tokio::task::spawn_blocking(move || super::support::homes::ProcessHomes::under(&root))
            .await?,
    )
}
