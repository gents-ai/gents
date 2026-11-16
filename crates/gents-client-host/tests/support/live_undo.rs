use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use base64::Engine;
use gents_protocol::file_changes::{FileImage, ToolFileChanges};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use serde_json::{json, Value};

use super::Attachment;

const DRAFT: &str = "Keep this unsent Undo draft";
const PARTICIPANT_SUFFIX: &str = "Editor save participant λ\n";

pub struct Undo {
    root: PathBuf,
    cases: Value,
}

impl Undo {
    pub async fn prepare(root: &Path, gate: Value) -> Result<Self> {
        let root = tokio::fs::canonicalize(root).await?;
        let mut cases = json!([
            {"files":[
                {"path":"undo-success-a.txt","before":"Original success A λ\n","after":"Recorded success A λ\n"},
                {"path":"undo-success-b.txt","before":"Original success B β\n","after":"Recorded success B β\n"}
            ]},
            {"files":[
                {"path":"undo-dirty.txt","before":"Original dirty\n","after":"Recorded dirty\n","later":"Unsent user buffer\n"},
                {"path":"undo-disk.txt","before":"Original disk\n","after":"Recorded disk\n","later":"Later disk changes\n"},
                {"path":"undo-created.txt","before":null,"after":"Created recorded target\n"},
                {"path":"undo-locked.txt","before":"Original locked\n","after":"Recorded locked\n"}
            ]},
            {"files":[
                {"path":"undo-partial-a.txt","before":"Original partial A\n","after":"Recorded partial A\n"},
                {"path":"undo-partial-b.txt","before":"Original partial B\n","after":"Recorded partial B\n","later":"External save conflict\n"}
            ]}
        ]);
        for (index, case) in cases
            .as_array_mut()
            .context("Undo cases")?
            .iter_mut()
            .enumerate()
        {
            let files = case["files"].as_array().context("Undo files")?;
            for file in files {
                if let Some(before) = file["before"].as_str() {
                    tokio::fs::write(root.join(text(file, "path")?), before).await?;
                }
            }
            let writes = files.iter().map(|file| json!({
                "path":file["path"],"content":file["after"],"overwrite":true,"raw_json":true,
            })).collect::<Vec<_>>();
            let instructions = format!(
                "Call write_file with each of these argument objects exactly: {}. Preserve every other file. After all writes succeed, answer briefly. Do not call update_goal, create another Goal, or launch background work.",
                json!(writes),
            );
            case["prompt"] = if index == 0 {
                json!(format!("First complete this foreground held tool: {} Only after the held tool returns, {instructions}", text(&gate, "prompt")?))
            } else {
                json!(instructions)
            };
        }
        tokio::fs::write(
            root.join("editor-undo-fixture.json"),
            serde_json::to_vec(&json!({
                "cases":cases,"draft":DRAFT,"participantSuffix":PARTICIPANT_SUFFIX,"gate":gate,
            }))?,
        )
        .await?;
        Ok(Self { root, cases })
    }

    pub fn configure(&self, document: &mut Value, node: &str) -> Result<()> {
        let context_id = document["agents"]
            .as_array()
            .and_then(|agents| agents.iter().find(|agent| agent["node_did"] == node))
            .and_then(|agent| agent["context_id"].as_str())
            .context("configure the held tool before Undo")?
            .to_owned();
        let context = document["contexts"]
            .as_array_mut()
            .and_then(|contexts| {
                contexts.iter_mut().find(|context| {
                    context["node_did"] == node && context["context_id"] == context_id
                })
            })
            .context("Undo context")?;
        let tools_id = text(context, "tools_id")?.to_owned();
        context["system_prompt"] = json!("Follow the current user's exact native file-write recipe. A supplied held tool must run in the foreground before file writes; wait for its result. After the specified writes complete, give a brief acknowledgement. The user controls the Goal: never call update_goal or change its status.");
        let tools = document["tools"]
            .as_array_mut()
            .and_then(|tools| {
                tools
                    .iter_mut()
                    .find(|tools| tools["node_did"] == node && tools["tools_id"] == tools_id)
            })
            .context("Undo tools")?;
        tools["host"]["root"] = json!(self.root);
        tools["host"]["files"] = json!({"mode":"ReadWrite"});
        Ok(())
    }

    pub async fn verify(
        &self,
        client: &mut Attachment,
        id: &mut u32,
        access: &ConfigAccess,
        node: &str,
        author: &str,
    ) -> Result<Value> {
        let ui: Value =
            serde_json::from_slice(&tokio::fs::read(self.root.join("editor-undo-ui.json")).await?)?;
        for field in [
            "twoFilesRestoredAndSaved",
            "savedEventsObserved",
            "globalThenIndividualRefused",
            "reloadPreserved",
            "crashRestartPreserved",
            "dirtyPreserved",
            "multiFileWorkspaceEditPreserved",
            "saveParticipantPreserved",
            "diskPreserved",
            "createdPreserved",
            "lockPreserved",
            "partialSavePreserved",
            "partialSaveReloadPreserved",
            "partialSaveCrashRestartPreserved",
            "draftAndPausedGoalPreserved",
            "retainedReviewReopened",
        ] {
            ensure!(ui[field] == true, "installed Undo omitted {field}");
        }
        let value = access.execute(&format!(
            "{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}}}) {{{}}} Goal(filter: {{node_did: {{_eq: \"{}\"}}}}) {{goal_id objective status session_id}} }}",
            escape_graphql_string(node), gents::SIGNED_REQUEST_FIELDS, escape_graphql_string(node),
        )).await?;
        let requests: Vec<AgentRequestRow> =
            serde_json::from_value(value["data"]["AgentRequest"].clone())?;
        ensure!(
            requests
                .iter()
                .all(|request| request.content.as_deref() != Some(DRAFT)),
            "Undo submitted the user's draft"
        );
        let mut sessions = Vec::new();
        let mut retained = Vec::new();
        for case in self.cases.as_array().context("Undo cases")? {
            let request = requests
                .iter()
                .find(|request| request.content.as_deref() == case["prompt"].as_str())
                .context("the user's Undo preparation request")?;
            gents_store::identity::request_receipt::verify_request_receipt_signature(request)?;
            let session = request.session_id.as_deref().context("Undo session")?;
            ensure!(
                request.requester_did.as_deref() == Some(author)
                    && request.admission_signer_did.as_deref() == Some(author)
                    && request.lifecycle_state == Some(RequestLifecycleState::Completed)
                    && !sessions.iter().any(|known| known == session),
                "Undo cases lost their signed author, completion, or separate chats"
            );
            sessions.push(session.to_owned());
            let tools = access.execute(&format!(
                "{{ AgentToolCall(filter: {{request_doc_id: {{_eq: \"{}\"}}}}) {{_docID lifecycle_state}} }}",
                escape_graphql_string(request.doc_id.as_deref().context("Undo physical request")?),
            )).await?;
            let mut changes = Vec::new();
            for call in tools["data"]["AgentToolCall"]
                .as_array()
                .context("Undo tools")?
            {
                if call["lifecycle_state"] != "completed" {
                    continue;
                }
                let value = client.try_call(*id, "desktop_tool_file_changes", json!({
                    "nodeDid":node,"sessionId":session,"requestId":request.request_id,"toolCallDocId":call["_docID"],
                })).await?;
                *id += 1;
                if !value.is_null() {
                    changes.extend(serde_json::from_value::<ToolFileChanges>(value)?.changes);
                }
            }
            for file in case["files"].as_array().context("Undo files")? {
                let expected_path =
                    tokio::fs::canonicalize(self.root.join(text(file, "path")?)).await?;
                let mut observed = false;
                for change in &changes {
                    if tokio::fs::canonicalize(Path::new(&change.root).join(&change.path)).await?
                        != expected_path
                    {
                        continue;
                    }
                    observed |= exact_image(&change.before, file["before"].as_str())?
                        && exact_image(&change.after, file["after"].as_str())?;
                }
                ensure!(
                    observed,
                    "Undo lost the exact retained images for {}",
                    text(file, "path")?
                );
                retained.push(
                    json!({"path":file["path"],"requestId":request.request_id,"sessionId":session}),
                );
            }
        }
        let success = &self.cases[0];
        ensure!(
            value["data"]["Goal"]
                .as_array()
                .is_some_and(|goals| goals
                    .iter()
                    .any(|goal| goal["objective"] == success["prompt"]
                        && goal["status"] == "paused"
                        && goal["session_id"] == sessions[0])),
            "Undo did not preserve the user's paused Goal"
        );
        for file in success["files"]
            .as_array()
            .context("successful Undo files")?
        {
            ensure!(
                tokio::fs::read(self.root.join(text(file, "path")?)).await?
                    == text(file, "after")?.as_bytes(),
                "a repeated Undo replaced the recreated postimage"
            );
        }
        for file in self.cases[1]["files"]
            .as_array()
            .context("protected Undo files")?
        {
            let expected = if file["path"] == "undo-disk.txt" {
                text(file, "later")?
            } else {
                text(file, "after")?
            };
            ensure!(
                tokio::fs::read(self.root.join(text(file, "path")?)).await? == expected.as_bytes(),
                "Undo changed a protected file"
            );
        }
        let conflict = text(&ui, "partialConflictPath")?;
        let partial = self.cases[2]["files"]
            .as_array()
            .context("partial Undo files")?;
        ensure!(
            partial.iter().any(|file| file["path"] == conflict),
            "partial conflict is outside the prepared files"
        );
        let conflict_text = partial
            .iter()
            .find_map(|file| file["later"].as_str())
            .context("partial conflict bytes")?;
        for file in partial {
            let expected = if file["path"] == conflict {
                conflict_text.to_owned()
            } else {
                format!("{}{PARTICIPANT_SUFFIX}", text(file, "before")?)
            };
            ensure!(
                tokio::fs::read(self.root.join(text(file, "path")?)).await? == expected.as_bytes(),
                "Undo lost a saved or conflicted file outcome"
            );
        }
        Ok(
            json!({"ui":ui,"retainedImages":retained,"independentChats":sessions,
            "signedRequestsVerified":true,"pausedGoalPreserved":true,"workingCopiesPreserved":true}),
        )
    }
}

fn exact_image(image: &FileImage, expected: Option<&str>) -> Result<bool> {
    Ok(match (image, expected) {
        (FileImage::Missing, None) => true,
        (FileImage::File { data, .. }, Some(expected)) => {
            base64::engine::general_purpose::STANDARD.decode(data)? == expected.as_bytes()
        }
        _ => false,
    })
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .with_context(|| format!("Undo fixture omitted {field}"))
}
