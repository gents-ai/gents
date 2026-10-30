use super::*;
use gents_protocol::job::StampedField;

#[test]
fn a_run_script_import_needs_a_job_plugin_with_a_script_input() {
    let ok = PackPlugin {
        job: PluginJobDeclaration(true),
        host_imports: vec![HostImport::RunScript],
        input_schema: serde_json::json!({"type": "object", "properties": {"script": {"type": "string"}}}),
        ..valid_plugin()
    };
    ok.validate().unwrap();
    let no_job = PackPlugin {
        job: PluginJobDeclaration(false),
        ..ok.clone()
    };
    assert!(no_job
        .validate()
        .unwrap_err()
        .to_string()
        .contains("host_imports"));
    let no_script = PackPlugin {
        input_schema: serde_json::json!({"type": "object", "properties": {}}),
        ..ok.clone()
    };
    assert!(no_script
        .validate()
        .unwrap_err()
        .to_string()
        .contains("script"));
}

#[test]
fn host_imports_round_trip_and_default_empty() {
    let text = r#"{"name":"w","description":"d","artifact":"a.afb","language":"rust","input_schema":{},"job":true,"host_imports":["run_script"]}"#;
    let plugin: PackPlugin = serde_json::from_str(text).unwrap();
    assert_eq!(plugin.host_imports, vec![HostImport::RunScript]);
    assert!(serde_json::to_value(PackPlugin::default())
        .unwrap()
        .get("host_imports")
        .is_none());
}

fn valid_plugin() -> PackPlugin {
    PackPlugin {
        name: "format_check".to_owned(),
        description: "Checks formatting".to_owned(),
        artifact: "plugins/format_check.afb".to_owned(),
        source: None,
        language: "rust".to_owned(),
        input_schema: serde_json::json!({"type": "object"}),
        manifold: None,
        instructions: None,
        bind_dir: None,
        limits: None,
        model_slot: None,
        service: None,
        ..Default::default()
    }
}

#[test]
fn plugin_manifest_job_fields_parse_from_the_job_fixture() {
    let manifest: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/packs/job_plugin_fixture/manifest.json"
    ))
    .unwrap();
    let plugin: PackPlugin = serde_json::from_value(manifest["plugins"][0].clone()).unwrap();
    assert_eq!(plugin.job, PluginJobDeclaration(true));
    let data = plugin.data.expect("the fixture declares data");
    assert_eq!(data.collections, ["FixtureNote"]);
    assert_eq!(data.ops, ["query", "insert"]);
    assert_eq!(
        data.stamped.get("job_row_doc_id"),
        Some(&StampedField::JobRowDocId)
    );
    assert_eq!(
        data.stamped.get("chain_root_doc_id"),
        Some(&StampedField::ChainRootDocId)
    );
}

#[test]
fn plugin_manifest_job_fields_default_off_and_are_not_serialized() {
    let plain = serde_json::json!({
        "name": "format_check",
        "description": "Checks formatting",
        "artifact": "plugins/format_check.afb",
        "language": "rust",
        "input_schema": {"type": "object"}
    });
    let plugin: PackPlugin = serde_json::from_value(plain.clone()).unwrap();
    assert_eq!(plugin.job, PluginJobDeclaration(false));
    assert!(plugin.data.is_none());
    assert_eq!(serde_json::to_value(&plugin).unwrap(), plain);
}

fn data_plugin(collections: &[&str], ops: &[&str]) -> PackPlugin {
    PackPlugin {
        data: Some(PluginDataDeclaration {
            collections: collections.iter().map(|name| (*name).to_owned()).collect(),
            ops: ops.iter().map(|name| (*name).to_owned()).collect(),
            stamped: BTreeMap::new(),
        }),
        ..valid_plugin()
    }
}

fn refusal(plugin: &PackPlugin) -> String {
    format!("{:#}", plugin.validate().expect_err("must be refused"))
}

#[test]
fn data_collections_must_be_non_empty_and_unique() {
    data_plugin(&["FixtureNote"], &["query"])
        .validate()
        .unwrap();
    for collections in [&[][..], &["FixtureNote", "FixtureNote"][..]] {
        let message = refusal(&data_plugin(collections, &["query"]));
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("data.collections"), "{message}");
    }
}

#[test]
fn unknown_data_ops_are_refused() {
    data_plugin(&["FixtureNote"], &["query", "insert", "update", "delete"])
        .validate()
        .unwrap();
    let message = refusal(&data_plugin(&["FixtureNote"], &["query", "upsert"]));
    assert!(message.contains("format_check"), "{message}");
    assert!(message.contains("data.ops"), "{message}");
    assert!(message.contains("upsert"), "{message}");
}

#[test]
fn unknown_stamp_values_are_refused() {
    let plugin = |stamp: &str| {
        serde_json::from_value::<PackPlugin>(serde_json::json!({
            "name": "format_check",
            "description": "Checks formatting",
            "artifact": "plugins/format_check.afb",
            "language": "rust",
            "input_schema": {"type": "object"},
            "data": {
                "collections": ["FixtureNote"],
                "ops": ["insert"],
                "stamped": {"owner": stamp}
            }
        }))
    };
    plugin("node_did").unwrap().validate().unwrap();
    let error = plugin("host_secret").expect_err("an unknown stamp value must not parse");
    assert!(error.to_string().contains("host_secret"), "{error}");
}

#[test]
fn stamped_keys_must_be_graphql_names() {
    let mut plugin = data_plugin(&["FixtureNote"], &["insert"]);
    let data = plugin.data.as_mut().unwrap();
    data.stamped
        .insert("owner_did".to_owned(), StampedField::NodeDid);
    plugin.validate().unwrap();
    let data = plugin.data.as_mut().unwrap();
    data.stamped
        .insert("owner did {".to_owned(), StampedField::SessionId);
    let message = refusal(&plugin);
    assert!(message.contains("format_check"), "{message}");
    assert!(message.contains("data.stamped"), "{message}");
    assert!(message.contains("owner did {"), "{message}");
}

#[test]
fn secret_stamped_field_names_are_refused() {
    for field in ["api_token", "clientSecret", "password_hash", "signing_key"] {
        let mut plugin = data_plugin(&["FixtureNote"], &["insert"]);
        plugin
            .data
            .as_mut()
            .unwrap()
            .stamped
            .insert(field.to_owned(), StampedField::NodeDid);
        let message = refusal(&plugin);
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains(field), "{message}");
    }
}

fn plugins_pack(slots: serde_json::Value, model_slot: Option<&str>) -> PackManifest {
    let mut plugin = serde_json::to_value(valid_plugin()).unwrap();
    plugin["model_slot"] = serde_json::json!(model_slot);
    serde_json::from_value(serde_json::json!({
        "manifest_version": 1, "name": "ocr", "version": "1.0.0",
        "description": "reads pages", "authors": ["tests"], "kind": "plugins",
        "assets": ["README.md", "plugins/format_check.afb"],
        "inference_slots": slots, "plugins": [plugin],
    }))
    .unwrap()
}

#[test]
fn generated_plugin_model_slots_require_optional_agent_free_declarations() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().plugin_resource_cases;
    for case in cases["model_slots"].as_array().unwrap() {
        let slots = if case["declared"].as_bool().unwrap() {
            serde_json::json!([{"name":"remote_ocr", "description":"d",
                "optional":case["optional"],
                "agents":if case["agent_free"].as_bool().unwrap() {vec![]} else {vec!["scan"]}}])
        } else {
            serde_json::json!([])
        };
        let mut manifest = plugins_pack(slots, Some("remote_ocr"));
        manifest.metadata.kind = PackKind::Documents;
        manifest.config = Some("config.json".to_owned());
        manifest.metadata.assets.push("config.json".to_owned());
        assert_eq!(
            validate_pack_manifest(&manifest).is_ok(),
            case["expected"].as_bool().unwrap(),
            "{case}"
        );
    }
}

/* ---- ux plugins ---- */

fn valid_ux() -> PackUxPlugin {
    PackUxPlugin {
        name: "board".to_owned(),
        description: "A board page".to_owned(),
        entry: Some("ux/board/plugin.js".to_owned()),
        css: None,
        plugin: None,
        contributes: UxContributions {
            areas: vec!["nav".to_owned(), "agent.sections".to_owned()],
            directives: vec![],
        },
        default_enabled: None,
        data: vec![],
    }
}

fn no_plugins() -> BTreeSet<&'static str> {
    BTreeSet::new()
}

fn with_data(data: Vec<UxDataSurface>) -> PackUxPlugin {
    PackUxPlugin { data, ..valid_ux() }
}

fn surface(surface: &str, tools: &[&str]) -> UxDataSurface {
    UxDataSurface {
        surface: surface.to_owned(),
        tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
    }
}

#[test]
fn a_ux_data_declaration_round_trips_and_an_absent_one_is_not_written() {
    let plugin = with_data(vec![
        surface("board", &["list_tasks", "add_task"]),
        surface("notes", &["list_notes"]),
    ]);
    plugin.validate(&no_plugins()).expect("declared surfaces");
    let json = serde_json::to_value(&plugin).unwrap();
    assert_eq!(
        json["data"],
        serde_json::json!([
            { "surface": "board", "tools": ["list_tasks", "add_task"] },
            { "surface": "notes", "tools": ["list_notes"] },
        ])
    );
    let back: PackUxPlugin = serde_json::from_value(json).unwrap();
    assert_eq!(back, plugin);

    let bare = serde_json::to_value(valid_ux()).unwrap();
    assert!(
        bare.get("data").is_none(),
        "an empty data list is not written out"
    );
    let back: PackUxPlugin = serde_json::from_value(bare).unwrap();
    assert!(back.data.is_empty());

    let unknown: Result<UxDataSurface> = serde_json::from_value(serde_json::json!({
        "surface": "board", "tools": ["list_tasks"], "collection": "Task"
    }))
    .map_err(Into::into);
    assert!(
        unknown.is_err(),
        "a data surface takes surface and tools only"
    );
}

#[test]
fn ux_data_surfaces_and_tools_are_checked_by_name() {
    let error = with_data(vec![surface("board", &["List-Tasks"])])
        .validate(&no_plugins())
        .unwrap_err();
    let error = format!("{error:#}");
    assert!(error.contains("\"List-Tasks\""), "{error}");
    assert!(error.contains("\"board\""), "{error}");
    assert!(error.contains("starting with a letter"), "{error}");
    for bad in ["", "9tasks", "a b", &"a".repeat(65)] {
        assert!(
            with_data(vec![surface("board", &[bad])])
                .validate(&no_plugins())
                .is_err(),
            "{bad:?} must be refused"
        );
    }
    with_data(vec![surface(
        "board",
        &["a", "list_tasks_2", &"a".repeat(64)],
    )])
    .validate(&no_plugins())
    .expect("snake_case tool names up to 64 characters");

    let error = with_data(vec![
        surface("board", &["list_tasks"]),
        surface("board", &["add_task"]),
    ])
    .validate(&no_plugins())
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("data surface \"board\" twice"),
        "{error:#}"
    );

    let error = with_data(vec![surface("board", &["list_tasks", "list_tasks"])])
        .validate(&no_plugins())
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("tool \"list_tasks\" on data surface \"board\" twice"),
        "{error:#}"
    );

    let error = with_data(vec![surface("board", &[])])
        .validate(&no_plugins())
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("data surface \"board\" with no tools"),
        "{error:#}"
    );

    for bad in ["", "my board", "board\n", &"s".repeat(129)] {
        let error = with_data(vec![surface(bad, &["list_tasks"])])
            .validate(&no_plugins())
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("a surface id is non-empty"),
            "{bad:?}: {error:#}"
        );
    }
    with_data(vec![surface(&"s".repeat(128), &["list_tasks"])])
        .validate(&no_plugins())
        .expect("128 characters is the ceiling");
}

#[test]
fn a_ux_plugin_needs_exactly_one_producer() {
    valid_ux()
        .validate(&no_plugins())
        .expect("entry alone is fine");
    let both = PackUxPlugin {
        plugin: Some("gen".to_owned()),
        ..valid_ux()
    };
    let error = both.validate(&BTreeSet::from(["gen"])).unwrap_err();
    assert!(
        format!("{error:#}").contains("both entry and plugin"),
        "{error:#}"
    );
    let neither = PackUxPlugin {
        entry: None,
        ..valid_ux()
    };
    let error = neither.validate(&no_plugins()).unwrap_err();
    assert!(format!("{error:#}").contains("neither"), "{error:#}");
}

#[test]
fn a_ux_entry_lives_under_ux_and_is_a_js_file() {
    for entry in [
        "plugins/board.js",
        "ux/board/plugin.ts",
        "ux/Board/plugin.js",
        "../x.js",
    ] {
        let plugin = PackUxPlugin {
            entry: Some(entry.to_owned()),
            ..valid_ux()
        };
        assert!(
            plugin.validate(&no_plugins()).is_err(),
            "{entry} must be refused"
        );
    }
}

#[test]
fn an_afb_produced_ux_plugin_names_a_declared_plugin_and_ships_no_css() {
    let produced = PackUxPlugin {
        entry: None,
        plugin: Some("report_ui".to_owned()),
        ..valid_ux()
    };
    produced
        .validate(&BTreeSet::from(["report_ui"]))
        .expect("declared");
    let error = produced.validate(&no_plugins()).unwrap_err();
    assert!(
        format!("{error:#}").contains("does not declare"),
        "{error:#}"
    );
    let styled = PackUxPlugin {
        css: Some("ux/report/plugin.css".to_owned()),
        ..produced
    };
    let error = styled.validate(&BTreeSet::from(["report_ui"])).unwrap_err();
    assert!(format!("{error:#}").contains("css"), "{error:#}");
}

#[test]
fn ux_areas_and_directives_are_checked_by_name() {
    let unknown_area = PackUxPlugin {
        contributes: UxContributions {
            areas: vec!["statusbar.right".to_owned()],
            directives: vec![],
        },
        ..valid_ux()
    };
    let error = unknown_area.validate(&no_plugins()).unwrap_err();
    assert!(
        format!("{error:#}").contains("statusbar.right"),
        "{error:#}"
    );
    assert!(format!("{error:#}").contains("known areas"), "{error:#}");

    let no_areas = PackUxPlugin {
        contributes: UxContributions::default(),
        ..valid_ux()
    };
    assert!(no_areas.validate(&no_plugins()).is_err());

    let bad_directive = PackUxPlugin {
        contributes: UxContributions {
            areas: vec!["transcript.directives".to_owned()],
            directives: vec!["Board".to_owned()],
        },
        ..valid_ux()
    };
    assert!(bad_directive.validate(&no_plugins()).is_err());

    let directive_without_area = PackUxPlugin {
        contributes: UxContributions {
            areas: vec!["nav".to_owned()],
            directives: vec!["board".to_owned()],
        },
        ..valid_ux()
    };
    let error = directive_without_area.validate(&no_plugins()).unwrap_err();
    assert!(
        format!("{error:#}").contains("transcript.directives"),
        "{error:#}"
    );

    let good = PackUxPlugin {
        contributes: UxContributions {
            areas: vec!["transcript.directives".to_owned()],
            directives: vec!["board".to_owned(), "board-2".to_owned()],
        },
        ..valid_ux()
    };
    good.validate(&no_plugins())
        .expect("declared directives in a declared area");
}

fn documents_manifest(dependencies: Vec<&str>) -> PackManifest {
    serde_json::from_value(serde_json::json!({
        "manifest_version": 1,
        "name": "dep_test",
        "version": "1.0.0",
        "description": "a test documents pack",
        "authors": ["gents"],
        "kind": "documents",
        "assets": ["README.md", "pack_config.json"],
        "config": "pack_config.json",
        "dependencies": dependencies,
    }))
    .unwrap()
}

fn ux_pack(ux: Vec<serde_json::Value>, assets: Vec<&str>) -> PackManifest {
    serde_json::from_value(serde_json::json!({
        "manifest_version": 1,
        "name": "boardpack",
        "version": "1.0.0",
        "description": "a board",
        "authors": ["tests"],
        "kind": "plugins",
        "assets": assets,
        "plugins": [serde_json::to_value(valid_plugin()).unwrap()],
        "ux": ux,
    }))
    .unwrap()
}

#[test]
fn a_manifest_requires_ux_files_to_be_declared_assets_and_unique_names() {
    let ux = serde_json::to_value(valid_ux()).unwrap();
    validate_pack_manifest(&ux_pack(
        vec![ux.clone()],
        vec![
            "README.md",
            "plugins/format_check.afb",
            "ux/board/plugin.js",
        ],
    ))
    .expect("entry declared as an asset");

    let error = validate_pack_manifest(&ux_pack(
        vec![ux.clone()],
        vec!["README.md", "plugins/format_check.afb"],
    ))
    .unwrap_err();
    assert!(format!("{error:#}").contains("not its entry"), "{error:#}");

    let error = validate_pack_manifest(&ux_pack(
        vec![ux.clone(), ux],
        vec![
            "README.md",
            "plugins/format_check.afb",
            "ux/board/plugin.js",
        ],
    ))
    .unwrap_err();
    assert!(format!("{error:#}").contains("twice"), "{error:#}");
}

#[test]
fn document_packs_can_ship_ux_as_declared_assets() {
    let ux = serde_json::to_value(valid_ux()).unwrap();
    let mut manifest = documents_manifest(vec![]);
    manifest.metadata.ux = vec![serde_json::from_value(ux).unwrap()];
    manifest
        .metadata
        .assets
        .push("ux/board/plugin.js".to_owned());
    validate_pack_manifest(&manifest).expect("a document pack may include UX assets");
}

#[test]
fn an_older_manifest_without_ux_still_parses_and_a_record_round_trips() {
    let manifest = ux_pack(vec![], vec!["README.md", "plugins/format_check.afb"]);
    assert!(manifest.metadata.ux.is_empty());
    let json = serde_json::to_value(&manifest).unwrap();
    assert!(
        json.get("ux").is_none(),
        "an empty ux list is not written out"
    );
    let back: PackManifest = serde_json::from_value(json).unwrap();
    assert!(back.metadata.ux.is_empty());
}

#[test]
fn retired_graph_kind_is_refused_with_next_step() {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "manifest_version": 1,
        "name": "code_review",
        "version": "1.7.0",
        "description": "a retired graph pack",
        "kind": "graph",
        "namespace": "acme",
        "authors": ["a"],
        "compiler_version": "1"
    }))
    .unwrap();
    let error = format!("{:#}", PackManifest::from_slice(&bytes).unwrap_err());
    assert!(
        error.contains(
            "graph packs are retired; run `gents pack remove acme/code_review` and install the 2.x version"
        ),
        "{error}"
    );
}

#[test]
fn directive_names_follow_the_desktop_parser() {
    for ok in ["a", "board", "board-2", "x9"] {
        assert!(is_valid_directive_name(ok), "{ok}");
    }
    for bad in ["", "Board", "9x", "a_b", "a b", &"a".repeat(65)] {
        assert!(!is_valid_directive_name(bad), "{bad:?}");
    }
}
