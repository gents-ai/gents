use std::path::Path;

use anyhow::{ensure, Context, Result};
use gents_documents::{DatastoreToolSurfaceDocument, SurfaceToolDecl};
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents_store::graphql::escape_graphql_string;
use serde_json::{json, Value};

const TARGET: &str = "live-question-worker";
const TITLE: &str = "Worker approach";
const CANCEL_TITLE: &str = "Interrupted plan";
const CANCEL_QUESTION: &str = "Should this cancelled task proceed?";
const ORIGINAL_WORKER_PROMPT: &str = "Inspect the parser and propose a focused patch.";
const EDITED_WORKER_PROMPT: &str = "Inspect only the parser and report QUESTION_WORKER_EDGE_CASES.";
const WORKER_TITLE: &str = "Inspect parser edge cases";
const SURFACE: &str = "live-question-mailbox";
const WORKER_CONTEXT: &str = "live-question-worker-context";

pub struct Questions {
    binding: AgentIdentityBinding,
    prompt: String,
    cancel_prompt: String,
}

impl Questions {
    pub async fn prepare(
        root: &Path,
        endpoint: &GraphqlEndpoint,
        node: &str,
        cancellation_gate: Value,
    ) -> Result<Self> {
        let binding = gents_desktop_core::agent_identity::provision(endpoint, node).await?;
        let arguments = json!({
            "title":TITLE,
            "question":{
                "version":3,
                "prompt":"What should the worker inspect?",
                "allow_free_text":true,
                "options":[
                    {"id":"parser","label":"Inspect parser","description":"A focused review with less disruption.","prompt":ORIGINAL_WORKER_PROMPT},
                    {"id":"related","label":"Inspect related modules","description":"A wider comparison before choosing a patch.","prompt":"Compare the parser and related modules."},
                    {"id":"tests","label":"Review test gaps","description":"Gather evidence before changing behavior.","prompt":"Review parser test coverage and report gaps."}
                ]
            }
        });
        let prompt = format!(
            "Ask the user to choose the worker approach by calling file_mailbox_item with these arguments: {arguments}. End this turn after filing the question, without starting a worker or completing the Goal. When the user's signed answer arrives, use its edited worker prompt with agent_new targeting {TARGET}, title {WORKER_TITLE}, then end that turn. On the automatic Goal continuation after the answer, call update_goal with status complete."
        );
        let cancellation = json!({
            "title":CANCEL_TITLE,
            "question":{"version":3,"prompt":CANCEL_QUESTION,"allow_free_text":true,"options":[]}
        });
        let held_prompt = cancellation_gate["prompt"]
            .as_str()
            .context("the cancellation gate's model instructions")?;
        let cancel_prompt = format!(
            "First call file_mailbox_item with these arguments: {cancellation}. After that call has returned, {held_prompt} Do not answer or resolve the filed question."
        );
        tokio::fs::write(
            root.join("editor-questions-fixture.json"),
            serde_json::to_vec(&json!({
                "prompt":prompt,
                "originalWorkerPrompt":ORIGINAL_WORKER_PROMPT,
                "editedWorkerPrompt":EDITED_WORKER_PROMPT,
                "workerTitle":WORKER_TITLE,
                "cancelPrompt":cancel_prompt,
                "cancelQuestion":CANCEL_QUESTION,
                "cancellationGate":cancellation_gate,
            }))?,
        )
        .await?;
        Ok(Self {
            binding,
            prompt,
            cancel_prompt,
        })
    }

    pub fn configure(&self, document: &mut Value, node: &str) -> Result<()> {
        let parent = document["agents"]
            .as_array()
            .and_then(|agents| agents.iter().find(|agent| agent["node_did"] == node))
            .context("selected Agent configuration")?
            .clone();
        let context_id = parent["context_id"]
            .as_str()
            .context("configure the held tool before the question tools")?;
        let context = document["contexts"]
            .as_array_mut()
            .and_then(|contexts| {
                contexts.iter_mut().find(|context| {
                    context["node_did"] == node && context["context_id"] == context_id
                })
            })
            .context("selected held-tool context")?;
        let tools_id = context["tools_id"]
            .as_str()
            .context("selected held-tool tools")?
            .to_owned();
        let previous = context["system_prompt"].as_str().unwrap_or_default();
        context["system_prompt"] = json!(format!(
            "{previous}\nQuestions are asynchronous. Filing a question ends the asking turn without completing its Goal. On a user's Decision on Worker approach, pass the chosen edited prompt to agent_new targeting {TARGET} with title {WORKER_TITLE}; do not ask again. After that answered turn, an automatic continuation completes the same Goal with update_goal status complete. Never complete the Goal while its question is unanswered."
        ));
        let tools = document["tools"]
            .as_array_mut()
            .and_then(|tools| {
                tools
                    .iter_mut()
                    .find(|tools| tools["node_did"] == node && tools["tools_id"] == tools_id)
            })
            .context("selected held-tool configuration")?;
        tools["agents"] = json!({"enabled":true,"target_ids":[TARGET]});
        if !tools["datastore"].is_object() {
            tools["datastore"] = json!({});
        }
        tools["datastore"]["datastore_tool_surface_ids"] = json!([SURFACE]);
        if !tools["built_ins"].is_object() {
            tools["built_ins"] = json!({});
        }
        tools["built_ins"]["enable_goal_tools"] = json!(true);
        let mut worker = parent;
        worker["agent_did"] = json!(self.binding.agent_did);
        worker["creation_nonce"] = json!(self.binding.creation_nonce);
        worker["context_id"] = json!(WORKER_CONTEXT);
        worker["display_name"] = json!(WORKER_TITLE);
        append(document, "agents", worker);
        append(
            document,
            "contexts",
            json!({"node_did":node,"context_id":WORKER_CONTEXT,"system_prompt":"Follow the current user's supplied inspection prompt and return a brief answer. Do not start another worker."}),
        );
        append(
            document,
            "agent_targets",
            json!({"node_did":node,"target_id":TARGET,"name":TARGET,"target_node_did":node,"agent_did":self.binding.agent_did}),
        );
        let surface = DatastoreToolSurfaceDocument {
            surface_id: SURFACE.to_owned(),
            node_did: node.to_owned(),
            display_name: Some("Live editor questions".to_owned()),
            enabled: true,
            entries: Some(vec![SurfaceToolDecl::Create(
                gents_documents::mailbox_write_decl::canonical_mailbox_write_decl(),
            )]),
            created_at: None,
            tags: Vec::new(),
        };
        append(
            document,
            "datastore_tool_surfaces",
            serde_json::to_value(surface)?,
        );
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
        let response = access.execute(&format!(
            "{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}}}) {{{}}} MailboxItem(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}) {{{}}} Goal(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}) {{goal_id status assignment_root_request_doc_id}} }}",
            escape_graphql_string(node), gents::SIGNED_REQUEST_FIELDS,
            escape_graphql_string(node), escape_graphql_string(session), gents_session::session::mailbox::MAILBOX_FIELDS,
            escape_graphql_string(node), escape_graphql_string(session),
        )).await?;
        let requests: Vec<AgentRequestRow> =
            serde_json::from_value(response["data"]["AgentRequest"].clone())?;
        let questions: Vec<gents_session::session::mailbox::MailboxItem> =
            serde_json::from_value(response["data"]["MailboxItem"].clone())?;
        let question = questions
            .iter()
            .find(|question| question.title == TITLE)
            .context("filed worker question")?;
        ensure!(
            question.status == "acted",
            "the selected question was not answered"
        );
        let parent = requests
            .iter()
            .find(|request| request.content.as_deref() == Some(self.prompt.as_str()))
            .context("asking request")?;
        signed_user_turn(parent, session, author, RequestLifecycleState::Completed)?;
        let answer = requests
            .iter()
            .find(|request| {
                request.caused_by_source_doc_id.as_deref() == Some(question.doc_id.as_str())
                    && request
                        .input
                        .as_ref()
                        .is_some_and(|input| input.question_reply.is_some())
            })
            .context("signed answer request")?;
        signed_user_turn(answer, session, author, RequestLifecycleState::Completed)?;
        let evidence = answer
            .input
            .as_ref()
            .and_then(|input| input.question_reply.as_ref())
            .context("signed question answer evidence")?;
        ensure!(
            evidence.question_version == Some(3)
                && evidence.answer.option_ids == ["parser"]
                && evidence.answer.free_text.as_deref() == Some(EDITED_WORKER_PROMPT)
                && question.resolved_doc_id == answer.doc_id,
            "the selected option and edited instructions were not retained"
        );
        evidence.validate_for(
            &question.doc_id,
            &question.title,
            question.summary.as_deref(),
            question
                .payload
                .as_deref()
                .context("filed question payload")?,
            question.request_id.as_deref(),
            answer.content.as_deref().context("signed answer text")?,
        )?;
        let worker = requests
            .iter()
            .find(|request| {
                request.agent_did.as_deref() == Some(self.binding.agent_did.as_str())
                    && request.caused_by_parent_request_doc_id == answer.doc_id
                    && request
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains(EDITED_WORKER_PROMPT))
            })
            .context("worker admitted from the edited answer")?;
        gents_store::identity::request_receipt::verify_request_receipt_signature(worker)?;
        ensure!(
            worker.session_id.as_deref() != Some(session)
                && worker.caused_by_parent_tool_call_doc_id.is_some()
                && worker.lifecycle_state == Some(RequestLifecycleState::Completed),
            "edited worker lost its separate session, tool ancestry, or completion"
        );
        ensure!(
            response["data"]["Goal"]
                .as_array()
                .is_some_and(|goals| goals.iter().any(|goal| goal["status"] == "complete"
                    && goal["assignment_root_request_doc_id"].as_str()
                        == parent.doc_id.as_deref())),
            "the answered Goal did not complete its original assignment"
        );
        let cancelled = requests
            .iter()
            .find(|request| request.content.as_deref() == Some(self.cancel_prompt.as_str()))
            .context("cancelled question request")?;
        signed_user_turn(
            cancelled,
            session,
            author,
            RequestLifecycleState::Interrupted,
        )?;
        let pending = questions
            .iter()
            .find(|question| question.title == CANCEL_TITLE)
            .context("cancelled turn's question")?;
        ensure!(
            pending.status == "open"
                && pending.resolved_doc_id.is_none()
                && pending.request_id.as_deref() == Some(cancelled.request_id.as_str()),
            "Stop consumed or replaced the unanswered question"
        );
        ensure!(
            requests
                .iter()
                .all(|request| request.caused_by_source_doc_id.as_deref()
                    != Some(pending.doc_id.as_str())
                    && request.caused_by_parent_request_doc_id != cancelled.doc_id),
            "the cancelled question dispatched additional work"
        );
        let ui: Value =
            serde_json::from_slice(&tokio::fs::read(root.join("editor-questions-ui.json")).await?)?;
        ensure!(
            ui["draftPreserved"] == true
                && ui["editedPromptRestored"] == true
                && ui["requestingWorkUsedAnswer"] == true
                && ui["automaticGoalContinuation"] == true
                && ui["cancellationRetainedAcrossReload"] == true,
            "question UI evidence is incomplete"
        );
        Ok(
            json!({"question":question.doc_id,"answer":answer.doc_id,"worker":worker.doc_id,
            "cancelledQuestion":pending.doc_id,"signedAnswerRetained":true,"editedPromptDelivered":true,
            "goalCompleted":true,"cancelledQuestionRemainsOpen":true,"ui":ui}),
        )
    }
}

fn signed_user_turn(
    request: &AgentRequestRow,
    session: &str,
    author: &str,
    state: RequestLifecycleState,
) -> Result<()> {
    gents_store::identity::request_receipt::verify_request_receipt_signature(request)?;
    ensure!(
        request.requester_did.as_deref() == Some(author)
            && request.session_id.as_deref() == Some(session)
            && request.lifecycle_state == Some(state),
        "signed question turn changed author, session, or terminal outcome"
    );
    Ok(())
}

fn append(document: &mut Value, collection: &str, value: Value) {
    if !document[collection].is_array() {
        document[collection] = json!([]);
    }
    document[collection].as_array_mut().unwrap().push(value);
}
