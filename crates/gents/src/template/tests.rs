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
        "{% if doc.urgent %}{{ doc.items | map('nosuchfilter') }}{% endif %}",
        "{% for row in doc.rows %}{{ row.tags | select('notempty') }}{% endfor %}",
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
fn authoring_rejects_a_function_a_fire_would_only_reach_conditionally() {
    for template in [
        "{% if doc.urgent %}{{ now() }}{% endif %}",
        "{% for row in doc.rows %}{{ uuid() }}{% endfor %}",
        "{% if doc.urgent %}ok{% else %}{{ lipsum() }}{% endif %}",
        "{% if doc.body is defined %}{{ cycler(1, 2) }}{% endif %}",
    ] {
        let error = check_template_vocabulary(template).expect_err(&format!("accepted {template}"));
        assert!(
            matches!(&error, TemplateError::UnknownName { kind, .. } if *kind == "function"),
            "accepted {template}: {error}"
        );
    }
}

#[test]
fn authoring_accepts_a_called_name_the_template_binds_itself() {
    // A call resolves through the frames the template itself binds, and through
    // the globals the engine does provide: neither is an unknown name.
    for template in [
        "{% set f = args.formatter %}{{ f() }}",
        "{% set f = args.formatter %}{% if doc.urgent %}{{ f() }}{% endif %}",
        "{% with f = args.formatter %}{{ f() }}{% endwith %}",
        "{% for f in group.docs %}{{ f() }}{% endfor %}",
        "{% for row in group.docs recursive %}{{ loop(row.children) }}{% endfor %}",
        "{{ range(3) | length }} {{ dict(a=1) }} {% set ns = namespace(n=0) %}{{ ns.n }}",
    ] {
        check_template_vocabulary(template).expect(template);
    }
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
        "{% raw %}{{ doc.x | toyaml }}{% endraw %}",
    ] {
        check_template_vocabulary(template).expect(template);
    }
}

#[test]
fn authoring_reports_a_syntax_error_rather_than_a_missing_name() {
    for template in [
        "{% if doc.message %}missing endif",
        // The walk reads one instruction stream. A block or a macro would
        // compile into another, and the engine this workspace builds cannot
        // parse either, so neither reaches the walk.
        "{% block body %}{{ doc.x | toyaml }}{% endblock %}",
        "{% macro shout(x) %}{{ x | toyaml }}{% endmacro %}",
    ] {
        let error = check_template_vocabulary(template).expect_err(template);
        assert!(
            matches!(error, TemplateError::Parse(_)),
            "{template}: {error}"
        );
    }
}

#[test]
fn authoring_rejects_a_filter_name_map_resolves_from_its_argument() {
    let error = check_template_vocabulary("{{ doc.items | map('nosuchfilter') }}").unwrap_err();
    assert!(
        matches!(&error, TemplateError::UnknownName { kind, name } if *kind == "filter" && name == "nosuchfilter"),
        "{error}"
    );
}

#[test]
fn authoring_rejects_a_test_name_select_resolves_from_its_argument() {
    let error = check_template_vocabulary("{{ doc.rows | select('notempty') }}").unwrap_err();
    assert!(
        matches!(&error, TemplateError::UnknownName { kind, name } if *kind == "test" && name == "notempty"),
        "{error}"
    );
}

#[test]
fn authoring_rejects_a_test_name_reject_resolves_from_its_argument() {
    let error = check_template_vocabulary("{{ doc.rows | reject('notempty') }}").unwrap_err();
    assert!(
        matches!(&error, TemplateError::UnknownName { kind, name } if *kind == "test" && name == "notempty"),
        "{error}"
    );
}

#[test]
fn authoring_rejects_a_test_name_selectattr_resolves_from_its_argument() {
    let error =
        check_template_vocabulary("{{ doc.rows | selectattr('id', 'nosuchtest') }}").unwrap_err();
    assert!(
        matches!(&error, TemplateError::UnknownName { kind, name } if *kind == "test" && name == "nosuchtest"),
        "{error}"
    );
}

#[test]
fn authoring_rejects_a_test_name_rejectattr_resolves_from_its_argument() {
    let error =
        check_template_vocabulary("{{ doc.rows | rejectattr('id', 'nosuchtest') }}").unwrap_err();
    assert!(
        matches!(&error, TemplateError::UnknownName { kind, name } if *kind == "test" && name == "nosuchtest"),
        "{error}"
    );
}

#[test]
fn authoring_rejects_a_test_the_engine_cannot_provide() {
    // `trim` is a registered filter and not a registered test, so judging a name
    // in test position against the filters would admit it.
    for (template, unknown) in [
        ("{% if doc.body is jsonish %}yes{% endif %}", "jsonish"),
        ("{% if doc.body is trim %}yes{% endif %}", "trim"),
        ("{{ doc.rows | select('trim') }}", "trim"),
    ] {
        let error = check_template_vocabulary(template).expect_err(template);
        assert!(
            matches!(&error, TemplateError::UnknownName { kind, name } if *kind == "test" && name == unknown),
            "{template}: {error}"
        );
    }
}

#[test]
fn authoring_accepts_a_name_argument_admission_cannot_decide() {
    for template in [
        "{{ doc.items | map('lower') }}",
        "{{ doc.items | map(attribute='username') }}",
        "{{ doc.items | map(attribute='username', default='anonymous') }}",
        "{{ doc.rows | select('odd') }}",
        "{{ doc.rows | select('gt', 3) }}",
        "{{ doc.rows | reject('none') }}",
        "{{ doc.rows | selectattr('id', 'even') }}",
        "{{ doc.rows | selectattr('active') }}",
        "{{ doc.rows | rejectattr('id', 'gt', 3) }}",
        "{{ doc.rows | select }}",
        // A name no constant carries is not an admission fact.
        "{{ doc.items | map(doc.filter_name) }}",
        "{{ doc.items | map(*args.spec) }}",
        "{{ doc.items | map('lower', doc.extra) }}",
        // `is filter` and `is test` answer whether a name resolves rather than
        // resolving it, so an absent name is their result, not a failure.
        "{% if 'nosuchfilter' is filter %}yes{% endif %}",
        "{% if 'nosuchtest' is test %}yes{% endif %}",
        "{{ doc.rows | select(doc.test_name, doc.threshold) }}",
        "{{ doc.rows | selectattr('id', args.test, 3) }}",
        // A conditional or short-circuit name reaches the lookup as whichever
        // constant the invocation selects, in either branch order.
        "{{ doc.items | map('nosuchfilter' if doc.conditional else 'lower') }}",
        "{{ doc.items | map('lower' if doc.conditional else 'nosuchfilter') }}",
        "{{ doc.rows | select(doc.test_name or 'nosuchtest', doc.threshold) }}",
        "{{ doc.rows | select('nosuchtest' if doc.conditional else 'gt', doc.threshold) }}",
        // Named arguments after a registered name carry no name themselves.
        "{{ doc.rows | selectattr('id', 'gt', doc.threshold if doc.strict else 0) }}",
    ] {
        check_template_vocabulary(template).expect(template);
    }
}

#[test]
fn authoring_accepts_a_call_on_every_name_the_render_scope_resolves() {
    // Emit position compiles `super()` and a one-argument `loop()` into their
    // own instructions, so only a call inside an expression reaches the walk.
    for template in [
        "{% for row in group.docs recursive %}{% set sub = loop(row.children) %}{{ sub }}{% endfor %}",
        "{% set inherited = super() %}{{ inherited }}",
        "{{ doc() }} {{ args() }} {{ event() }} {{ group() }} {{ node() }} {{ ctx() }}",
    ] {
        check_template_vocabulary(template).expect(template);
    }
}

#[test]
fn authoring_rejects_a_constant_name_argument_followed_by_dynamic_arguments() {
    // The builtin resolves its name argument before it uses any later one, so a
    // dynamic value after the name does not make the name value-dependent.
    for (template, kind, unknown) in [
        (
            "{{ doc.rows | select('nosuchtest', doc.threshold) }}",
            "test",
            "nosuchtest",
        ),
        (
            "{{ doc.items | map('nosuchfilter', doc.extra) }}",
            "filter",
            "nosuchfilter",
        ),
        (
            "{{ doc.rows | reject('nosuchtest', doc.threshold + 1) }}",
            "test",
            "nosuchtest",
        ),
        (
            "{{ doc.rows | selectattr('id', 'nosuchtest', doc.threshold) }}",
            "test",
            "nosuchtest",
        ),
        (
            "{{ doc.rows | rejectattr(doc.field, 'nosuchtest', doc.threshold) }}",
            "test",
            "nosuchtest",
        ),
        (
            "{{ doc.rows | select('nosuchtest', doc.a if doc.flag else doc.b) }}",
            "test",
            "nosuchtest",
        ),
        (
            "{{ doc.items | map('nosuchfilter', doc.extra | default(1), key=doc.k) }}",
            "filter",
            "nosuchfilter",
        ),
        (
            "{{ doc.rows | select('nosuchtest', doc.a < doc.b < doc.c) }}",
            "test",
            "nosuchtest",
        ),
    ] {
        let error = check_template_vocabulary(template).expect_err(template);
        assert!(
            matches!(&error, TemplateError::UnknownName { kind: found, name } if *found == kind && name == unknown),
            "{template}: {error}"
        );
    }
}

#[test]
fn conditional_name_arguments_that_admission_defers_render_for_their_invocations() {
    for (template, flag) in [
        (
            "{{ doc.items | map('lower' if doc.flag else 'nosuchfilter') | join(',') }}",
            true,
        ),
        (
            "{{ doc.items | map('nosuchfilter' if doc.flag else 'lower') | join(',') }}",
            false,
        ),
    ] {
        check_template_vocabulary(template).expect(template);
        let scope = TemplateScope {
            event: serde_json::Value::Null,
            doc: Some(serde_json::json!({ "items": ["A", "B"], "flag": flag })),
            args: None,
            group: None,
            node: serde_json::Value::Null,
            ctx: serde_json::Value::Null,
        };
        assert_eq!(render_template(template, &scope).expect(template), "a,b");
    }
}
