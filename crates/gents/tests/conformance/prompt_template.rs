//! PromptAssembly.Template conformance (issue #497).

use gents::template::catalog::{default_catalog, Volatility};
use gents::template::reads::{collect_system_reads, validate_system_template};
use gents::template::{render_template, TemplateScope};

fn scope(now: &str) -> TemplateScope {
    TemplateScope {
        event: serde_json::json!({}),
        doc: None,
        args: None,
        node: serde_json::json!({
            "node_did": "did:key:zNODE",
            "behavior_id": "policy_agent",
        }),
        ctx: serde_json::json!({
            "now": now,
        }),
    }
}

#[test]
fn system_render_stable_under_per_request_change() {
    let tmpl = "You are {{ node.behavior_id }} on {{ node.node_did }}.";
    let cat = default_catalog();
    validate_system_template(tmpl, &cat).expect("well-formed system template");

    let r1 = render_template(tmpl, &scope("2026-06-15T00:00:00Z")).unwrap();
    let r2 = render_template(tmpl, &scope("2030-01-01T12:00:00Z")).unwrap();
    assert_eq!(r1, r2, "system render must be byte-stable across requests");
}

#[test]
fn validate_rejects_per_request_ref_in_system_template() {
    let cat = default_catalog();
    let err = validate_system_template("Now: {{ ctx.now }}", &cat).unwrap_err();
    assert!(
        format!("{err}").contains("ctx.now"),
        "error must name the offending per-request var, got: {err}"
    );
}

#[test]
fn validate_rejects_unanalyzable_construct_in_system_template() {
    let cat = default_catalog();
    assert!(
        validate_system_template("{% for x in node.list %}{{ x }}{% endfor %}", &cat).is_err(),
        "system template with control flow must be rejected"
    );
}

#[test]
fn validate_accepts_per_request_ref_inside_raw_block() {
    let cat = default_catalog();
    validate_system_template("Literal: {% raw %}{{ ctx.now }}{% endraw %}", &cat)
        .expect("raw block contents are not reads");
}

#[test]
fn validate_rejects_unknown_namespace_path() {
    let cat = default_catalog();
    assert!(
        validate_system_template("{{ ctx.bogus_unknown }}", &cat).is_err(),
        "unknown ctx.* path must reject"
    );
    assert!(
        validate_system_template("{{ node.bogus_unknown }}", &cat).is_err(),
        "unknown node.* path must reject"
    );
}

#[test]
fn collect_system_reads_returns_full_refs() {
    let reads = collect_system_reads(r#"{{ node.node_did }} {{ node["behavior_id"] }}"#).unwrap();
    assert!(reads.contains("node.node_did"));
    assert!(reads.contains("node.behavior_id"));
}

#[test]
fn catalog_volatility_matches_model() {
    let cat = default_catalog();
    assert_eq!(
        cat.volatility("node.node_did"),
        Some(Volatility::RunConstant)
    );
    assert_eq!(cat.volatility("ctx.now"), Some(Volatility::PerRequest));
    assert_eq!(cat.volatility("ctx.unknown"), None);
}
