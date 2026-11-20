use super::*;

fn slots() -> Vec<(String, Option<String>)> {
    vec![
        ("worker".into(), None),
        ("lead".into(), Some("did:key:lead".into())),
    ]
}

#[test]
fn a_slot_id_splits_into_its_slot_and_a_bound_slot_into_its_agent() {
    assert_eq!(
        split("did:pack:slot:worker:tools", &slots()),
        Some(vec![
            IdPart::Slot("worker".into()),
            IdPart::Text(":tools".into())
        ])
    );
    assert_eq!(
        split("did:pack:slot:lead:tools", &slots()),
        Some(vec![IdPart::Text("did:key:lead:tools".into())])
    );
    assert_eq!(
        split("plain", &slots()),
        Some(vec![IdPart::Text("plain".into())])
    );
}

#[test]
fn a_stand_in_naming_no_slot_or_two_derives_nothing() {
    assert_eq!(split("did:pack:slot:other", &slots()), None);
    let overlapping = vec![("a".into(), None), ("ab".into(), None)];
    assert_eq!(split("did:pack:slot:ab", &overlapping), None);
}

#[test]
fn a_job_fills_its_targets_with_the_wired_agent_only() {
    let settings: PluginJobSettings =
        serde_json::from_value(serde_json::json!({"agent_targets": []})).unwrap();
    let yields = PackYield {
        jobs: BTreeMap::from([(
            "acme/tool".into(),
            YieldedJob {
                agent_targets: vec![vec![IdPart::Slot("worker".into())]],
                settings: settings.clone(),
            },
        )]),
        ..PackYield::default()
    };
    assert_eq!(
        yields.job("acme/tool", "did:key:agent"),
        Some(Some(PluginJobSettings {
            agent_targets: vec!["did:key:agent".into()],
            ..settings
        }))
    );
    assert_eq!(yields.job("acme/other", "did:key:agent"), Some(None));
}

fn slotted_pack() -> ResolvedPack {
    let directory = tempfile::tempdir().unwrap();
    let write = |name: &str, value: &serde_json::Value| {
        std::fs::write(
            directory.path().join(name),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
    };
    write(
        "manifest.json",
        &serde_json::json!({
            "manifest_version":1,"name":"slotted","namespace":"fixture","version":"1.0.0",
            "description":"Slot fixture","kind":"documents","authors":["Fixture"],
            "assets":["README.md","config.json"],"config":"config.json",
            "inference_slots":[{"name":"model","description":"Model","agents":["worker"]}]
        }),
    );
    write(
        "config.json",
        &serde_json::json!({"node":{},"agents":[{
            "agent_slot":"worker","display_name":"Worker",
            "inference_profile_id":"gents:inference-slot:model"
        }],
        "tools":[{"tools_id":"${GENTS_PACK_AGENT_DID.worker}:tools"}],
        "agent_targets":[{"target_id":"worker-target","name":"Worker",
            "target_node_did":"${GENTS_PACK_NODE_DID}","agent_did":"gents:agent-slot:worker"}]}),
    );
    std::fs::write(
        directory.path().join("README.md"),
        "# Fixture\n\n## Usage\nFixture",
    )
    .unwrap();
    let (bytes, _) = crate::pack::archive::pack_dir(directory.path()).unwrap();
    ResolvedPack::received(
        crate::pack::archive::PackArchive::from_bytes(&bytes).unwrap(),
        crate::pack::PackSource::Installed,
    )
    .unwrap()
}

#[test]
fn an_id_named_after_a_provisioned_slot_is_approved_as_that_slot() {
    let yields = derive(
        &slotted_pack(),
        "fixture/slotted",
        "did:key:node",
        &PackVariables::default(),
    )
    .unwrap();
    assert_eq!(
        yields.tools,
        vec![vec![
            IdPart::Slot("worker".into()),
            IdPart::Text(":tools".into())
        ]]
    );
    assert_eq!(
        yields.agent_targets,
        vec![vec![IdPart::Text("worker-target".into())]]
    );
    assert_eq!(
        fill_id(Some("did:key:agent"), &yields.tools[0]).as_deref(),
        Some("did:key:agent:tools")
    );
    assert_eq!(fill_id(None, &yields.tools[0]), None);
}

#[test]
fn an_id_named_after_a_bound_slot_is_approved_as_that_agent() {
    let mut variables = PackVariables::default();
    variables.shared.insert(
        crate::pack::agent_binding_variable("worker"),
        "did:key:bound".into(),
    );
    let yields = derive(
        &slotted_pack(),
        "fixture/slotted",
        "did:key:node",
        &variables,
    )
    .unwrap();
    assert_eq!(
        yields.tools,
        vec![vec![IdPart::Text("did:key:bound:tools".into())]]
    );
}
