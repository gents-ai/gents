use gents_lean_vocab::families::mailbox_question_contracts_presentations::PresentationOutput;
use gents_lean_vocab::families::mailbox_question_contracts_questions::QuestionOutput;
use gents_lean_vocab::families::mailbox_question_contracts_receipt_versions::ReceiptOutput;
use gents_lean_vocab::families::mailbox_question_contracts_runtimes::RuntimeOutput;
use gents_lean_vocab::families::mailbox_question_contracts_waits::WaitOutput;
use gents_protocol::mailbox_question::{
    question_requires_reply_receipt, MailboxQuestion, MailboxQuestionAnswer,
};
use serde_json::json;

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

fn question_payload(version: u64, optional: bool, option_count: u64, free_text: bool) -> String {
    json!({
        "version": version,
        "prompt": "Which approach?",
        "options": (0..option_count).map(|i| json!({
            "id": format!("choice-{i}"), "label": format!("Choice {i}")
        })).collect::<Vec<_>>(),
        "optional": optional,
        "allow_free_text": free_text,
    })
    .to_string()
}

#[test]
fn question_policy_follows_the_executable_model() {
    gents_lean_vocab::families::mailbox_question_contracts_questions::replay(|case| {
        let payload = question_payload(
            case.version,
            case.optional,
            case.option_count,
            case.free_text,
        );
        let question = MailboxQuestion::from_payload(&payload);
        let valid = question.is_ok();
        let answer = serde_json::from_value::<MailboxQuestionAnswer>(json!({
            "skip": case.skip,
            "option_ids": if case.selected { vec!["choice-0"] } else { vec![] },
            "free_text": if case.text { Some("My answer") } else { None },
        }));
        let content = question.and_then(|question| {
            question.reply_content("Plan", &answer.expect("supported answer encoding"))
        });
        QuestionOutput {
            valid,
            accepted: content.is_ok(),
            dismissible: !question_requires_reply_receipt(&payload),
        }
    });
}

#[test]
fn awaiting_answer_follows_the_executable_model() {
    gents_lean_vocab::families::mailbox_question_contracts_waits::replay(|case| WaitOutput {
        waiting: MailboxQuestion::from_payload(&question_payload(
            case.policy.version,
            case.policy.optional,
            case.policy.option_count,
            case.policy.free_text,
        ))
        .is_ok_and(|question| {
            question.awaiting_answer(case.is_open, case.same_parent, case.completed)
        }),
    });
}

#[test]
fn question_presentation_follows_the_executable_model() {
    gents_lean_vocab::families::mailbox_question_contracts_presentations::replay(|case| {
        let mut value: serde_json::Value = serde_json::from_str(&question_payload(
            case.policy.version,
            case.policy.optional,
            case.policy.option_count,
            case.policy.free_text,
        ))
        .unwrap();
        value["multi_select"] = json!(case.multi_select);
        if case.has_prompt {
            value["options"][0]["prompt"] = json!(if case.nonempty_prompt {
                "Review this focused change and report the result."
            } else {
                " "
            });
        }
        if let Some(order) = &case.order {
            value["recommended_order"] = json!(order
                .iter()
                .map(|index| format!("choice-{index}"))
                .collect::<Vec<_>>());
        }
        PresentationOutput {
            valid: MailboxQuestion::from_payload(&value.to_string()).is_ok(),
        }
    });
}

#[test]
fn reply_receipt_versions_follow_the_executable_model() {
    gents_lean_vocab::families::mailbox_question_contracts_receipt_versions::replay(|case| {
        let payload = question_payload(
            case.policy.version,
            case.policy.optional,
            case.policy.option_count,
            case.policy.free_text,
        );
        let observed_hash = gents_protocol::mailbox_question::question_source_hash(
            "Plan",
            None,
            &payload,
            Some("request"),
        )
        .unwrap();
        let valid_answer = MailboxQuestionAnswer {
            option_ids: if case.policy.option_count > 0 {
                vec!["choice-0".into()]
            } else {
                Vec::new()
            },
            free_text: (case.policy.option_count == 0).then(|| "My answer".into()),
            question_hash: Some(observed_hash),
            ..Default::default()
        };
        let receipt = gents_protocol::mailbox_question::MailboxQuestionReply::new(
            "question",
            "Plan",
            None,
            &payload,
            Some("request"),
            valid_answer.clone(),
        );
        let current = receipt.as_ref().ok().is_some_and(|original| {
            let mut evidence = original.clone();
            evidence.question_version = case.version.map(|version| version as u32);
            let content = MailboxQuestion::from_payload(&payload)
                .unwrap()
                .reply_content("Plan", &valid_answer)
                .unwrap();
            evidence
                .validate_for(
                    "question",
                    "Plan",
                    None,
                    &payload,
                    Some("request"),
                    &content,
                )
                .is_ok()
        });
        ReceiptOutput { current }
    });
}

#[test]
fn reply_runtime_support_follows_the_executable_model() {
    gents_lean_vocab::families::mailbox_question_contracts_runtimes::replay(|case| {
        let input = gents_protocol::request_input::RequestInput {
            question_reply: Some(gents_protocol::mailbox_question::MailboxQuestionReply {
                question_version: case.reply_version.map(|version| version as u32),
                source_doc_id: "question".into(),
                question_hash: "source".into(),
                answer: Default::default(),
            }),
            ..Default::default()
        };
        RuntimeOutput {
            supported: input.question_reply_compatible(case.version),
        }
    });
}
