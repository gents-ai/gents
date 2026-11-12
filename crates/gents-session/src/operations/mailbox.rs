//! Lean `Mailbox.Operations`: the one read of served items, the one dismissal
//! and the one reply admission.

use std::cmp::Ordering;

use chrono::Utc;
use gents_documents::mailbox_write_decl::{MailboxAction, MailboxKind};
use gents_protocol::mailbox_question::{
    question_requires_reply_receipt, question_source_hash, MailboxQuestion, MailboxQuestionAnswer,
    MailboxQuestionReply,
};
use gents_protocol::operations::mailbox::{
    MailboxDismissDecision, MailboxListInput, MailboxListScope, MailboxOperationError,
    MailboxReply, MailboxReplyDecision, MailboxReplyKind, MailboxReplyRoute, MailboxReplySource,
    PreparedMailboxReply,
};
use gents_protocol::row::MAILBOX_COLLECTION;
use gents_store::config_client::{ConfigAccess, ConfigClientError, TransactionFailure};
use gents_store::graphql::{rows, single_mutation_document};

use crate::session::mailbox::{MailboxItem, MailboxStatus, MAILBOX_FIELDS};

type Result<T, E = MailboxOperationError> = std::result::Result<T, E>;

/// Lean `Mailbox.Operations.serves`.
pub fn serves(scope: MailboxListScope, status: MailboxStatus) -> bool {
    match scope {
        MailboxListScope::All => true,
        MailboxListScope::Waiting => !status.is_terminal(),
    }
}

/// `serves` for a stored status; a status this build does not know is served
/// only when everything is.
pub fn serves_stored(scope: MailboxListScope, status: &str) -> bool {
    match MailboxStatus::parse(status) {
        Some(status) => serves(scope, status),
        None => scope == MailboxListScope::All,
    }
}

/// Lean `Mailbox.Operations.precedes`: newest first, then by document identity.
pub fn served_order(left: (&str, &str), right: (&str, &str)) -> Ordering {
    right.0.cmp(left.0).then_with(|| left.1.cmp(right.1))
}

pub async fn list(access: &ConfigAccess, input: &MailboxListInput) -> Result<Vec<MailboxItem>> {
    use gents_protocol::document_query::{
        fields::mailbox_item as fields, Direction, Filter, Query, Selection,
    };
    let mut query = Query::select(Selection::owned(MAILBOX_FIELDS))
        .order_by(fields::CREATED_AT, Direction::Desc);
    if input.scope == MailboxListScope::Waiting {
        query = query.filter(Filter::eq(fields::STATUS, MailboxStatus::Open.as_str()));
    }
    if let Some(node_did) = input.node_did.as_deref() {
        query = query.filter(Filter::eq(fields::NODE_DID, node_did));
    }
    if let Some(requester_did) = input.requester_did.as_deref() {
        query = query.filter(Filter::eq(fields::REQUESTER_DID, requester_did));
    }
    let response = access.read(&query.render()).await.map_err(passthrough)?;
    let mut items = decode_items(&response)?;
    items.retain(|item| serves_stored(input.scope, &item.status));
    items.sort_by(|left, right| {
        served_order(
            (&left.created_at, &left.doc_id),
            (&right.created_at, &right.doc_id),
        )
    });
    Ok(items)
}

pub async fn get(access: &ConfigAccess, doc_id: &str) -> Result<MailboxItem> {
    crate::session::mailbox::load_mailbox_item_with_access(access, doc_id)
        .await
        .map_err(passthrough)?
        .ok_or(MailboxOperationError::ItemMissing)
}

/// A question that closes only through its answer or an explicit skip.
pub fn needs_answer(kind: &str, action: &str, payload: Option<&str>) -> bool {
    kind == MailboxKind::Ask.as_str()
        && MailboxAction::parse(action) == Some(MailboxAction::StartRequest)
        && payload.is_some_and(question_requires_reply_receipt)
}

/// Lean `Mailbox.Operations.dismissDecision`.
pub fn dismiss_decision(status: MailboxStatus, needs_answer: bool) -> MailboxDismissDecision {
    if status.is_terminal() {
        MailboxDismissDecision::AlreadyClosed
    } else if needs_answer {
        MailboxDismissDecision::NeedsAnswer
    } else {
        MailboxDismissDecision::Dismissed
    }
}

/// Dismisses as whoever `access` signs for; ACP decides whether that actor
/// may. A closed item is returned as it stands.
pub async fn dismiss(access: &ConfigAccess, doc_id: &str) -> Result<MailboxItem> {
    if access.actor_did().is_none() {
        return Err(MailboxOperationError::ActorRequired);
    }
    access
        .transact_write("mailbox.dismiss", |txn| {
            Box::pin(async move {
                let response = txn.run(&item_query(doc_id)).await?;
                let item = single(decode_items(&response)?)?;
                let status = MailboxStatus::parse(&item.status)
                    .ok_or_else(|| MailboxOperationError::UnknownStatus(item.status.clone()))?;
                let needs_answer = needs_answer(&item.kind, &item.action, item.payload.as_deref());
                match dismiss_decision(status, needs_answer) {
                    MailboxDismissDecision::AlreadyClosed => return Ok(item),
                    MailboxDismissDecision::NeedsAnswer => {
                        return Err(MailboxOperationError::NeedsAnswer.into())
                    }
                    MailboxDismissDecision::Dismissed => {}
                }
                let now = Utc::now().to_rfc3339();
                let mutation = format!(
                    r#"mutation($id: ID!, $input: MailboxItemMutationInputArg!) {{ update_MailboxItem(
                    filter: {{ _docID: {{ _eq: $id }}, status: {{ _eq: "open" }} }},
                    input: $input
                ) {{ {MAILBOX_FIELDS} }} }}"#,
                );
                let response = txn
                    .execute_client_signed_update(
                        &mutation,
                        &serde_json::json!({"id": doc_id, "input": {
                            "status": MailboxStatus::Dismissed.as_str(),
                            "updated_at": now,
                            "resolved_at": now,
                            "resolved_doc_id": null,
                        }}),
                        gents_protocol::schemas::MAILBOX_ITEM,
                    )
                    .await?;
                let response = defra_node::QueryResponse::success(
                    response.get("data").cloned().unwrap_or_default(),
                );
                let document = single_mutation_document(&response, "update_MailboxItem")?
                    .ok_or(MailboxOperationError::DismissalLost)?;
                Ok(serde_json::from_value(document.clone())?)
            })
        })
        .await
        .map_err(|error: ApplyError| error.into())
}

/// Lean `Mailbox.Operations.replyDecision`.
pub fn reply_decision(
    status: MailboxStatus,
    action: MailboxAction,
    is_question: bool,
    kind: MailboxReplyKind,
    route_matches: bool,
    question_current: bool,
) -> MailboxReplyDecision {
    let answer = kind == MailboxReplyKind::Answer;
    if status != MailboxStatus::Open {
        MailboxReplyDecision::NotOpen
    } else if action != MailboxAction::StartRequest {
        MailboxReplyDecision::NotReplyable
    } else if answer && !is_question {
        MailboxReplyDecision::NotAQuestion
    } else if !route_matches {
        MailboxReplyDecision::RouteDiffers
    } else if answer && !question_current {
        MailboxReplyDecision::QuestionChanged
    } else {
        MailboxReplyDecision::Admitted
    }
}

/// `answer` as written against `source`: a client that built the answer from
/// this very copy of the item binds it to that copy's question.
pub fn answer_observing(
    source: MailboxReplySource<'_>,
    answer: MailboxQuestionAnswer,
) -> Result<MailboxQuestionAnswer> {
    Ok(MailboxQuestionAnswer {
        question_hash: question_hash(source)?,
        ..answer
    })
}

/// Admits `reply` to the item and returns the request to send, on the item's
/// own route. `route` is what the caller addressed, when its surface states it.
pub fn prepare_reply(
    source: MailboxReplySource<'_>,
    reply: MailboxReply,
    route: &MailboxReplyRoute,
) -> Result<PreparedMailboxReply> {
    let status = MailboxStatus::parse(source.status)
        .ok_or_else(|| MailboxOperationError::UnknownStatus(source.status.to_owned()))?;
    let action = MailboxAction::parse(source.action)
        .ok_or_else(|| MailboxOperationError::UnknownAction(source.action.to_owned()))?;
    let is_question = source.kind == MailboxKind::Ask.as_str();
    let answer = match &reply {
        MailboxReply::Text(_) => None,
        MailboxReply::Answer(answer) => Some(answer),
    };
    let question_current = match answer {
        Some(answer) if is_question && action == MailboxAction::StartRequest => {
            let current = question_hash(source)?;
            current.is_none() || answer.question_hash == current
        }
        _ => true,
    };
    reply_decision(
        status,
        action,
        is_question,
        reply.kind(),
        route_matches(source, answer.is_some(), route),
        question_current,
    )
    .admit()?;
    let (content, question_reply) = match reply {
        MailboxReply::Text(text) => (text, None),
        MailboxReply::Answer(answer) => {
            let payload = source
                .payload
                .ok_or(MailboxOperationError::QuestionPayloadMissing)?;
            let content =
                MailboxQuestion::from_payload(payload)?.reply_content(source.title, &answer)?;
            let receipt = question_requires_reply_receipt(payload)
                .then(|| {
                    MailboxQuestionReply::new(
                        source.doc_id,
                        source.title,
                        source.summary,
                        payload,
                        source.request_id,
                        answer,
                    )
                })
                .transpose()?;
            (content, receipt)
        }
    };
    Ok(PreparedMailboxReply {
        mailbox_item_id: source.doc_id.to_owned(),
        node_did: source.target_node_did.to_owned(),
        agent_did: source.target_agent_did.to_owned(),
        session_id: source.session_id.map(ToOwned::to_owned),
        content,
        question_reply,
    })
}

fn route_matches(source: MailboxReplySource<'_>, answer: bool, route: &MailboxReplyRoute) -> bool {
    let stated = |stated: &Option<String>, own: Option<&str>| {
        stated
            .as_deref()
            .is_none_or(|stated| Some(stated.trim()) == own)
    };
    (!answer || source.session_id.is_some())
        && stated(&route.session_id, source.session_id)
        && stated(&route.node_did, Some(source.target_node_did))
        && stated(&route.agent_did, Some(source.target_agent_did))
}

fn question_hash(source: MailboxReplySource<'_>) -> Result<Option<String>> {
    source
        .payload
        .filter(|payload| question_requires_reply_receipt(payload))
        .map(|payload| {
            question_source_hash(source.title, source.summary, payload, source.request_id)
        })
        .transpose()
        .map_err(Into::into)
}

impl<'a> From<&'a MailboxItem> for MailboxReplySource<'a> {
    fn from(item: &'a MailboxItem) -> Self {
        Self {
            doc_id: &item.doc_id,
            status: &item.status,
            kind: &item.kind,
            action: &item.action,
            title: &item.title,
            summary: item.summary.as_deref(),
            payload: item.payload.as_deref(),
            request_id: item.request_id.as_deref(),
            session_id: item.session_id.as_deref(),
            target_node_did: &item.target_node_did,
            target_agent_did: &item.target_agent_did,
        }
    }
}

fn item_query(doc_id: &str) -> gents_protocol::document_query::Rendered {
    use gents_protocol::document_query::{
        fields::mailbox_item as fields, Filter, Query, Selection,
    };
    Query::select(Selection::owned(MAILBOX_FIELDS))
        .filter(Filter::eq(fields::DOC_ID, doc_id))
        .limit(2)
        .render()
}

fn decode_items(response: &serde_json::Value) -> Result<Vec<MailboxItem>> {
    let response = defra_node::QueryResponse::success(
        response.get("data").cloned().unwrap_or_default(),
    );
    rows(&response, MAILBOX_COLLECTION).map_err(passthrough)
}

fn single(mut items: Vec<MailboxItem>) -> Result<MailboxItem> {
    if items.len() > 1 {
        return Err(MailboxOperationError::ItemAmbiguous);
    }
    items.pop().ok_or(MailboxOperationError::ItemMissing)
}

fn passthrough<E>(error: E) -> MailboxOperationError
where
    E: Into<anyhow::Error>,
{
    MailboxOperationError::Passthrough(
        error
            .into()
            .reallocate_into_boxed_dyn_error_without_backtrace(),
    )
}

enum ApplyError {
    Operation(MailboxOperationError),
    Other(anyhow::Error),
}

impl From<MailboxOperationError> for ApplyError {
    fn from(error: MailboxOperationError) -> Self {
        match error {
            MailboxOperationError::Passthrough(error) => Self::Other(anyhow::Error::from_boxed(error)),
            error => Self::Operation(error),
        }
    }
}

impl From<anyhow::Error> for ApplyError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

impl From<ConfigClientError> for ApplyError {
    fn from(error: ConfigClientError) -> Self {
        Self::Other(error.into())
    }
}

impl From<gents_store::graphql::GraphqlResponseError> for ApplyError {
    fn from(error: gents_store::graphql::GraphqlResponseError) -> Self {
        Self::Other(error.into())
    }
}

impl From<serde_json::Error> for ApplyError {
    fn from(error: serde_json::Error) -> Self {
        Self::Other(error.into())
    }
}

impl From<ApplyError> for MailboxOperationError {
    fn from(error: ApplyError) -> Self {
        match error {
            ApplyError::Operation(error) => error,
            ApplyError::Other(error) => passthrough(error),
        }
    }
}

impl TransactionFailure for ApplyError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Operation(_) => None,
            Self::Other(error) => error.config_client_error(),
        }
    }
}

#[cfg(test)]
mod conformance {
    use super::*;

    #[test]
    fn serves_matches_lean() {
        gents_lean_vocab::families::mailbox_operations_serves::replay(|i| {
            serves(i.list_scope, i.status)
        });
    }

    #[test]
    fn dismiss_decision_matches_lean() {
        gents_lean_vocab::families::mailbox_operations_dismiss_decision::replay(|i| {
            dismiss_decision(i.status, i.needs_answer)
        });
    }

    #[test]
    fn reply_decision_matches_lean() {
        gents_lean_vocab::families::mailbox_operations_reply_decision::replay(|i| {
            reply_decision(
                i.status,
                i.handling,
                i.is_question,
                i.reply_kind,
                i.route_matches,
                i.question_current,
            )
        });
    }
}

#[cfg(test)]
mod pushdown_tests;
