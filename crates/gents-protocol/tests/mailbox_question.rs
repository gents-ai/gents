use gents_protocol::mailbox_question::{
    question_requires_reply_receipt, MailboxQuestion, MailboxQuestionAnswer,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct Case {
    version: u32,
    optional: bool,
    option_count: usize,
    free_text: bool,
    skip: bool,
    selected: bool,
    text: bool,
    valid: bool,
    accepted: bool,
    dismissible: bool,
}

#[test]
fn existing_question_and_answer_wire_shapes_remain_readable() {
    let mut question = MailboxQuestion::from_payload(
        &json!({
            "version": 1, "prompt": "Which approach?", "options": [
                { "id": "small", "label": "Small change" },
                { "id": "broad", "label": "Broader change" }
            ]
        })
        .to_string(),
    )
    .unwrap();
    let mut answer: MailboxQuestionAnswer = serde_json::from_value(json!({
        "option_ids": ["small"]
    }))
    .unwrap();
    assert_eq!(
        question.reply_content("Plan", &answer).unwrap(),
        "Decision on Plan: Small change (small)"
    );
    question.optional = Some(false);
    answer.skip = Some(false);
    assert!(serde_json::to_value(&question)
        .unwrap()
        .get("optional")
        .is_none());
    assert!(serde_json::to_value(&answer).unwrap().get("skip").is_none());
    let custom: MailboxQuestionAnswer = serde_json::from_value(json!({
        "option_ids": [], "free_text": "A custom answer"
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(&custom).unwrap(),
        json!({
            "option_ids": [], "free_text": "A custom answer"
        })
    );
}

#[test]
fn signed_question_evidence_rejects_retargeting_and_changed_decisions() {
    use gents_protocol::mailbox_question::{question_source_hash, MailboxQuestionReply};
    let payload = json!({ "version": 2, "prompt": "Any constraints?", "options": [],
        "allow_free_text": true, "optional": true })
    .to_string();
    let observed =
        question_source_hash("Plan", Some("Context"), &payload, Some("request")).unwrap();
    let answer = MailboxQuestionAnswer {
        skip: Some(true),
        question_hash: Some(observed),
        ..Default::default()
    };
    let content = MailboxQuestion::from_payload(&payload)
        .unwrap()
        .reply_content("Plan", &answer)
        .unwrap();
    let evidence = MailboxQuestionReply::new(
        "question",
        "Plan",
        Some("Context"),
        &payload,
        Some("request"),
        answer.clone(),
    )
    .unwrap();
    let stored = serde_json::to_value(&evidence).unwrap();
    assert!(stored["answer"]["option_ids"].is_null());
    let recovered: MailboxQuestionReply = serde_json::from_value(stored).unwrap();
    assert_eq!(recovered, evidence);
    assert!(evidence
        .validate_for(
            "question",
            "Plan",
            Some("Context"),
            &payload,
            Some("request"),
            &content
        )
        .is_ok());
    for (id, context, request, text) in [
        ("other", "Context", "request", content.as_str()),
        ("question", "Changed context", "request", content.as_str()),
        ("question", "Context", "other-request", content.as_str()),
        ("question", "Context", "request", "A different answer"),
    ] {
        assert!(evidence
            .validate_for(id, "Plan", Some(context), &payload, Some(request), text)
            .is_err());
    }
    assert!(MailboxQuestionReply::new(
        "question",
        "Plan",
        Some("Changed context"),
        &payload,
        Some("request"),
        answer
    )
    .is_err());
    let required = payload.replace("\"optional\":true", "\"optional\":false");
    let forged = MailboxQuestionReply::new(
        "question",
        "Plan",
        Some("Context"),
        &required,
        Some("request"),
        MailboxQuestionAnswer {
            skip: Some(true),
            question_hash: Some(
                question_source_hash("Plan", Some("Context"), &required, Some("request")).unwrap(),
            ),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(forged
        .validate_for(
            "question",
            "Plan",
            Some("Context"),
            &required,
            Some("request"),
            &content
        )
        .is_err());
}

#[test]
fn question_policy_follows_the_executable_model() {
    let snapshot: serde_json::Value =
        serde_json::from_str(&gents_lean_contract::load_contract_json().unwrap()).unwrap();
    let cases: Vec<Case> =
        serde_json::from_value(snapshot["mailbox_question_cases"].clone()).unwrap();
    assert_eq!(cases.len(), 640);
    for case in cases {
        let payload = json!({
            "version": case.version,
            "prompt": "Which approach?",
            "options": (0..case.option_count).map(|i| json!({
                "id": format!("choice-{i}"), "label": format!("Choice {i}")
            })).collect::<Vec<_>>(),
            "optional": case.optional,
            "allow_free_text": case.free_text,
        })
        .to_string();
        assert_eq!(
            !question_requires_reply_receipt(&payload),
            case.dismissible,
            "{case:?}"
        );
        let question = MailboxQuestion::from_payload(&payload);
        assert_eq!(question.is_ok(), case.valid, "{case:?}");
        let answer = serde_json::from_value::<MailboxQuestionAnswer>(json!({
            "skip": case.skip,
            "option_ids": if case.selected { vec!["choice-0"] } else { vec![] },
            "free_text": if case.text { Some("My answer") } else { None },
        }));
        let content = question.and_then(|question| {
            question.reply_content("Plan", &answer.expect("supported answer encoding"))
        });
        assert_eq!(content.is_ok(), case.accepted, "{case:?}");
    }
}
