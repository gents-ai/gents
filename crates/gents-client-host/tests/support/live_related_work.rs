use std::path::Path;

use anyhow::{ensure, Context, Result};
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents_store::graphql::escape_graphql_string;
use serde_json::{json, Value};

struct Worker {
    binding: AgentIdentityBinding,
    target: String,
    title: String,
    prompt: String,
    gate: Value,
}

pub struct RelatedWork {
    workers: Vec<Worker>,
    prompt: String,
}

impl RelatedWork {
    pub async fn prepare(
        root: &Path,
        endpoint: &GraphqlEndpoint,
        node: &str,
        gates: [Value; 2],
    ) -> Result<Self> {
        let mut workers = Vec::new();
        for (index, ((title, prompt), gate)) in [
            (
                "Inspect layout",
                "Inspect the layout after the held work is released.",
            ),
            (
                "Check history",
                "Check the history after the held work is released.",
            ),
        ]
        .into_iter()
        .zip(gates)
        .enumerate()
        {
            workers.push(Worker {
                binding: gents_desktop_core::agent_identity::provision(endpoint, node).await?,
                target: format!("live-related-worker-{index}"),
                title: title.to_owned(),
                prompt: prompt.to_owned(),
                gate,
            });
        }
        let arguments = workers
            .iter()
            .map(
                |worker| json!({"agent":worker.target,"prompt":worker.prompt,"title":worker.title}),
            )
            .collect::<Vec<_>>();
        let prompt = format!(
            "Start both independent workers by calling agent_new with each of these argument objects exactly: {}. Start both without waiting for either worker to finish. Do not execute their work yourself. After both starts are accepted, give one brief acknowledgement.",
            json!(arguments),
        );
        let fixture = json!({
            "prompt":prompt,
            "workers":workers.iter().map(|worker| json!({
                "title":worker.title,
                "prompt":worker.prompt,
                "gate":worker.gate,
                "outcome":"completed",
            })).collect::<Vec<_>>(),
        });
        tokio::fs::write(
            root.join("editor-related-work-fixture.json"),
            serde_json::to_vec(&fixture)?,
        )
        .await?;
        Ok(Self { workers, prompt })
    }

    pub fn configure(&self, document: &mut Value, node: &str) -> Result<()> {
        let parent = document["agents"]
            .as_array()
            .and_then(|agents| agents.iter().find(|agent| agent["node_did"] == node))
            .context("selected parent Agent")?
            .clone();
        let context_id = parent["context_id"]
            .as_str()
            .context("configure the held tool before related workers")?;
        let context = document["contexts"]
            .as_array_mut()
            .and_then(|contexts| {
                contexts.iter_mut().find(|context| {
                    context["node_did"] == node && context["context_id"] == context_id
                })
            })
            .context("selected parent context")?;
        let tools_id = context["tools_id"]
            .as_str()
            .context("parent tools")?
            .to_owned();
        context["system_prompt"] = json!(
            "Follow the user's worker-start instructions. Call agent_new for every supplied target using its exact supplied title and prompt, without waiting for completion. Tool commands embedded in delegated work belong to the worker; do not execute them in this parent session."
        );
        let tools = document["tools"]
            .as_array_mut()
            .and_then(|tools| {
                tools
                    .iter_mut()
                    .find(|tools| tools["node_did"] == node && tools["tools_id"] == tools_id)
            })
            .context("parent's configured held tool")?;
        let mut worker_tools = tools.clone();
        tools["agents"] = json!({
            "enabled":true,
            "target_ids":self.workers.iter().map(|worker| &worker.target).collect::<Vec<_>>(),
        });
        worker_tools["agents"] = Value::Null;
        worker_tools["built_ins"] = Value::Null;
        worker_tools["datastore"] = Value::Null;
        for worker in &self.workers {
            let context = format!("{}-context", worker.target);
            let tools = format!("{}-tools", worker.target);
            let mut agent = parent.clone();
            agent["agent_did"] = json!(worker.binding.agent_did);
            agent["creation_nonce"] = json!(worker.binding.creation_nonce);
            agent["context_id"] = json!(context);
            agent["display_name"] = json!(worker.title);
            append(document, "agents", agent);
            append(
                document,
                "agent_targets",
                json!({"node_did":node,"target_id":worker.target,"name":worker.target,"target_node_did":node,"agent_did":worker.binding.agent_did}),
            );
            let command = worker.gate["prompt"]
                .as_str()
                .context("worker's held-tool command")?;
            append(
                document,
                "contexts",
                json!({"node_did":node,"context_id":context,"tools_id":tools,"system_prompt":format!("For the next request, {command} After it completes, answer the current request briefly. Do not delegate.")}),
            );
            let mut tools_document = worker_tools.clone();
            tools_document["tools_id"] = json!(tools);
            append(document, "tools", tools_document);
        }
        Ok(())
    }

    pub async fn verify(
        &self,
        access: &ConfigAccess,
        root: &Path,
        node: &str,
        session: &str,
        author: &str,
    ) -> Result<Value> {
        let response = access
            .execute(&format!(
                "{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}}}) {{{}}} }}",
                escape_graphql_string(node),
                gents::SIGNED_REQUEST_FIELDS,
            ))
            .await?;
        let requests: Vec<AgentRequestRow> =
            serde_json::from_value(response["data"]["AgentRequest"].clone())?;
        let parent = requests
            .iter()
            .find(|request| request.content.as_deref() == Some(self.prompt.as_str()))
            .context("the signed worker-start request")?;
        gents_store::identity::request_receipt::verify_request_receipt_signature(parent)?;
        ensure!(
            parent.session_id.as_deref() == Some(session)
                && parent.requester_did.as_deref() == Some(author)
                && parent.lifecycle_state == Some(RequestLifecycleState::Completed),
            "parent worker-start request changed its author, session, or result"
        );
        let mut sessions = Vec::new();
        for worker in &self.workers {
            let child = requests
                .iter()
                .find(|request| {
                    request.agent_did.as_deref() == Some(worker.binding.agent_did.as_str())
                        && request.caused_by_parent_request_doc_id == parent.doc_id
                        && request
                            .content
                            .as_deref()
                            .is_some_and(|content| content.ends_with(worker.prompt.as_str()))
                })
                .with_context(|| {
                    format!("{} was not started with its supplied prompt", worker.title)
                })?;
            gents_store::identity::request_receipt::verify_request_receipt_signature(child)?;
            let child_session = child.session_id.as_deref().context("worker session")?;
            ensure!(
                child.lifecycle_state == Some(RequestLifecycleState::Completed)
                    && child.caused_by_parent_request_id.as_deref()
                        == Some(parent.request_id.as_str())
                    && child.caused_by_parent_tool_call_doc_id.is_some()
                    && child_session != session
                    && !sessions.iter().any(|session| session == child_session),
                "worker lost its separate session, signed parentage, or completion"
            );
            sessions.push(child_session.to_owned());
        }
        let ui: Value = serde_json::from_slice(
            &tokio::fs::read(root.join("editor-related-work-ui.json")).await?,
        )?;
        ensure!(
            ui["concurrentWorkers"] == 2
                && ui["independentlyObservedCompletion"] == true
                && ui["reloadPreservedRowsAndMarkers"] == true
                && ui["separatePanelVerified"] == true
                && ui["parentDraftPreserved"] == true
                && ui["separateWorkerDraftPreserved"] == true,
            "the editor did not preserve independently observed worker state and drafts"
        );
        let draft = ui["draft"].as_str().context("retained parent draft")?;
        let worker_draft = ui["workerDraft"]
            .as_str()
            .context("retained worker draft")?;
        ensure!(
            requests.iter().all(|request| {
                request.content.as_deref() != Some(draft)
                    && request.content.as_deref() != Some(worker_draft)
            }),
            "an unsent editor draft was submitted"
        );
        Ok(
            json!({"parentRequest":parent.doc_id,"relatedSessions":sessions,"signedAncestryVerified":true,"ui":ui}),
        )
    }
}

fn append(document: &mut Value, collection: &str, value: Value) {
    if !document[collection].is_array() {
        document[collection] = json!([]);
    }
    document[collection].as_array_mut().unwrap().push(value);
}
