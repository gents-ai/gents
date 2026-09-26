use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::output::TerminalOutput;
use gents_protocol::request_admission::RequestPurpose;
use tokio::sync::watch;

use super::BehaviorDaemon;
use crate::admission::{self, AdmissionCallContext, CallKind};
use crate::lifecycle::{ClaimOutcome, RequestLifecycle, RequestTerminalOutcome, TerminalizeResult};
use crate::session;
use crate::streaming::DefraStreamWriter;
use crate::watcher::AgentRequest;

const RECENT_TITLE_LIMIT: usize = 5;
const GENERATED_TITLE_MAX_WORDS: usize = 5;
const GENERATED_TITLE_MAX_LEN: usize = 48;
const TITLE_GENERATION_MAX_ATTEMPTS: i64 = 2;
const TITLE_GENERATION_TIMEOUT_SECS: u64 = 10;
const TITLE_GENERATION_PREAMBLE: &str = "Generate concise conversation titles. Return only a lowercase hyphenated 3-5 word title. Never call tools. Never explain.";

struct TitleTask<M: rig::completion::CompletionModel> {
    node: Arc<EmbeddedNode>,
    behavior: Arc<crate::config::ResolvedBehavior>,
    provider_family: Option<String>,
    model: Arc<M>,
    verifier: crate::request_admission::AgentRequestAdmissionVerifier,
    capture_factory: Option<crate::rendered_request::RenderedRequestCaptureFactory>,
}

enum TitleResult {
    Generated {
        title: String,
        parent_requester_did: Option<String>,
    },
    Skipped,
    Interrupted,
}

impl<M: rig::completion::CompletionModel + 'static> BehaviorDaemon<M> {
    fn title_task(&self) -> TitleTask<M> {
        TitleTask {
            node: Arc::clone(&self.node),
            behavior: Arc::clone(&self.behavior),
            provider_family: self.provider_family.clone(),
            model: Arc::clone(&self.model),
            verifier: self.request_admission.clone(),
            capture_factory: self.rendered_request_capture_factory.clone(),
        }
    }

    pub(super) fn spawn_conversation_title_generation(&self, request: &AgentRequest) {
        // A generated title is optional presentation metadata, not part of the
        // requested agent result. Budgeted requests therefore skip this
        // out-of-band provider call instead of giving it a second allowance or
        // racing it against the response loop's request-wide ledger.
        if !title_generation_allowed(request.max_total_tokens) {
            tracing::debug!(
                request_id = %request.request_id,
                "skipping generated title for an aggregate-token-budgeted request"
            );
            return;
        }
        let node = Arc::clone(&self.node);
        let parent = request.clone();
        tokio::spawn(Box::pin(async move {
            let result: Result<()> = async {
                if !session::session_needs_generated_title(
                    node.as_ref(),
                    &parent.agent_did,
                    parent.requester_did.as_deref(),
                    &parent.session_id,
                )
                .await?
                {
                    return Ok(());
                }
                crate::lifecycle::materialize::write_pending_title_request(
                    node.as_ref(),
                    &parent,
                    parent.content.clone(),
                )
                .await?;
                Ok(())
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(
                    error = %error,
                    "failed to create owned conversation title request"
                );
            }
        }));
    }

    pub(super) fn spawn_title_audit_request(
        &self,
        request: AgentRequest,
        shutdown: watch::Receiver<bool>,
    ) {
        let task = self.title_task();
        tokio::spawn(Box::pin(async move {
            if let Err(error) = task.run(request, shutdown).await {
                tracing::warn!(%error, "failed to resume owned conversation title request");
            }
        }));
    }
}

fn title_generation_allowed(max_total_tokens: Option<i64>) -> bool {
    max_total_tokens.is_none()
}

impl<M: rig::completion::CompletionModel + 'static> TitleTask<M> {
    async fn run(self, request: AgentRequest, shutdown: watch::Receiver<bool>) -> Result<()> {
        anyhow::ensure!(
            request.purpose == RequestPurpose::TitleAudit,
            "expected title audit request"
        );
        let Some(request) = super::verify_request_at_claim_boundary(
            &self.verifier,
            Arc::clone(&self.node),
            &self.behavior.behavior_id,
            request,
        )
        .await
        else {
            return Ok(());
        };
        let origin =
            crate::lifecycle::ExecutionOrigin::from_persisted(request.execution_origin.as_deref())
                .context("admitted title request is missing execution origin")?;
        let mut lifecycle = RequestLifecycle::new_with_execution_binding(
            Arc::clone(&self.node),
            &self.behavior.behavior_id,
            self.behavior.agent_did(),
            request.clone(),
            self.behavior.deadline_duration.as_secs(),
            origin,
            self.behavior.backend_id.clone().unwrap_or_default(),
        );
        lifecycle.set_execution_lease_duration(self.behavior.stream_liveness_timeout);
        lifecycle.set_configured_max_total_tokens(self.behavior.max_total_tokens);
        match lifecycle.claim_with_identity().await {
            Ok(ClaimOutcome::Claimed) => {}
            Ok(ClaimOutcome::Queued | ClaimOutcome::Interrupted | ClaimOutcome::Expired) => {
                return Ok(())
            }
            Err(error) if crate::lifecycle::is_claim_admission_error(&error) => {
                lifecycle.reject_admission(&error.to_string()).await?;
                return Ok(());
            }
            Err(error) => return Err(error.context("claiming title audit request")),
        }
        let writer = DefraStreamWriter::new(
            Arc::clone(&self.node),
            self.behavior.agent_did(),
            Duration::from_millis(self.behavior.stream_batch_ms),
        );
        let title_request = lifecycle.request().clone();
        let result = self
            .execute(&mut lifecycle, &writer, title_request.clone(), shutdown)
            .await;
        let (outcome, title, reason) = match result {
            Ok(TitleResult::Generated {
                title,
                parent_requester_did,
            }) => (
                RequestTerminalOutcome::Completed,
                Some((title, parent_requester_did)),
                None,
            ),
            Ok(TitleResult::Skipped) => (RequestTerminalOutcome::Completed, None, None),
            Ok(TitleResult::Interrupted) => (RequestTerminalOutcome::Interrupted, None, None),
            Err(error) => (RequestTerminalOutcome::Failed, None, Some(error)),
        };
        let reason_text = reason.as_ref().map(ToString::to_string);
        let terminal = lifecycle
            .terminalize_owned(outcome, TerminalOutput::NoMessage, reason_text.as_deref())
            .await;
        let terminal = match terminal {
            Ok(value) => value,
            Err(terminal_error) => {
                if let Some(error) = reason {
                    return Err(
                        terminal_error.context(format!("title audit failed first: {error:#}"))
                    );
                }
                return Err(terminal_error);
            }
        };
        if let Some(error) = reason {
            return Err(error);
        }
        if terminal == TerminalizeResult::Won {
            if let Some((title, parent_requester_did)) = title {
                if let Err(error) = session::update_session_title_with_source(
                    self.node.as_ref(),
                    &title_request.agent_did,
                    parent_requester_did.as_deref(),
                    &title_request.session_id,
                    &title,
                    gents_protocol::session::SessionTitleSource::Generated,
                )
                .await
                {
                    tracing::warn!(%error, request_id = %title_request.request_id, "title audit completed but optional session title update failed");
                }
            }
        }
        Ok(())
    }

    async fn execute(
        &self,
        lifecycle: &mut RequestLifecycle,
        writer: &DefraStreamWriter,
        request: AgentRequest,
        shutdown: watch::Receiver<bool>,
    ) -> Result<TitleResult> {
        let parent = load_title_parent(&self.node, &request).await?;
        let commit_cid = lifecycle
            .request_commit_cid()
            .context("claimed title request has no exact commit CID")?;
        let generation = lifecycle.execution_generation()?.to_owned();
        let capture_context = crate::rendered_request::context_for_claimed_request(
            &request,
            commit_cid,
            self.behavior.model_name.clone(),
            self.provider_family.clone(),
        );
        let mut capture_scope = crate::rendered_request::scope_from_factory(
            capture_context,
            self.capture_factory.as_ref(),
        )
        .context("title audit has no durable provider capture authority")?;
        Arc::get_mut(&mut capture_scope)
            .context("new title capture scope is already shared")?
            .set_auxiliary_output_sink(writer.auxiliary_output_sink(
                request.clone(),
                generation,
                crate::provider_input::ProviderInputProfile::resolve(
                    self.behavior.backend_provider_kind,
                    self.behavior.openai_wire_api,
                ),
            ));
        let admission_context = AdmissionCallContext::for_request(
            &request,
            &self.behavior.behavior_id,
            self.behavior.backend_id.clone().unwrap_or_default(),
        );
        admission::scope_request(
            admission_context,
            crate::rendered_request::scope::scope_request(capture_scope, async {
                lifecycle.begin_owned_execution(writer).await?;
                if !session::session_needs_generated_title(
                    self.node.as_ref(),
                    &request.agent_did,
                    parent.requester_did.as_deref(),
                    &request.session_id,
                )
                .await?
                {
                    return Ok(TitleResult::Skipped);
                }
                let recent = session::load_recent_titles_for_agent(
                    self.node.as_ref(),
                    &request.agent_did,
                    &request.session_id,
                    RECENT_TITLE_LIMIT,
                )
                .await
                .unwrap_or_default();
                let prompt = title_generation_prompt(&request.content, &recent);
                let mut config = crate::completion_factory::loop_config(
                    &self.behavior,
                    title_generation_preamble(),
                    0,
                    crate::rendered_request::CaptureScopeKind::Title,
                );
                config.temperature = Some(0.0);
                config.max_tokens = Some(24);
                config.max_turns = 1;
                config.retry_policy =
                    crate::agent::completion_retry::CompletionRetryPolicy::no_retry();
                config.aggregate_token_budget =
                    crate::completion_factory::aggregate_token_budget_for_request(
                        self.node.as_ref(),
                        &request,
                    )
                    .await?;
                self.generate_with_fallback(
                    &request,
                    &prompt,
                    config,
                    parent.requester_did,
                    shutdown,
                )
                .await
            }),
        )
        .await
    }

    async fn generate_with_fallback(
        &self,
        request: &AgentRequest,
        prompt: &str,
        config: crate::agent::loop_stream::LoopConfig,
        parent_requester_did: Option<String>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<TitleResult> {
        let mut last_error = None;
        for attempt in 1..=TITLE_GENERATION_MAX_ATTEMPTS {
            if *shutdown.borrow() {
                return Ok(TitleResult::Interrupted);
            }
            let run = admission::scope_call(
                CallKind::OneOff,
                attempt,
                crate::agent::loop_stream::run_loop_to_text::<M>(
                    (*self.model).clone(),
                    crate::llm::message::Message::user(prompt.to_string()),
                    Vec::new(),
                    Arc::new(Vec::new()),
                    config.clone(),
                ),
            );
            let result = tokio::select! {
                biased;
                _ = async {
                    let _ = shutdown.wait_for(|value| *value).await;
                } => {
                    crate::rendered_request::scope::flush_received_auxiliary_partial().await?;
                    return Ok(TitleResult::Interrupted);
                }
                result = tokio::time::timeout(Duration::from_secs(TITLE_GENERATION_TIMEOUT_SECS), run) => result,
            };
            match result {
                Ok(Ok(raw)) => {
                    return Ok(TitleResult::Generated {
                        title: sanitize_generated_title(&raw, &request.content),
                        parent_requester_did,
                    })
                }
                Ok(Err(error)) => {
                    crate::rendered_request::scope::flush_received_auxiliary_partial().await?;
                    if error
                        .downcast_ref::<crate::agent::loop_stream::OneShotProviderFailure>()
                        .is_none()
                    {
                        return Err(error.context("title audit provider/output invariant failed"));
                    }
                    last_error = Some(error);
                }
                Err(_) => {
                    crate::rendered_request::scope::flush_received_auxiliary_partial().await?;
                    last_error = Some(anyhow::anyhow!(
                        "title inference timed out after {}s",
                        TITLE_GENERATION_TIMEOUT_SECS
                    ));
                }
            }
            tracing::warn!(request_id = %request.request_id, attempt, error = %last_error.as_ref().expect("failed attempt has error"), "title inference failed");
        }
        let fallback = sanitize_generated_title("", &request.content);
        tracing::info!(request_id = %request.request_id, title = %fallback, error = ?last_error.map(|error| error.to_string()), "using fallback conversation title after provider failures");
        Ok(TitleResult::Generated {
            title: fallback,
            parent_requester_did,
        })
    }
}

async fn load_title_parent(node: &EmbeddedNode, title: &AgentRequest) -> Result<AgentRequest> {
    let parent_doc_id = title
        .caused_by_parent_request_doc_id
        .as_deref()
        .context("title request lacks parent document")?;
    let escaped = crate::graphql::escape_graphql_string(parent_doc_id);
    let query = format!(
        "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{escaped}\" }} }}, limit: 1) {{ {} }} }}",
        crate::request_admission::SIGNED_REQUEST_FIELDS
    );
    let response =
        crate::graphql::graphql_with_transaction_retry(node, &query, "load title parent receipt")
            .await?;
    let row: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&response, "AgentRequest")?
            .context("title parent request disappeared")?;
    crate::request_admission::verify_request_receipt_signature(&row)?;
    let parent = AgentRequest::try_from(row)?;
    anyhow::ensure!(
        parent.purpose == RequestPurpose::Normal
            && parent.doc_id == parent_doc_id
            && title.caused_by_parent_request_id.as_deref() == Some(parent.request_id.as_str())
            && parent.agent_did == title.agent_did
            && parent.session_id == title.session_id
            && parent.behavior_id == title.behavior_id,
        "title parent receipt does not match signed provenance"
    );
    Ok(parent)
}

pub(super) fn title_generation_preamble() -> String {
    TITLE_GENERATION_PREAMBLE.to_string()
}

fn title_generation_prompt(request_content: &str, recent_titles: &[String]) -> String {
    let request_excerpt = truncate_for_title_prompt(request_content, 600);
    let recent = if recent_titles.is_empty() {
        "none".to_string()
    } else {
        recent_titles
            .iter()
            .take(RECENT_TITLE_LIMIT)
            .map(|title| format!("- {title}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "Generate a concise session title for this conversation.\n\
Return only the title.\n\
Constraints:\n\
- 3-5 words\n\
- lowercase\n\
- hyphenated\n\
- no punctuation except hyphens\n\
- no quotes\n\
- avoid repeating a recent title exactly\n\n\
Recent session titles:\n{recent}\n\n\
First user request:\n{request_excerpt}"
    )
}

fn truncate_for_title_prompt(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    normalized.chars().take(max_chars).collect()
}

fn sanitize_generated_title(raw_title: &str, fallback_source: &str) -> String {
    let first_line = raw_title
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .to_ascii_lowercase();

    let mut words = first_line
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .flat_map(|part| part.split(['-', '_']))
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .take(GENERATED_TITLE_MAX_WORDS)
        .map(str::to_string)
        .collect::<Vec<_>>();

    if words.is_empty() {
        words = fallback_words(fallback_source);
    }

    let mut title = words.join("-");
    if title.len() > GENERATED_TITLE_MAX_LEN {
        title.truncate(GENERATED_TITLE_MAX_LEN);
        title = title.trim_matches('-').to_string();
    }

    if title.is_empty() {
        "conversation".to_string()
    } else {
        title
    }
}

fn fallback_words(source: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "a", "about", "agent", "amy", "an", "and", "are", "by", "can", "desktop", "do", "for",
        "give", "hello", "help", "hey", "how", "i", "in", "is", "it", "its", "me", "model", "of",
        "on", "please", "s", "tell", "that", "thats", "the", "think", "this", "to", "used", "via",
        "what", "with", "works", "you",
    ];

    let mut words = source
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .filter(|word| !STOPWORDS.contains(word))
        .take(GENERATED_TITLE_MAX_WORDS)
        .map(str::to_string)
        .collect::<Vec<_>>();

    if words.is_empty() {
        words.push("conversation".to_string());
    }

    words
}

#[cfg(test)]
mod owned_tests;

#[cfg(test)]
mod tests {
    use super::{sanitize_generated_title, title_generation_allowed};

    #[test]
    fn budgeted_requests_do_not_dispatch_optional_title_inference() {
        assert!(title_generation_allowed(None));
        assert!(!title_generation_allowed(Some(100_000)));
    }

    #[test]
    fn sanitize_generated_title_cases() {
        for (raw_title, fallback_source, expected) in [
            (
                "\"Agent Desktop Debugging Redux\"",
                "fallback text",
                "agent-desktop-debugging-redux",
            ),
            (
                "",
                "please inspect p2p request model",
                "inspect-p2p-request",
            ),
            (
                "",
                "document-based request model that's used by this agent",
                "document-based-request",
            ),
        ] {
            assert_eq!(
                sanitize_generated_title(raw_title, fallback_source),
                expected,
                "unexpected title for raw {raw_title:?} with fallback {fallback_source:?}"
            );
        }
    }
}
