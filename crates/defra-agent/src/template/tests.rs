use super::*;

#[test]
fn renders_event_var() {
    let scope = TemplateScope {
        event: serde_json::json!({"fired_at": "2026-04-21T00:00:00Z", "trigger_kind": "schedule"}),
        doc: None,
        args: None,
    };
    let out = render_template("fired at {{ event.fired_at }}", &scope).unwrap();
    assert_eq!(out, "fired at 2026-04-21T00:00:00Z");
}

#[test]
fn strict_undefined_errors_on_missing_var() {
    let scope = TemplateScope {
        event: serde_json::json!({}),
        doc: None,
        args: None,
    };
    let err = render_template("{{ event.missing }}", &scope).unwrap_err();
    assert!(matches!(err, TemplateError::Render(_)));
}

#[test]
fn enforces_rendered_size_cap() {
    // construct a template whose output exceeds MAX_RENDERED_BYTES
    let big = "x".repeat(2_000_000);
    let scope = TemplateScope {
        event: serde_json::json!({"big": big}),
        doc: None,
        args: None,
    };
    let err = render_template("{{ event.big }}", &scope).unwrap_err();
    assert!(matches!(err, TemplateError::SizeCap(_)));
}

#[test]
fn enforces_template_size_cap() {
    let big = "x".repeat(100_000); // exceeds 64 KB
    let scope = TemplateScope {
        event: serde_json::json!({}),
        doc: None,
        args: None,
    };
    let err = render_template(&big, &scope).unwrap_err();
    assert!(matches!(err, TemplateError::Parse(_)));
}
