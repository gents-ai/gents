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

const COUNT: usize = 65;
const CONTEXT: &str = "live-inventory-context";
const TOOLS: &str = "live-inventory-tools";

pub struct Inventory {
    root: PathBuf,
    pub seed_prompts: Vec<String>,
    next_prompt: String,
    repeat_prompt: String,
    follow_prompt: String,
    partial_prompt: String,
}

impl Inventory {
    pub async fn prepare(root: &Path) -> Result<Self> {
        let root = tokio::fs::canonicalize(root).await?;
        let mut files = Vec::new();
        for index in 0..=COUNT + 2 {
            let path = root.join(file(index));
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(path, before(index)).await?;
            files.push(json!({
                "file":file(index),"before":before(index),"after":after(index),"later":later(index),
                "beforeMarker":format!("Original recorded file {index}"),
                "afterMarker":format!("Updated recorded file {index}"),
            }));
        }
        let seed_prompts = (0..COUNT)
            .collect::<Vec<_>>()
            .chunks(8)
            .map(|indices| edit_prompt(indices))
            .collect::<Vec<_>>();
        let next_prompt = edit_prompt(&[COUNT]);
        let repeated = COUNT + 1;
        let intermediate = format!("Intermediate recorded version {repeated}\r\n");
        let repeat_prompt = format!(
            "Perform these native file operations in order. First read_file {}. Then edit_file {}. Then edit_file {}. Finally write_file {} to the same final bytes. Do not change any other file. After they succeed, reply briefly.",
            json!({"path":file(repeated),"raw_json":true}),
            json!({"path":file(repeated),"old_text":before(repeated),"new_text":intermediate,"raw_json":true}),
            json!({"path":file(repeated),"old_text":intermediate,"new_text":after(repeated),"raw_json":true}),
            json!({"path":file(repeated),"content":after(repeated),"overwrite":true,"raw_json":true}),
        );
        let command =
            json!({"command":"printf","args":["Unobserved inventory action\n"],"timeout_secs":10});
        let follow_prompt = format!(
            "Call bash with these arguments: {command}. Do not read or change files. Then reply briefly."
        );
        let partial_prompt = format!(
            "First call bash with these arguments: {command}. Then call edit_file with these arguments: {}. Do not change other files. After both calls finish, reply briefly.",
            edit_arguments(COUNT + 2),
        );
        tokio::fs::write(
            root.join("editor-inventory-fixture.json"),
            serde_json::to_vec(&json!({
                "count":COUNT,"alreadySeeded":true,"files":files,
                "nextPrompt":next_prompt,"repeatPrompt":repeat_prompt,
                "followPrompt":follow_prompt,"partialPrompt":partial_prompt,
            }))?,
        )
        .await?;
        Ok(Self {
            root,
            seed_prompts,
            next_prompt,
            repeat_prompt,
            follow_prompt,
            partial_prompt,
        })
    }

    pub fn configure(&self, document: &mut Value, node: &str) -> Result<()> {
        let agent = document["agents"]
            .as_array_mut()
            .and_then(|agents| agents.iter_mut().find(|agent| agent["node_did"] == node))
            .context("selected inventory Agent")?;
        agent["context_id"] = json!(CONTEXT);
        append(
            document,
            "contexts",
            json!({"node_did":node,"context_id":CONTEXT,"tools_id":TOOLS,
                "system_prompt":"Perform the user's specified native file operations with their exact paths and content. Preserve all unspecified files and bytes. Complete every requested operation before your short final acknowledgement. Do not add extra file edits."}),
        );
        append(
            document,
            "tools",
            json!({"node_did":node,"tools_id":TOOLS,"host":{
                "root":self.root,"files":{"mode":"ReadWrite"},
                "bash":{"mode":"ReadOnly","network_mode":"disabled","read_only_commands":["printf"],"allowed_argv_prefixes":[["printf"]],"timeout_secs":10,"max_timeout_secs":10}
            }}),
        );
        Ok(())
    }

    pub async fn verify(
        &self,
        client: &mut Attachment,
        id: &mut u32,
        access: &ConfigAccess,
        node: &str,
        session: &str,
        author: &str,
    ) -> Result<Value> {
        let ui: Value = serde_json::from_slice(
            &tokio::fs::read(self.root.join("editor-inventory-ui.json")).await?,
        )?;
        for field in [
            "pagedAllFiles",
            "restoredSelectedDiff",
            "newActivityVisible",
            "restoredAfterNewActivity",
            "workingCopyPreserved",
            "repeatedNetComparison",
            "settledCardUnpinned",
            "repeatedDiffRestored",
            "unknownOtherRequestPreservesTotals",
            "unknownSameRequestHidesTotals",
            "partialSurvivesReload",
        ] {
            ensure!(ui[field] == true, "editor inventory omitted {field}");
        }
        let selected = usize::try_from(
            ui["selectedIndex"]
                .as_u64()
                .context("selected inventory file")?,
        )?;
        ensure!(selected < COUNT, "the editor selected a non-initial file");
        let response = access
            .execute(&format!(
                "{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}) {{{}}} }}",
                escape_graphql_string(node), escape_graphql_string(session), gents::SIGNED_REQUEST_FIELDS,
            ))
            .await?;
        let requests: Vec<AgentRequestRow> =
            serde_json::from_value(response["data"]["AgentRequest"].clone())?;
        let mut captures = Vec::new();
        for prompt in self.seed_prompts.iter().chain([
            &self.next_prompt,
            &self.repeat_prompt,
            &self.follow_prompt,
            &self.partial_prompt,
        ]) {
            let request = requests
                .iter()
                .find(|request| request.content.as_deref() == Some(prompt.as_str()))
                .context("authored inventory request")?;
            gents_store::identity::request_receipt::verify_request_receipt_signature(request)?;
            ensure!(
                request.requester_did.as_deref() == Some(author)
                    && request.admission_signer_did.as_deref() == Some(author)
                    && request.lifecycle_state == Some(RequestLifecycleState::Completed),
                "inventory work changed its author or did not complete"
            );
            let response = access.execute(&format!(
                "{{ AgentToolCall(filter: {{request_doc_id: {{_eq: \"{}\"}}}}) {{_docID lifecycle_state}} }}",
                escape_graphql_string(request.doc_id.as_deref().context("physical inventory request")?),
            )).await?;
            for call in response["data"]["AgentToolCall"]
                .as_array()
                .context("inventory tools")?
            {
                if call["lifecycle_state"] != "completed" {
                    continue;
                }
                let value = client
                    .try_call(
                        *id,
                        "desktop_tool_file_changes",
                        json!({
                            "nodeDid":node,"sessionId":session,"requestId":request.request_id,
                            "toolCallDocId":call["_docID"],
                        }),
                    )
                    .await?;
                *id += 1;
                if !value.is_null() {
                    captures.push(serde_json::from_value::<ToolFileChanges>(value)?);
                }
            }
        }
        ensure!(
            requests.iter().all(|request| request.content.as_deref()
                != Some("Keep this draft while reviewing the complete file inventory.")),
            "the editor submitted its unsent inventory draft"
        );
        let mut retained = Vec::new();
        for change in captures.iter().flat_map(|capture| &capture.changes) {
            retained.push((
                tokio::fs::canonicalize(Path::new(&change.root).join(&change.path)).await?,
                image_bytes(&change.before)?,
                image_bytes(&change.after)?,
            ));
        }
        for index in 0..=COUNT + 2 {
            let expected_path = tokio::fs::canonicalize(self.root.join(file(index))).await?;
            let expected_before = before(index);
            let expected_after = after(index);
            let before_seen = retained.iter().any(|(path, before, _)| {
                path == &expected_path && before.as_deref() == Some(expected_before.as_bytes())
            });
            let after_seen = retained.iter().any(|(path, _, after)| {
                path == &expected_path && after.as_deref() == Some(expected_after.as_bytes())
            });
            ensure!(
                before_seen && after_seen,
                "file {index} lost its retained original or final image"
            );
            let expected = if index == selected || index == COUNT + 1 {
                later(index)
            } else {
                after(index)
            };
            ensure!(
                tokio::fs::read(self.root.join(file(index))).await? == expected.as_bytes(),
                "review changed the current bytes of file {index}"
            );
        }
        Ok(
            json!({"sessionId":session,"initialFiles":COUNT,"selectedIndex":selected,
            "retainedOriginalAndFinalImages":true,"workingCopiesPreserved":true,"ui":ui}),
        )
    }
}

fn file(index: usize) -> String {
    match index {
        0 => "inventory/first/shared.txt".to_owned(),
        64 => "inventory/second/shared.txt".to_owned(),
        _ => format!("inventory/file-{index:03}.txt"),
    }
}

fn before(index: usize) -> String {
    format!("Original recorded file {index}\r\nretained original tail {index}\r\n")
}

fn after(index: usize) -> String {
    format!("Updated recorded file {index}\r\nretained updated tail {index}\r\n")
}

fn later(index: usize) -> String {
    format!("Later working copy {index} must remain unchanged.\n")
}

fn edit_arguments(index: usize) -> Value {
    json!({"path":file(index),"old_text":before(index),"new_text":after(index),"raw_json":true})
}

fn edit_prompt(indices: &[usize]) -> String {
    format!(
        "Call edit_file for every argument object in this list: {}. Preserve all other bytes and files. After all these edits succeed, give a short acknowledgement.",
        json!(indices.iter().map(|index| edit_arguments(*index)).collect::<Vec<_>>()),
    )
}

fn image_bytes(image: &FileImage) -> Result<Option<Vec<u8>>> {
    match image {
        FileImage::File { data, .. } => Ok(Some(
            base64::engine::general_purpose::STANDARD.decode(data)?,
        )),
        FileImage::Missing | FileImage::Unavailable { .. } => Ok(None),
    }
}

fn append(document: &mut Value, collection: &str, value: Value) {
    if !document[collection].is_array() {
        document[collection] = json!([]);
    }
    document[collection].as_array_mut().unwrap().push(value);
}
