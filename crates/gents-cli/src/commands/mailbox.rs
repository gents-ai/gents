use anyhow::{bail, ensure, Context, Result};
use gents::session::mailbox::{
    MailboxAction, MailboxItem, MailboxKind, MailboxStatus, MAILBOX_FIELDS,
};
use gents::store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents::store::graphql::escape_graphql_string;
use gents_protocol::mailbox_question::{
    question_requires_reply_receipt, question_source_hash, MailboxQuestion, MailboxQuestionAnswer,
    MailboxQuestionReply,
};
use gents_protocol::request_input::RequestInput;

use crate::cli::args::{MailboxCommand, MailboxItemArgs, MailboxListArgs, MailboxReplyArgs};
use crate::cli::output_format::OutputFormat;
use crate::{print_json, resolve_config_access, SubmittedRequest};

pub(crate) async fn dispatch(command: MailboxCommand) -> Result<()> {
    match command {
        MailboxCommand::List(args) => list(args).await,
        MailboxCommand::Show(args) => show(args).await,
        MailboxCommand::Dismiss(args) => dismiss(args).await,
        MailboxCommand::Reply(args) => reply(args).await,
    }
}

async fn reply(args: MailboxReplyArgs) -> Result<()> {
    args.item
        .output
        .ensure_supported("automation mailbox reply", &[OutputFormat::Json])?;
    let home = args.item.access.home.as_deref();
    let graphql = crate::resolve_graphql_endpoint(args.item.access.graphql.as_deref(), home)?;
    let access = ConfigAccess::Graphql(graphql.clone());
    let item = load_item(&access, &args.item.doc_id)
        .await?
        .context("MailboxItem not found")?;
    let submitted = reply_to(&graphql, &item, Reply::Text(args.message)).await?;
    print_json(&serde_json::json!({
        "request_id": submitted.request_id,
        "request_doc_id": submitted.request_doc_id,
        "session_id": submitted.session_id,
        "mailbox_item_id": item.doc_id,
    }))
}

async fn list(args: MailboxListArgs) -> Result<()> {
    args.output
        .ensure_supported("automation mailbox list", &[OutputFormat::Json])?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    print_json(&serde_json::to_value(
        served_items(&access, args.all).await?,
    )?)
}

/// The items this actor may read, newest first, open ones unless `all`.
pub(crate) async fn served_items(access: &ConfigAccess, all: bool) -> Result<Vec<MailboxItem>> {
    let filter = if all {
        ""
    } else {
        r#"filter: { status: { _eq: "open" } },"#
    };
    decode_rows(
        access
            .execute(&format!(
                "{{ MailboxItem({filter} order: {{ created_at: DESC }}) {{ {MAILBOX_FIELDS} }} }}"
            ))
            .await?,
    )
}

async fn show(args: MailboxItemArgs) -> Result<()> {
    args.output
        .ensure_supported("automation mailbox show", &[OutputFormat::Json])?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let item = load_item(&access, &args.doc_id)
        .await?
        .context("MailboxItem not found")?;
    print_json(&serde_json::to_value(item)?)
}

async fn dismiss(args: MailboxItemArgs) -> Result<()> {
    args.output
        .ensure_supported("automation mailbox dismiss", &[OutputFormat::Json])?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let item = gents::session::mailbox::dismiss_mailbox_item_on(&access, &args.doc_id).await?;
    print_json(&serde_json::to_value(item)?)
}

/// A version 2 or later question, which only an answer or an explicit skip closes.
pub(crate) fn needs_answer(item: &MailboxItem) -> bool {
    item.kind == MailboxKind::Ask.as_str()
        && item.parsed_action() == Some(MailboxAction::StartRequest)
        && item
            .payload
            .as_deref()
            .is_some_and(question_requires_reply_receipt)
}

/// Replies as a request caused by the item, on its session and to its agent.
pub(crate) async fn reply_to(
    graphql: &GraphqlEndpoint,
    item: &MailboxItem,
    reply: Reply,
) -> Result<SubmittedRequest> {
    ensure!(
        item.parsed_status() == Some(MailboxStatus::Open),
        "MailboxItem is no longer open"
    );
    ensure!(
        item.parsed_action() == Some(MailboxAction::StartRequest),
        "MailboxItem does not accept request replies"
    );
    let (content, input) = match reply {
        Reply::Text(text) => (text, None),
        Reply::Answer(answer) => {
            let (content, question_reply) = question_reply(item, &answer)?;
            let input = RequestInput {
                question_reply,
                ..Default::default()
            };
            (content, Some(input))
        }
    };
    crate::create_agent_request(
        graphql,
        &item.target_node_did,
        &content,
        item.session_id.as_deref(),
        Some(&item.target_agent_did),
        crate::RequestSubmitOptions {
            caused_by_source_doc_id: Some(item.doc_id.clone()),
            input,
            ..Default::default()
        },
    )
    .await
}

pub(crate) async fn load_item(access: &ConfigAccess, doc_id: &str) -> Result<Option<MailboxItem>> {
    let response = access
        .execute(&format!(
            r#"{{ MailboxItem(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ {MAILBOX_FIELDS} }} }}"#,
            escape_graphql_string(doc_id)
        ))
        .await?;
    let mut items = decode_rows(response)?;
    if items.len() > 1 {
        bail!("MailboxItem _docID lookup returned more than one row");
    }
    Ok(items.pop())
}

fn decode_rows(response: serde_json::Value) -> Result<Vec<MailboxItem>> {
    serde_json::from_value(
        response
            .pointer("/data/MailboxItem")
            .cloned()
            .context("mailbox query omitted rows")?,
    )
    .context("decoding MailboxItem rows")
}

/// How a mailbox item is answered: plain text, or a question's answer.
#[derive(Debug, PartialEq)]
pub(crate) enum Reply {
    Text(String),
    Answer(MailboxQuestionAnswer),
}

/// The reply's content, and the receipt a version 2 or later question requires, bound to the
/// question as `item` holds it.
pub(crate) fn question_reply(
    item: &MailboxItem,
    answer: &MailboxQuestionAnswer,
) -> Result<(String, Option<MailboxQuestionReply>)> {
    let payload = item
        .payload
        .as_deref()
        .context("question payload is unavailable")?;
    let content = MailboxQuestion::from_payload(payload)?.reply_content(&item.title, answer)?;
    if !question_requires_reply_receipt(payload) {
        return Ok((content, None));
    }
    let (summary, request) = (item.summary.as_deref(), item.request_id.as_deref());
    let answer = MailboxQuestionAnswer {
        question_hash: Some(question_source_hash(
            &item.title,
            summary,
            payload,
            request,
        )?),
        ..answer.clone()
    };
    let receipt =
        MailboxQuestionReply::new(&item.doc_id, &item.title, summary, payload, request, answer)?;
    Ok((content, Some(receipt)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// An open question on session `s1`, asked with `payload`.
    fn question(payload: serde_json::Value) -> MailboxItem {
        serde_json::from_value(json!({
            "_docID": "bae-q1",
            "item_key": "runtime:request-doc:ask:1",
            "requester_did": "did:key:z6MkNode",
            "node_did": "did:key:z6MkNode",
            "status": "open",
            "kind": "ask",
            "action": "start_request",
            "title": "Release channel",
            "summary": "which one ships",
            "payload": payload.to_string(),
            "source_kind": "runtime",
            "source_id": "request-doc",
            "session_id": "s1",
            "request_id": "r1",
            "target_node_did": "did:key:z6MkNode",
            "target_agent_did": "did:key:z6MkAgent",
            "created_at": "2026-11-05T12:00:00Z",
        }))
        .expect("a mailbox item")
    }

    fn channels(version: u32, optional: bool) -> serde_json::Value {
        json!({
            "version": version,
            "prompt": "Which channel ships?",
            "options": [{ "id": "stable", "label": "Stable" }, { "id": "beta", "label": "Beta" }],
            "optional": optional,
        })
    }

    fn pick(option: &str) -> MailboxQuestionAnswer {
        MailboxQuestionAnswer {
            option_ids: vec![option.to_string()],
            ..Default::default()
        }
    }

    fn skip() -> MailboxQuestionAnswer {
        MailboxQuestionAnswer {
            skip: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn a_version_three_answer_carries_its_receipt() {
        let item = question(channels(3, false));

        let (content, receipt) = question_reply(&item, &pick("beta")).expect("an answer");

        assert_eq!(content, "Decision on Release channel: Beta (beta)");
        let receipt = receipt.expect("a modern question needs its receipt");
        assert_eq!(receipt.source_doc_id, "bae-q1");
        assert_eq!(receipt.question_version, Some(3));
        let payload = item.payload.as_deref().unwrap();
        receipt
            .validate_for(
                &item.doc_id,
                &item.title,
                item.summary.as_deref(),
                payload,
                item.request_id.as_deref(),
                &content,
            )
            .expect("the claim accepts the receipt for this content");
    }

    #[test]
    fn skip_needs_an_optional_modern_question() {
        let (content, receipt) =
            question_reply(&question(channels(3, true)), &skip()).expect("an optional skip");
        assert_eq!(content, "Skipped optional decision on Release channel.");
        assert!(receipt.is_some());

        for version in [1, 3] {
            let refused = question_reply(&question(channels(version, false)), &skip())
                .expect_err("a required question cannot be skipped");
            assert!(
                refused.to_string().starts_with("answer this question;"),
                "{refused}"
            );
        }
    }
}
