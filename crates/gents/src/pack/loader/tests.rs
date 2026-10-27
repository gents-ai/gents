use super::*;
use serde_json::json;

fn manifest() -> PackManifest {
    serde_json::from_value(json!({
        "manifest_version":1,"name":"example","version":"1","description":"Example",
        "kind":"documents","authors":["Example"],
        "assets":["README.md","config/bundle.json","config/prompt.md"],
        "config":"config/bundle.json",
    }))
    .unwrap()
}

fn load(value: Value) -> Result<PackConfig> {
    load_pack_config(
        &manifest(),
        &PackInstallOptions {
            node_did: "did:key:owner".into(),
        },
        &|path| match path {
            "config/bundle.json" => Ok(serde_json::to_vec(&value)?),
            "config/prompt.md" => Ok(b"literal {{ task.input }} ${HOST_SHELL}".to_vec()),
            _ => anyhow::bail!("unexpected asset {path}"),
        },
        &|name| (name == "TEXT").then(|| "quoted \"value\"\nsecond line".into()),
    )
}

#[test]
fn document_loading_applies_scope_defaults_and_literal_sidecars() {
    let value = json!({
        "node":{"created_by":"did:key:creator"},
        "contexts":[{"context_id":"work","system_prompt":"./prompt.md"}],
        "tasks":[{"task_id":"review","agent_id":"reviewer","prompt_template":"./prompt.md"}],
        "agent_targets":[{"target_id":"remote","name":"remote","target_node_did":"did:key:foreign","agent_id":"worker"}],
        "tools":null
    });
    let document = load(value).unwrap();
    assert_eq!(document.node.node_did, "did:key:owner");
    assert_eq!(document.node.created_by.as_deref(), Some("did:key:creator"));
    assert_eq!(document.agent_targets[0].node_did, "did:key:owner");
    assert_eq!(document.agent_targets[0].target_node_did, "did:key:foreign");
    assert_eq!(document.tasks[0].node_did, "did:key:owner");
    assert!(document.tasks[0].enabled);
    assert!(document.tools.is_empty());
    assert_eq!(
        document.contexts[0].system_prompt.as_deref(),
        Some("literal {{ task.input }} ${HOST_SHELL}")
    );
    assert_eq!(
        document.tasks[0].prompt_template,
        "literal {{ task.input }} ${HOST_SHELL}"
    );
}

#[test]
fn interpolation_cannot_change_json_structure_and_preserves_task_braces() {
    let config = load(json!({"node":{},"tasks":[{
        "task_id":"review","agent_id":"reviewer",
        "prompt_template":"${TEXT} {{ task.input }} $${LITERAL} ${MISSING:-fallback}"
    }]}))
    .unwrap();
    assert_eq!(
        config.tasks[0].prompt_template,
        "quoted \"value\"\nsecond line {{ task.input }} ${LITERAL} fallback"
    );
    assert!(load(json!({"node":{"display_name":"${UNSET}"}})).is_err());
}

#[test]
fn explicit_owner_mismatch_and_unknown_authoring_keys_fail() {
    for owner in [json!("did:key:foreign"), json!(null), json!("")] {
        assert!(load(json!({"node":{"node_did":owner}})).is_err());
        assert!(
            load(json!({"node":{},"contexts":[{"node_did":owner,"context_id":"work"}]})).is_err()
        );
    }
    assert!(load(json!({"node":{},"old_tool_selections":[]})).is_err());
    assert!(load(
        json!({"node":{},"contexts":[{"context_id":"work","request_context_template":"removed"}]})
    )
    .is_err());
}

#[test]
fn sidecar_cannot_escape_or_read_undeclared_assets() {
    for path in [
        "./../private.md",
        "./missing.md",
        "./nested/../../private.md",
        "./prompt\\other.md",
    ] {
        let error =
            load(json!({"node":{},"contexts":[{"context_id":"work","system_prompt":path}]}))
                .unwrap_err();
        assert!(
            !format!("{error:#}").contains("unexpected asset"),
            "resolver must not receive forbidden path: {error:#}"
        );
    }
    let mut undeclared = manifest();
    undeclared.config = Some("private.json".into());
    let result = load_pack_config(
        &undeclared,
        &PackInstallOptions {
            node_did: "did:key:owner".into(),
        },
        &|_| panic!("invalid manifest must fail before read"),
        &|_| None,
    );
    assert!(result.is_err());
}

#[test]
fn plugin_documents_fixture_loads_slot_authoring_and_literal_prompt_assets() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/packs/plugin_documents_fixture");
    let manifest: PackManifest =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let config = load_pack_config(
        &manifest,
        &PackInstallOptions {
            node_did: "did:key:review-owner".into(),
        },
        &|path| Ok(std::fs::read(root.join(path))?),
        &|_| None,
    )
    .unwrap();
    assert!(config.inference_backends.is_empty());
    assert!(config.inference_profiles.is_empty());
    assert_eq!(
        config.agents[0].inference_profile_id,
        "gents:inference-slot:coordinator"
    );
    let authored: Value =
        serde_json::from_slice(&std::fs::read(root.join("pack_config.json")).unwrap()).unwrap();
    for (field, prompt_field, resolved) in [
        (
            "contexts",
            "system_prompt",
            config
                .contexts
                .iter()
                .map(|row| row.system_prompt.as_deref().unwrap())
                .collect::<Vec<_>>(),
        ),
        (
            "tasks",
            "prompt_template",
            config
                .tasks
                .iter()
                .map(|row| row.prompt_template.as_str())
                .collect::<Vec<_>>(),
        ),
    ] {
        for (row, prompt) in authored[field].as_array().unwrap().iter().zip(resolved) {
            let path = row[prompt_field]
                .as_str()
                .unwrap()
                .strip_prefix("./")
                .unwrap();
            assert_eq!(prompt, std::fs::read_to_string(root.join(path)).unwrap());
        }
    }
}

#[test]
fn every_fixture_pack_that_ships_a_config_loads_it_cleanly() {
    for name in crate::support::fixtures::fixture_pack_names() {
        let (_guard, copy) = crate::test_support::fixture_pack_copy(&name, &json!({}));
        let root = copy;
        let manifest: PackManifest =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap())
                .unwrap_or_else(|error| panic!("{name} manifest: {error}"));
        crate::pack::validate_manifest(&manifest.name, &manifest)
            .unwrap_or_else(|error| panic!("{name} manifest: {error:#}"));
        if manifest.config.is_none() {
            continue;
        }
        load_pack_config(
            &manifest,
            &PackInstallOptions {
                node_did: "did:key:fixture-owner".into(),
            },
            &|path| Ok(std::fs::read(root.join(path))?),
            &|_| None,
        )
        .unwrap_or_else(|error| panic!("{name} config: {error:#}"));
    }
}

fn eval_manifest(assets: &[&str]) -> PackManifest {
    serde_json::from_value(json!({
        "manifest_version": 1, "name": "example", "version": "1", "description": "Example",
        "kind": "documents", "authors": ["Example"], "assets": assets,
        "config": "config/bundle.json",
    }))
    .unwrap()
}

fn eval_config(cases: Value) -> Value {
    json!({"node": {}, "eval_definitions": [{
        "definition_id": "defn",
        "comparability_version": 1,
        "subject": {"kind": "agent"},
        "cases": cases,
    }]})
}

const SIDECAR_CASE: &str = r#"{"case_id":"one","split":"train","stages":[{"stage_id":"s","prompt":"literal ${NOT_INTERPOLATED}","deadline_secs":60,"checks":[{"check":"finding_count","params":{"count":1},"tier":"acceptance"}]}]}"#;

fn load_eval(manifest: &PackManifest, config: Value) -> Result<PackConfig> {
    load_pack_config(
        manifest,
        &PackInstallOptions {
            node_did: "did:key:owner".into(),
        },
        &|path| match path {
            "config/bundle.json" => Ok(serde_json::to_vec(&config)?),
            "config/cases/one.json" => Ok(SIDECAR_CASE.as_bytes().to_vec()),
            "config/cases/bad.json" => {
                Ok(br#"{"case_id":"bad","split":"train","stages":[],"surprise":1}"#.to_vec())
            }
            _ => anyhow::bail!("unexpected asset {path}"),
        },
        &|_| None,
    )
}

#[test]
fn eval_definition_case_sidecars_hydrate_as_literal_cases_beside_inline_ones() {
    let manifest = eval_manifest(&["README.md", "config/bundle.json", "config/cases/one.json"]);
    let inline = json!({"case_id": "two", "split": "validation", "stages": [{
        "stage_id": "s", "prompt": "p", "deadline_secs": 60,
        "checks": [{"check": "finding_count", "params": {"count": 0}, "tier": "acceptance"}],
    }]});
    let config = load_eval(&manifest, eval_config(json!(["./cases/one.json", inline]))).unwrap();
    let definition = &config.eval_definitions[0];
    assert_eq!(definition.node_did, "did:key:owner");
    assert_eq!(
        definition
            .cases
            .iter()
            .map(|case| case.case_id.as_str())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(
        definition.cases[0].stages[0].prompt, "literal ${NOT_INTERPOLATED}",
        "sidecar contents are never interpolated"
    );
    definition.validate().unwrap();
}

#[test]
fn eval_definition_case_sidecars_refuse_undeclared_escaping_bare_and_malformed_paths() {
    let undeclared = load_eval(
        &eval_manifest(&["README.md", "config/bundle.json"]),
        eval_config(json!(["./cases/one.json"])),
    )
    .unwrap_err();
    assert!(
        format!("{undeclared:#}").contains("undeclared pack sidecar: config/cases/one.json"),
        "{undeclared:#}"
    );

    let manifest = eval_manifest(&[
        "README.md",
        "config/bundle.json",
        "config/cases/one.json",
        "config/cases/bad.json",
    ]);
    for path in [
        "./../escape.json",
        "./cases/../../escape.json",
        "cases/one.json",
    ] {
        let error = load_eval(&manifest, eval_config(json!([path]))).unwrap_err();
        assert!(
            !format!("{error:#}").contains("unexpected asset"),
            "the reader must not receive {path}: {error:#}"
        );
    }
    let malformed = load_eval(&manifest, eval_config(json!(["./cases/bad.json"]))).unwrap_err();
    assert!(
        format!("{malformed:#}").contains("./cases/bad.json is not one EvalCase"),
        "{malformed:#}"
    );
}

#[test]
fn skill_instructions_load_from_a_literal_sidecar() {
    let config = load(json!({
        "node":{},
        "skills":[{"skill_id":"review","name":"review","instructions":"./prompt.md"}]
    }))
    .unwrap();
    assert_eq!(
        config.skills[0].instructions.as_deref(),
        Some("literal {{ task.input }} ${HOST_SHELL}")
    );
}

#[test]
fn a_pack_that_selects_the_default_agent_is_refused() {
    let error = load(json!({"node":{"default_agent_id":"worker"}})).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("must not choose the default agent") && message.contains("worker"),
        "{message}"
    );
    assert!(load(json!({"node":{}})).is_ok());
}
