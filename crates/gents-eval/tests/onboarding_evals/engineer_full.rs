use gents_documents::SurfaceToolDecl;
use gents_pack::pack::core_packs::CorePack;
use gents_pack::pack::{
    declared_paths, load_pack_authoring_config, PackInstallOptions, PackManifest,
};

use super::registry::{definition, fixtures, suites, OWNER};

const FULL_SUBJECT: &str = "engineer_full";

#[test]
fn every_suite_on_the_full_engineer_installs_every_core_pack() {
    let expected: Vec<String> = CorePack::ALL
        .iter()
        .map(|pack| pack.coordinate().to_owned())
        .collect();
    let on_full: Vec<_> = suites()
        .into_iter()
        .filter(|suite| suite.subject == FULL_SUBJECT)
        .collect();
    assert!(
        !on_full.is_empty(),
        "the reference suite runs on {FULL_SUBJECT}"
    );
    for suite in on_full {
        assert_eq!(
            definition(&suite.dir).subject.core_packs,
            expected,
            "{}: the full Engineer carries the core packs init installs, in init's order",
            suite.id
        );
    }
}

#[test]
fn the_full_subject_is_the_engineer_init_provisions_without_host_bash() {
    let dir = fixtures().join(FULL_SUBJECT);
    let manifest: PackManifest =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    for path in declared_paths(&manifest) {
        assert!(dir.join(&path).is_file(), "declares missing {path}");
    }
    let authored = load_pack_authoring_config(
        &manifest,
        &PackInstallOptions {
            node_did: OWNER.into(),
        },
        &|path| Ok(std::fs::read(dir.join(path))?),
        &|_| None,
    )
    .unwrap();
    let [behavior] = authored.value["agents"].as_array().unwrap().as_slice() else {
        panic!("one behavior, the Engineer");
    };
    assert_eq!(behavior["agent_slot"], "engineer");
    assert!(behavior["tags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tag| tag == gents_tools::builtin::self_config::ENGINEER_AGENT_TAG));
    let config = &authored.documents;
    assert_eq!(manifest.metadata.inference_slots.len(), 1);
    assert_eq!(
        config.contexts[0].system_prompt.as_deref(),
        Some(gents_protocol::ENGINEER_PROMPT),
        "agents/engineer/system_prompt.md must equal the shipped Engineer prompt"
    );
    let [tools] = config.tools.as_slice() else {
        panic!("one Tools document");
    };
    assert_eq!(
        tools.self_config,
        Some(gents_tools::builtin::self_config::engineer_self_config()),
        "the shipped self-config grant"
    );
    let host = tools.host.as_ref().unwrap();
    assert_eq!(
        host.root, None,
        "the trial roots host tools at its workspace"
    );
    assert_eq!(
        host.files.as_ref().map(|files| files.mode),
        Some(gents_documents::FileToolMode::ReadWrite)
    );
    assert_eq!(
        host.bash.as_ref().map(|bash| bash.mode),
        Some(gents_documents::BashMode::Off),
        "an embedded trial refuses host bash"
    );
    let agents = tools.agents.as_ref().unwrap();
    assert_eq!(agents.enabled, Some(true));
    assert_eq!(agents.target_ids, ["engineer"]);
    let [target] = config.agent_targets.as_slice() else {
        panic!("one target, the Engineer's own");
    };
    assert_eq!(
        (target.target_id.as_str(), target.agent_did.as_str()),
        ("engineer", "gents:agent-slot:engineer")
    );
    let built_ins = tools.built_ins.as_ref().unwrap();
    assert_eq!(
        (
            built_ins.enable_goal_tools,
            built_ins.enable_goal_creation,
            built_ins.enable_session_history_tool,
            built_ins.enable_schema_tool,
            built_ins.enable_p2p_tool,
            built_ins.enable_p2p_mutations,
            built_ins.enable_context_budget,
        ),
        (
            Some(true),
            Some(true),
            Some(true),
            Some(true),
            Some(true),
            Some(true),
            Some(true)
        ),
        "the built-ins `gents init --engineer` sets"
    );
    let datastore = tools.datastore.as_ref().unwrap();
    assert_eq!(datastore.enable_defra_query, Some(true));
    assert_eq!(
        datastore.datastore_tool_surface_ids.as_deref(),
        Some(&["engineer-mailbox".to_string()][..])
    );
    let [surface] = config.datastore_tool_surfaces.as_slice() else {
        panic!("one surface, engineer-mailbox");
    };
    assert_eq!(
        surface.entries.as_ref(),
        Some(&vec![SurfaceToolDecl::Create(
            gents_documents::mailbox_write_decl::canonical_mailbox_write_decl(),
        )]),
        "the canonical mailbox declaration"
    );
}
