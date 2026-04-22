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

#[test]
fn parse_template_for_validation_collects_event_and_doc_paths() {
    let template = "{{ event.fired_at }} {{ doc.customer.name }}";
    let refs = parse_template_for_validation(template).unwrap();
    assert_eq!(
        refs,
        vec![
            VariableRef {
                path: vec!["event".to_string(), "fired_at".to_string()],
            },
            VariableRef {
                path: vec![
                    "doc".to_string(),
                    "customer".to_string(),
                    "name".to_string(),
                ],
            },
        ]
    );
}

#[test]
fn parse_template_for_validation_ignores_unrelated_identifiers() {
    let refs = parse_template_for_validation("hello {{ user.name }} world").unwrap();
    assert!(refs.is_empty());
}

#[test]
fn parse_template_for_validation_supports_bracket_string_indexing() {
    let template = r#"{{ event["fired_at"] }} {{ args['mode'] }}"#;
    let refs = parse_template_for_validation(template).unwrap();
    assert_eq!(
        refs,
        vec![
            VariableRef {
                path: vec!["event".to_string(), "fired_at".to_string()],
            },
            VariableRef {
                path: vec!["args".to_string(), "mode".to_string()],
            },
        ]
    );
}

#[test]
fn parse_template_for_validation_handles_statements_and_comments() {
    let template = "{# doc.ignored #}{% if event.ok %}yes{% endif %}";
    let refs = parse_template_for_validation(template).unwrap();
    assert_eq!(
        refs,
        vec![VariableRef {
            path: vec!["event".to_string(), "ok".to_string()],
        }]
    );
}

#[test]
fn parse_template_for_validation_skips_suffix_event_in_attr_access() {
    let template = "{{ doc.event.name }}";
    let refs = parse_template_for_validation(template).unwrap();
    assert_eq!(
        refs,
        vec![VariableRef {
            path: vec![
                "doc".to_string(),
                "event".to_string(),
                "name".to_string(),
            ],
        }]
    );
}
