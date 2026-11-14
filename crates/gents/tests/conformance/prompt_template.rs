use gents::turn::prompt::LayeredPromptBuilder;
use gents_documents::template::{render_template, task_node_ctx, TemplateScope};
use gents_documents::Task;

fn task_with_template(template: &str) -> Task {
    serde_json::from_value(serde_json::json!({
        "node_did": "did:key:zNODE",
        "task_id": "fence-task",
        "agent_did": "fence",
        "prompt_template": template
    }))
    .expect("compact canonical task document")
}

fn fire_scope(agent_did: &str, now: &str, args: serde_json::Value) -> TemplateScope {
    let (node, ctx) = task_node_ctx("did:key:zNODE", agent_did, now);
    TemplateScope {
        session: None,
        request: None,
        event: serde_json::json!({}),
        doc: None,
        args: Some(args),
        group: None,
        node,
        ctx,
    }
}

#[test]
fn system_preamble_is_literal_template_syntax_is_inert() {
    let literal = "You are {{ node.agent_did }}. Now: {{ ctx.now }}.";
    let builder = LayeredPromptBuilder::for_agent(literal, "fence", &["bash"], false, &[]);

    let preamble = builder.preamble();
    assert!(
        preamble.starts_with(literal),
        "the preamble must carry the system instruction bytes unchanged, got: {preamble}"
    );
    assert!(
        preamble.contains("{{ node.agent_did }}") && preamble.contains("{{ ctx.now }}"),
        "the preamble evaluated template syntax that must stay literal: {preamble}"
    );
}

#[test]
fn task_template_substitutes_only_in_the_task_slot() {
    let literal = "Deploy {{ args.target }} now.";
    let task = task_with_template(literal);

    let builder = LayeredPromptBuilder::for_agent(literal, "fence", &["bash"], false, &[]);
    let preamble = builder.preamble();
    assert!(
        preamble.starts_with(literal) && !preamble.contains("TREE"),
        "the preamble must not consume task bindings, got: {preamble}"
    );

    let rendered = render_template(
        &task.prompt_template,
        &fire_scope("fence", "T0", serde_json::json!({"target": "TREE"})),
    )
    .expect("the task slot renders the bound template");
    assert_eq!(rendered, "Deploy TREE now.");
}
