use super::*;
use serde_json::json;

fn manifest(graph: bool) -> PackManifest {
    serde_json::from_value(json!({
        "manifest_version":1,"name":"example","version":"1","description":"Example",
        "kind":if graph { "graph" } else { "documents" },"authors":["Example"],
        "assets":["README.md","config/bundle.json","config/prompt.md"],
        "config":"config/bundle.json",
        "compiler_version": if graph { Some(crate::graph_pipeline::COMPILER_VERSION) } else { None },
    })).unwrap()
}

fn load(value: Value, graph: bool) -> Result<PackConfig> {
    load_pack_config(
        &manifest(graph),
        &PackInstallOptions {
            agent_did: "did:key:owner".into(),
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
fn explicit_pack_caller_uses_selected_owner_and_cannot_be_spoofed_by_environment() {
    let value = json!({"agent_principal":{},"graph_capabilities":[{
        "capability_id":"review","revision":"1","target":{"kind":"task","task_id":"review"},
        "allowed_callers":["${GENTS_PACK_AGENT_DID}"]
    },{
        "capability_id":"closed","revision":"1","target":{"kind":"task","task_id":"review"}
    }]});
    let config = load_pack_config(
        &manifest(true),
        &PackInstallOptions {
            agent_did: "did:key:selected".into(),
        },
        &|_| Ok(serde_json::to_vec(&value)?),
        &|_| Some("did:key:spoofed".into()),
    )
    .unwrap();
    assert_eq!(
        config.graph_capabilities[0].allowed_callers,
        ["did:key:selected"]
    );
    assert!(
        config.graph_capabilities[1].allowed_callers.is_empty(),
        "owner binding must not invent grants"
    );
}

#[test]
fn document_and_graph_loading_share_scope_defaults_and_literal_sidecars() {
    let value = json!({
        "agent_principal":{"created_by":"did:key:creator"},
        "contexts":[{"context_id":"work","system_prompt":"./prompt.md"}],
        "tasks":[{"task_id":"review","behavior_id":"reviewer","prompt_template":"./prompt.md"}],
        "subagent_targets":[{"target_id":"remote","name":"remote","target_agent_did":"did:key:foreign","behavior_id":"worker"}],
        "tools":null
    });
    let document = load(value.clone(), false).unwrap();
    let graph = load(value, true).unwrap();
    assert_eq!(
        serde_json::to_value(&document).unwrap(),
        serde_json::to_value(graph).unwrap()
    );
    assert_eq!(document.agent_principal.agent_did, "did:key:owner");
    assert_eq!(
        document.agent_principal.created_by.as_deref(),
        Some("did:key:creator")
    );
    assert_eq!(document.subagent_targets[0].agent_did, "did:key:owner");
    assert_eq!(
        document.subagent_targets[0].target_agent_did,
        "did:key:foreign"
    );
    assert_eq!(document.tasks[0].agent_did, "did:key:owner");
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
    let config = load(
        json!({"agent_principal":{},"tasks":[{
            "task_id":"review","behavior_id":"reviewer",
            "prompt_template":"${TEXT} {{ task.input }} $${LITERAL} ${MISSING:-fallback}"
        }]}),
        false,
    )
    .unwrap();
    assert_eq!(
        config.tasks[0].prompt_template,
        "quoted \"value\"\nsecond line {{ task.input }} ${LITERAL} fallback"
    );
    assert!(load(
        json!({"agent_principal":{"display_name":"${UNSET}"}}),
        false
    )
    .is_err());
}

#[test]
fn explicit_owner_mismatch_and_unknown_authoring_keys_fail() {
    for owner in [json!("did:key:foreign"), json!(null), json!("")] {
        assert!(load(json!({"agent_principal":{"agent_did":owner}}), false).is_err());
        assert!(load(
            json!({"agent_principal":{},"contexts":[{"agent_did":owner,"context_id":"work"}]}),
            false
        )
        .is_err());
    }
    assert!(load(
        json!({"agent_principal":{},"old_tool_selections":[]}),
        false
    )
    .is_err());
    assert!(load(json!({"agent_principal":{},"contexts":[{"context_id":"work","request_context_template":"removed"}]}), false).is_err());
}

#[test]
fn sidecar_cannot_escape_or_read_undeclared_assets() {
    for path in [
        "./../private.md",
        "./missing.md",
        "./nested/../../private.md",
        "./prompt\\other.md",
    ] {
        let error = load(
            json!({"agent_principal":{},"contexts":[{"context_id":"work","system_prompt":path}]}),
            false,
        )
        .unwrap_err();
        assert!(
            !format!("{error:#}").contains("unexpected asset"),
            "resolver must not receive forbidden path: {error:#}"
        );
    }
    let mut undeclared = manifest(false);
    undeclared.config = Some("private.json".into());
    let result = load_pack_config(
        &undeclared,
        &PackInstallOptions {
            agent_did: "did:key:owner".into(),
        },
        &|_| panic!("invalid manifest must fail before read"),
        &|_| None,
    );
    assert!(result.is_err());
}

#[test]
fn bundled_review_loads_slot_authoring_and_literal_prompt_assets() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/code_review");
    let manifest: PackManifest =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let config = load_pack_config(
        &manifest,
        &PackInstallOptions {
            agent_did: "did:key:review-owner".into(),
        },
        &|path| Ok(std::fs::read(root.join(path))?),
        &|_| None,
    )
    .unwrap();
    assert!(config.inference_backends.is_empty());
    assert!(config.inference_profiles.is_empty());
    assert_eq!(
        config.agent_behaviors[0].inference_profile_id,
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
fn skill_instructions_load_from_a_literal_sidecar() {
    let config = load(
        json!({
            "agent_principal":{},
            "skills":[{"skill_id":"review","name":"review","instructions":"./prompt.md"}]
        }),
        false,
    )
    .unwrap();
    assert_eq!(
        config.skills[0].instructions.as_deref(),
        Some("literal {{ task.input }} ${HOST_SHELL}")
    );
}

fn plugin_pack_config(target: Value) -> Result<PackConfig> {
    let manifest: PackManifest = serde_json::from_value(json!({
        "manifest_version":1,"name":"example","namespace":"team","version":"1",
        "description":"Example","kind":"graph","authors":["Example"],
        "assets":["README.md","config/bundle.json","plugins/lint.afb"],
        "config":"config/bundle.json",
        "compiler_version": crate::graph_pipeline::COMPILER_VERSION,
        "plugins":[{"name":"lint","description":"Lints a diff","artifact":"plugins/lint.afb",
                    "language":"rust","input_schema":{"type":"object"}}],
    }))
    .unwrap();
    let config = json!({"agent_principal":{},"graph_capabilities":[{
        "capability_id":"lint","revision":"1","target":target,
    }]});
    load_pack_config(
        &manifest,
        &PackInstallOptions {
            agent_did: "did:key:owner".into(),
        },
        &|path| match path {
            "config/bundle.json" => Ok(serde_json::to_vec(&config)?),
            "plugins/lint.afb" => Ok(b"artifact bytes".to_vec()),
            _ => anyhow::bail!("unexpected asset {path}"),
        },
        &|_| None,
    )
}

fn shipped_digest() -> String {
    format!(
        "sha256:{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(b"artifact bytes")
    )
}

#[test]
fn a_plugin_node_runs_the_packs_own_artifact() {
    let config = plugin_pack_config(json!({"kind":"plugin","plugin":"lint"})).unwrap();
    assert_eq!(
        config.graph_capabilities[0].target,
        crate::graph_pipeline::StageTarget::Plugin {
            plugin: "team/lint".into(),
            digest: Some(shipped_digest()),
            max_attempts: None,
        }
    );
    // An author's pin that matches the shipped artifact is kept.
    plugin_pack_config(json!({"kind":"plugin","plugin":"lint","digest":shipped_digest()})).unwrap();
}

#[test]
fn a_plugin_node_the_pack_cannot_run_is_refused() {
    let undeclared = plugin_pack_config(json!({"kind":"plugin","plugin":"missing"})).unwrap_err();
    assert!(format!("{undeclared:#}").contains("does not declare"));
    let wrong_pin = plugin_pack_config(
        json!({"kind":"plugin","plugin":"lint","digest":format!("sha256:{}", "0".repeat(64))}),
    )
    .unwrap_err();
    assert!(format!("{wrong_pin:#}").contains("the pack ships"));
    // A plugin from outside the pack keeps its author's coordinate and pin.
    let external = plugin_pack_config(json!({"kind":"plugin","plugin":"other/tool"})).unwrap();
    assert_eq!(
        external.graph_capabilities[0].target,
        crate::graph_pipeline::StageTarget::Plugin {
            plugin: "other/tool".into(),
            digest: None,
            max_attempts: None,
        }
    );
}
