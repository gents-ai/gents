use super::*;

#[test]
fn authoring_rejects_invalid_rendering_syntax_without_invocation_values() {
    for source in ["{{.message}}", "{{ doc.message", "{% if doc.message %}"] {
        let error = parse_template_for_validation(source).unwrap_err();
        assert!(error.to_string().contains("MiniJinja"));
    }
    parse_template_for_validation("Process {{ doc.message }} for {{ args.name }}").unwrap();
}

#[test]
fn renders_event_var() {
    let scope = TemplateScope {
        event: serde_json::json!({"fired_at": "2026-04-21T00:00:00Z", "trigger_kind": "schedule"}),
        doc: None,
        args: None,
        group: None,
        node: serde_json::json!({}),
        ctx: serde_json::json!({}),
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
        group: None,
        node: serde_json::json!({}),
        ctx: serde_json::json!({}),
    };
    let err = render_template("{{ event.missing }}", &scope).unwrap_err();
    assert!(matches!(err, TemplateError::Render(_)));
}

#[test]
fn renders_args_var_when_scope_has_args() {
    let scope = TemplateScope {
        event: serde_json::json!({"trigger_kind": "manual"}),
        doc: None,
        args: Some(serde_json::json!({"name": "Amy", "count": 3})),
        group: None,
        node: serde_json::json!({}),
        ctx: serde_json::json!({}),
    };
    let out = render_template("hi {{ args.name }}, n={{ args.count }}", &scope).unwrap();
    assert_eq!(out, "hi Amy, n=3");
}

#[test]
fn errors_on_missing_args_key() {
    let scope = TemplateScope {
        event: serde_json::json!({}),
        doc: None,
        args: Some(serde_json::json!({})), // args present but empty
        group: None,
        node: serde_json::json!({}),
        ctx: serde_json::json!({}),
    };
    let err = render_template("{{ args.missing }}", &scope).unwrap_err();
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
        group: None,
        node: serde_json::json!({}),
        ctx: serde_json::json!({}),
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
        group: None,
        node: serde_json::json!({}),
        ctx: serde_json::json!({}),
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
fn parse_template_for_validation_collects_group_paths() {
    let template = "{{ group.correlation_value }}{% if group.complete %}complete{% endif %}";
    let refs = parse_template_for_validation(template).unwrap();
    assert_eq!(
        refs,
        vec![
            VariableRef {
                path: vec!["group".to_string(), "correlation_value".to_string()],
            },
            VariableRef {
                path: vec!["group".to_string(), "complete".to_string()],
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
            path: vec!["doc".to_string(), "event".to_string(), "name".to_string(),],
        }]
    );
}

#[test]
fn authoring_rejects_a_filter_the_engine_cannot_provide() {
    let error = check_template_vocabulary("{{ doc.correlation | toyaml }}").unwrap_err();
    assert!(
        matches!(&error, TemplateError::UnknownName { kind, name } if *kind == "filter" && name == "toyaml"),
        "{error}"
    );
}

#[test]
fn authoring_accepts_a_filter_the_engine_registers() {
    check_template_vocabulary("{{ doc.correlation | tojson }}").unwrap();
}

#[test]
fn authoring_rejects_an_unresolvable_name_a_fire_would_only_reach_conditionally() {
    for template in [
        "{% if doc.urgent %}{{ doc.correlation | toyaml }}{% endif %}",
        "{% for row in doc.rows %}{{ row | toyaml }}{% endfor %}",
        "{% if doc.urgent %}ok{% else %}{{ doc.body | toyaml }}{% endif %}",
        "{{ doc.body | trim | toyaml }}",
        "{% if doc.body is jsonish %}yes{% endif %}",
    ] {
        let error = check_template_vocabulary(template).unwrap_err();
        assert!(
            matches!(error, TemplateError::UnknownName { .. }),
            "accepted {template}: {error}"
        );
    }
}

#[test]
fn authoring_rejects_a_function_the_engine_cannot_provide() {
    let error = check_template_vocabulary("{{ now() }}").unwrap_err();
    assert!(
        matches!(&error, TemplateError::UnknownName { kind, name } if *kind == "function" && name == "now"),
        "{error}"
    );
}

#[test]
fn authoring_accepts_fields_configuration_cannot_enumerate() {
    // Document and argument fields are caller-defined, and strict undefined is a
    // fire-time outcome: neither may be an authoring rejection.
    for template in [
        "{{ doc.customer.name | upper }} at {{ ctx.now }} for {{ node.behavior_id }}",
        "{{ args.mode | default('review') }} {{ args.count | int }}",
        "{{ doc.whatever.deeply.nested.field }}",
        "{% for row in group.docs %}{{ row.title | default('untitled') }}{% endfor %}",
        "{% if doc.body is defined %}{{ doc.body | trim | replace('a', 'b') }}{% endif %}",
        "{{ group.count }} of {{ event.correlation }} ({{ group.complete }})",
        "{% set names = doc.people | join(', ') %}{{ names | length }}",
        "{% raw %}{{ doc.x | tojson }}{% endraw %}",
    ] {
        check_template_vocabulary(template).expect(template);
    }
}

#[test]
fn authoring_reports_a_syntax_error_rather_than_a_missing_name() {
    let error = check_template_vocabulary("{% if doc.message %}missing endif").unwrap_err();
    assert!(matches!(error, TemplateError::Parse(_)), "{error}");
}
