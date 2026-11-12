//! The Claude request context of Gents requests: where the request's session
//! came from, what the call is for and which agent makes it, resolved from the
//! sessions' verified creation receipts and carried as the reserved
//! `_gents_claude_request` parameter that the Messages planner reads.

use std::future::Future;

use anyhow::{Context, Result};
use gents_claude::metadata;
use gents_claude::origin::{Cause, Kind, Origin};
use gents_loop::claude_messages_body::{
    ClaudeAgentIdentity, ClaudeRequestContext, CLAUDE_REQUEST_PARAM,
};
use gents_loop::loop_stream::LoopConfig;
use gents_protocol::session::{SessionCreationCause, SessionScope};

use crate::AgentRequest;
use gents_inference::resolved::ResolvedAgent;
use gents_session::request::lifecycle::ExecutionOrigin;
use gents_session::session::origin::{self, CreationOrigin};
use gents_store::config_client::ConfigAccess;

/// Longest starter chain read to find the session a conversation started from.
const MAX_CHAIN_HOPS: usize = 32;

/// How a session was created and where its chain of starters began.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionOrigin {
    pub(crate) cause: Cause,
    pub(crate) root_client: bool,
    /// The main session of the chain: the session itself unless it runs as a subagent, whose
    /// requests carry their main session's id as Claude Code's subagents do.
    pub(crate) main_session_id: String,
    /// The session that opened this one, when that session itself runs as a subagent.
    pub(crate) parent_subagent_session_id: Option<String>,
    /// The main session's request whose turn opened the chain. Claude Code's subagent prompts
    /// carry no prompt id of their own and send their parent's (`parentPromptId`), which a
    /// nested subagent inherits, so every subagent of the chain sends this one.
    pub(crate) main_prompt_doc_id: Option<String>,
}

fn cause_of(cause: SessionCreationCause) -> Cause {
    match cause {
        SessionCreationCause::AgentNew => Cause::AgentNew,
        SessionCreationCause::TaskTrigger => Cause::TaskTrigger,
        SessionCreationCause::PluginJob => Cause::PluginJob,
    }
}

/// Walks the verified starter chain of `scope` to the main session it runs under.
pub(crate) async fn session_origin(
    access: &ConfigAccess,
    scope: &SessionScope,
) -> Result<SessionOrigin> {
    walk(scope, |scope| async move {
        origin::verified_creation_origin(access, &scope).await
    })
    .await
}

/// [`session_origin`] over `creation`, the verified creation of a session scope.
async fn walk<F, Fut>(scope: &SessionScope, mut creation: F) -> Result<SessionOrigin>
where
    F: FnMut(SessionScope) -> Fut,
    Fut: Future<Output = Result<Option<CreationOrigin>>>,
{
    let Some(first) = creation(scope.clone()).await? else {
        return Ok(SessionOrigin {
            cause: Cause::Client,
            root_client: true,
            main_session_id: scope.session_id.clone(),
            parent_subagent_session_id: None,
            main_prompt_doc_id: None,
        });
    };
    let cause = cause_of(first.cause);
    let mut parent_subagent_session_id = None;
    let mut main_prompt_doc_id = None;
    let (mut session_id, mut current) = (scope.session_id.clone(), first);
    for hop in 0..MAX_CHAIN_HOPS {
        let link = match (cause_of(current.cause).subagent(), current.starter) {
            (true, Some(link)) => link,
            _ => {
                return Ok(SessionOrigin {
                    cause,
                    root_client: false,
                    main_session_id: session_id,
                    parent_subagent_session_id,
                    main_prompt_doc_id,
                });
            }
        };
        main_prompt_doc_id = Some(link.cause_request_doc_id.clone());
        let Some(next) = creation(link.scope.clone()).await? else {
            return Ok(SessionOrigin {
                cause,
                root_client: true,
                main_session_id: link.scope.session_id,
                parent_subagent_session_id,
                main_prompt_doc_id,
            });
        };
        if hop == 0 && cause_of(next.cause).subagent() {
            parent_subagent_session_id = Some(link.scope.session_id.clone());
        }
        (session_id, current) = (link.scope.session_id, next);
    }
    tracing::warn!(
        session_id = %scope.session_id,
        hops = MAX_CHAIN_HOPS,
        "the session's starter chain is deeper than the walk reads, so its Claude requests are \
         sent as background work of the deepest session read"
    );
    Ok(SessionOrigin {
        cause,
        root_client: false,
        main_session_id: session_id,
        parent_subagent_session_id,
        main_prompt_doc_id,
    })
}

/// The origin of a request of `kind` whose session resolved to `session`.
pub(crate) fn origin_of(
    session: &SessionOrigin,
    scheduled: bool,
    kind: Kind,
    agent: &ResolvedAgent,
) -> Origin {
    Origin {
        cause: session.cause,
        root_client: session.root_client,
        scheduled,
        kind,
        appends_system_prompt: !agent.system_prompt.trim().is_empty(),
    }
}

/// The context of a request of `kind`, or `None` when the agent's backend is not Claude's.
pub(crate) async fn for_request(
    access: &ConfigAccess,
    agent: &ResolvedAgent,
    request: &AgentRequest,
    kind: Kind,
) -> Result<Option<ClaudeRequestContext>> {
    if !agent.backend_provider_kind.uses_messages_wire() {
        return Ok(None);
    }
    let scope = SessionScope::of_request(request);
    let session = session_origin(access, &scope)
        .await
        .context("resolving the Claude request origin from the session's creation receipt")?;
    let scheduled = ExecutionOrigin::from_persisted(request.execution_origin.as_deref())
        .context("reading the request's execution origin")?
        == ExecutionOrigin::Scheduled;
    let origin = origin_of(&session, scheduled, kind, agent);
    Ok(Some(ClaudeRequestContext {
        origin,
        session_key: session.main_session_id.clone(),
        prompt_key: session
            .main_prompt_doc_id
            .as_ref()
            .filter(|_| origin.subagent())
            .unwrap_or(&request.doc_id)
            .clone(),
        parent_session_id: None,
        agent: origin.subagent().then(|| ClaudeAgentIdentity {
            agent_did: metadata::agent_did(request.session_id.as_bytes()),
            parent_agent_did: session
                .parent_subagent_session_id
                .as_deref()
                .map(|session_id| metadata::agent_did(session_id.as_bytes())),
        }),
    }))
}

/// The context of a one-shot run: no session exists, so the run is a client call with a
/// session and prompt of its own.
pub(crate) fn for_one_shot(agent: &ResolvedAgent) -> Option<ClaudeRequestContext> {
    agent.backend_provider_kind.uses_messages_wire().then(|| {
        let run = uuid::Uuid::new_v4().to_string();
        ClaudeRequestContext {
            origin: origin_of(
                &SessionOrigin {
                    cause: Cause::Client,
                    root_client: true,
                    main_session_id: run.clone(),
                    parent_subagent_session_id: None,
                    main_prompt_doc_id: None,
                },
                false,
                Kind::OneShot,
                agent,
            ),
            session_key: run.clone(),
            prompt_key: run,
            parent_session_id: None,
            agent: None,
        }
    })
}

/// The provider parameters of a compaction summary call made on behalf of `context`'s request.
pub(crate) fn compaction_params(context: &ClaudeRequestContext) -> Result<serde_json::Value> {
    let mut context = context.clone();
    context.origin.kind = Kind::Compaction;
    with_context(None, &context)
}

/// Adds `context` to the provider parameters of `config`.
pub(crate) fn apply(config: &mut LoopConfig, context: Option<&ClaudeRequestContext>) -> Result<()> {
    if let Some(context) = context {
        config.additional_params = Some(with_context(config.additional_params.take(), context)?);
    }
    Ok(())
}

/// `params` with `context` under the reserved parameter.
pub(crate) fn with_context(
    params: Option<serde_json::Value>,
    context: &ClaudeRequestContext,
) -> Result<serde_json::Value> {
    let mut params = params
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    params[CLAUDE_REQUEST_PARAM] = context.to_param()?;
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_lean_vocab::families::runtime_claude_session_lineage_session_origin as family;
    use gents_session::session::origin::SessionLink;

    fn scope(session: u64) -> SessionScope {
        SessionScope {
            node_did: "did:test:node".into(),
            session_id: session.to_string(),
            requester_did: Some("did:test:node".into()),
        }
    }

    #[tokio::test]
    async fn generated_session_origin_cases_replay_the_verified_walk() {
        for case in family::cases::<Cause>() {
            let input = &case.input;
            let actual = walk(&scope(input.session), |scope| {
                let row = input
                    .rows
                    .iter()
                    .find(|row| row.session.to_string() == scope.session_id);
                std::future::ready(Ok(row.map(|row| CreationOrigin {
                    cause: match row.cause {
                        Cause::AgentNew => SessionCreationCause::AgentNew,
                        Cause::TaskTrigger => SessionCreationCause::TaskTrigger,
                        Cause::PluginJob => SessionCreationCause::PluginJob,
                        Cause::Client => panic!("client sessions have no receipt"),
                    },
                    starter: row.starter.map(|session| SessionLink {
                        scope: self::scope(session),
                        cause_request_doc_id: session.to_string(),
                    }),
                })))
            })
            .await
            .unwrap();
            case.check(family::Observation {
                cause: actual.cause,
                root_client: actual.root_client,
                main_session: actual.main_session_id.parse().unwrap(),
                parent_subagent: actual
                    .parent_subagent_session_id
                    .map(|id| id.parse().unwrap()),
                main_prompt: actual.main_prompt_doc_id.map(|id| id.parse().unwrap()),
            });
        }
    }
}
