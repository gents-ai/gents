//! Task templates use the production renderer; LayeredPromptBuilder preserves
//! literal system preambles. Earlier system-prompt rendering in the behavior
//! builder remains a runtime migration; these hand-authored cases cover assembly
//! and rendering, not generated task-render witnesses or dispatch persistence.

use gents::document_config::Task;
use gents::prompt::LayeredPromptBuilder;
use gents::template::{render_template, task_node_ctx, TemplateError, TemplateScope};

/// A task document carrying a per-invocation template. Built from the
/// canonical config type so the fence tracks the authoring contract, not a
/// hand-rolled struct.
fn task_with_template(template: &str) -> Task {
    serde_json::from_value(serde_json::json!({
        "agent_did": "did:key:zPRINCIPAL",
        "task_id": "fence-task",
        "behavior_id": "fence",
        "prompt_template": template
    }))
    .expect("compact canonical task document")
}

/// A fixture scope using production node/context construction. Dispatch itself
/// is exercised by the trigger engine tests.
fn fire_scope(behavior_id: &str, now: &str, args: serde_json::Value) -> TemplateScope {
    let (node, ctx) = task_node_ctx("did:key:zPRINCIPAL", behavior_id, now);
    TemplateScope {
        event: serde_json::json!({}),
        doc: None,
        args: Some(args),
        group: None,
        node,
        ctx,
    }
}

/// Template syntax in these preamble fixtures stays literal: the builder
/// does not evaluate it (Template.assembled_preamble_literal).
#[test]
fn system_preamble_is_literal_template_syntax_is_inert() {
    let literal = "You are {{ node.behavior_id }}. Now: {{ ctx.now }}.";
    let builder = LayeredPromptBuilder::for_behavior(literal, "fence", &["bash"], false, &[]);

    let preamble = builder.preamble();
    assert!(
        preamble.starts_with(literal),
        "the preamble must carry the system instruction bytes unchanged, got: {preamble}"
    );
    // The binding-looking syntax is inert: nothing substituted it.
    assert!(
        preamble.contains("{{ node.behavior_id }}") && preamble.contains("{{ ctx.now }}"),
        "the preamble evaluated template syntax that must stay literal: {preamble}"
    );
}

/// Only the task slot substitutes bindings: the same literal text stays raw in
/// the system preamble while the task template renders it
/// (Template.task_binding_preserves_context).
#[test]
fn task_template_substitutes_only_in_the_task_slot() {
    let literal = "Deploy {{ args.target }} now.";
    let task = task_with_template(literal);

    let builder = LayeredPromptBuilder::for_behavior(literal, "fence", &["bash"], false, &[]);
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

/// The task template is rendered per invocation against that invocation's
/// binding: different bindings produce different prompts, the same binding is
/// deterministic (Template.assembled_task_rendered, render_determined).
#[test]
fn task_template_renders_per_invocation() {
    let task =
        task_with_template("Review {{ args.target }} at {{ ctx.now }} on {{ node.behavior_id }}.");

    let first = render_template(
        &task.prompt_template,
        &fire_scope("fence", "T1", serde_json::json!({"target": "a"})),
    )
    .expect("per-request variables are legal in the task slot");
    let second = render_template(
        &task.prompt_template,
        &fire_scope("fence", "T2", serde_json::json!({"target": "b"})),
    )
    .expect("per-request variables are legal in the task slot");

    assert_eq!(first, "Review a at T1 on fence.");
    assert_eq!(second, "Review b at T2 on fence.");
    assert_ne!(first, second, "the task slot must vary with its binding");

    let replay = render_template(
        &task.prompt_template,
        &fire_scope("fence", "T1", serde_json::json!({"target": "a"})),
    )
    .expect("re-rendering is deterministic");
    assert_eq!(replay, first);
}

/// Rendering depends only on the variables the template reads: scopes that
/// agree on every read variable render identically even where they disagree
/// elsewhere (Template.render_determined).
#[test]
fn task_render_depends_only_on_read_variables() {
    let task = task_with_template("Work {{ args.task }}");

    let (node, ctx_a) = task_node_ctx("did:key:zPRINCIPAL", "fence", "T1");
    let a = TemplateScope {
        event: serde_json::json!({}),
        doc: None,
        args: Some(serde_json::json!({"task": "triage"})),
        group: None,
        node,
        ctx: ctx_a,
    };
    let (node_b, ctx_b) = task_node_ctx("did:key:zPRINCIPAL", "fence", "T2");
    let b = TemplateScope {
        event: serde_json::json!({"unrelated": 1}),
        doc: Some(serde_json::json!({"also": "unrelated"})),
        args: Some(serde_json::json!({"task": "triage"})),
        group: Some(serde_json::json!({"untracked": true})),
        node: node_b,
        ctx: ctx_b,
    };

    assert_eq!(
        render_template(&task.prompt_template, &a).expect("renders"),
        render_template(&task.prompt_template, &b).expect("renders"),
        "renders must agree whenever every read variable agrees"
    );
}

/// Colliding names in separate scope slots must resolve through the named slot.
#[test]
fn task_render_keeps_event_and_argument_namespaces_separate() {
    let task = task_with_template("{{ event.trigger_kind }}:{{ args.trigger_kind }}");
    let mut scope = fire_scope(
        "fence",
        "T0",
        serde_json::json!({"trigger_kind": "argument"}),
    );
    scope.event = serde_json::json!({"trigger_kind": "event"});

    assert_eq!(
        render_template(&task.prompt_template, &scope).expect("both scope slots render"),
        "event:argument"
    );
}

/// Unbound variables fail closed at the task slot: the renderer errors instead
/// of silently substituting empty text.
#[test]
fn task_render_fails_closed_on_unbound_variables() {
    let task = task_with_template("Ship {{ ctx.bogus_missing }}");

    let err = render_template(
        &task.prompt_template,
        &fire_scope("fence", "T1", serde_json::json!({})),
    )
    .expect_err("an unbound variable must fail the render");
    assert!(
        matches!(err, TemplateError::Render(_)),
        "expected a render-time failure, got: {err:?}"
    );

    // An absent scope root must fail as well as an absent key.
    let task = task_with_template("Run {{ args.name }}");
    let mut scope = fire_scope("fence", "T1", serde_json::json!({}));
    scope.args = None;
    assert!(matches!(
        render_template(&task.prompt_template, &scope),
        Err(TemplateError::Render(_))
    ));
}
