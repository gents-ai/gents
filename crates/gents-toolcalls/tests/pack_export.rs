use std::collections::BTreeMap;

use serde_json::{json, Value};

use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_pack::pack::archive::{pack_dir, PackArchive};
use gents_pack::pack::authoring::export::{export_pack, ExportSelection};
use gents_pack::pack::{
    install_prepared_document_pack, prepare_document_pack_archive_with_runtime, DriftPolicy,
    InstallReport, InstalledVia,
};
use gents_store::config_client::ConfigAccess;
use gents_store::{collection::Collection, identity::NodeIdentity as _};

async fn apply(access: &ConfigAccess, documents: Vec<(Collection, Value)>) {
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .unwrap();
    access
        .transact_write("test.pack_export.apply", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
}

async fn install_in(
    db: &gents_inference::test_support::TestDb,
    user: &gents_store::identity::KeyIdentity,
    access: &ConfigAccess,
    archive: &PackArchive,
    bindings: &BTreeMap<String, String>,
    plugin_home: Option<&std::path::Path>,
    variables: &BTreeMap<String, String>,
) -> anyhow::Result<InstallReport> {
    let runtime = db.node_identity.clone();
    let local = ConfigAccess::local_as(db.node.clone(), identity::Did::new(user.did()).unwrap());
    let prepared = prepare_document_pack_archive_with_runtime(
        &local,
        runtime.did(),
        archive,
        bindings,
        &[],
        plugin_home,
        &|name| variables.get(name).cloned(),
        runtime.as_ref(),
        &Default::default(),
    )
    .await?;
    install_prepared_document_pack(
        access,
        runtime.did(),
        archive,
        &[],
        &prepared,
        plugin_home,
        false,
        InstalledVia::Operator,
        DriftPolicy::Refuse,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
}

async fn rows(access: &ConfigAccess, query: &str) -> Value {
    access.execute(query).await.unwrap()["data"].clone()
}

#[tokio::test]
async fn a_plugin_built_on_the_node_exports_as_the_packs_plugin_and_runs_where_it_installs() {
    let (origin, _user, access, worker) = gents_inference::test_support::signed_test_node(
        "pack-export-plugin",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let owner = origin.node_identity.did().to_owned();
    let tools_id = format!("{}:tools", worker.agent_did);
    let source = "module.exports = (input) => ({ n: input.n * 2 });\n";
    apply(
        &access,
        vec![
            (
                Collection::PluginSource,
                json!({"node_did":owner,"plugin_id":"double","language":"js","description":"Doubles n.","source":source,
                    "input_schema":{"type":"object","properties":{"n":{"type":"number"}}},
                    "tests":{"cases":[{"name":"two","input":{"n":2},"expect":{"n":4}}]}}),
            ),
            (
                Collection::Tools,
                json!({"node_did":owner,"tools_id":tools_id,"integrations":{"plugins":[
                    {"plugin":"node/double","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000"}
                ]}}),
            ),
            (
                Collection::AgentContext,
                json!({"node_did":owner,"context_id":format!("{}:context", worker.agent_did),"tools_id":tools_id,"system_prompt":"You double numbers.\n"}),
            ),
        ],
    )
    .await;
    let out = tempfile::tempdir().unwrap();
    let selection = |documents: Vec<(Collection, String)>| ExportSelection {
        documents: documents.into_iter().collect(),
        ..ExportSelection::default()
    };
    let report = export_pack(
        &access,
        &owner,
        &out.path().join("doubler"),
        "doubler",
        "fixture",
        &selection(vec![(Collection::Agent, worker.agent_did.clone())]),
    )
    .await
    .unwrap();
    assert_eq!(report.plugins, ["fixture/double"]);
    assert!(
        report.by_reference.iter().any(|pulled| {
            pulled.part == "PluginSource/double"
                && pulled.via == format!("Tools/{tools_id} integrations.plugins")
        }),
        "{:?}",
        report.by_reference
    );
    assert_eq!(report.check.unbuilt, ["double"], "{:?}", report.check);
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(report.dir.join("pack_config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        config["tools"][0]["integrations"]["plugins"],
        json!([{"plugin": "fixture/double"}]),
        "the Tools select the pack's plugin, unpinned: {config}"
    );
    assert_eq!(
        config["plugin_sources"],
        Value::Null,
        "the pack's plugin directory is the one copy of the source: {config}"
    );
    let manifest = gents_pack::pack::authoring::read_manifest(&report.dir).unwrap();
    let plugin = &manifest.metadata.plugins[0];
    assert_eq!(
        (plugin.name.as_str(), plugin.language.as_str()),
        ("double", "js")
    );
    assert_eq!(plugin.description, "Doubles n.");
    assert_eq!(plugin.input_schema["properties"]["n"]["type"], "number");
    assert_eq!(
        gents_pack::pack::authoring::plugins_with_source(&manifest),
        ["double"]
    );
    assert_eq!(
        std::fs::read_to_string(report.dir.join("plugins/double/source/main.js")).unwrap(),
        source
    );
    let case: Value = serde_json::from_str(
        &std::fs::read_to_string(report.dir.join("plugins/double/tests/01_two.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(case, json!({"input":{"n":2},"expect":{"n":4}}));

    // `pack build` compiles the source; a prebuilt artifact stands in for it.
    std::fs::write(
        report.dir.join(&plugin.artifact),
        gents_pack::plugin::test_fixtures::build_plugin_afb(
            &gents_pack::plugin::test_fixtures::constant_output_wat(br#"{"n":4}"#),
        ),
    )
    .unwrap();
    let check = gents_pack::pack::authoring::check_dir(&report.dir).await;
    assert!(
        check.problems.is_empty() && check.unbuilt.is_empty(),
        "{check:#?}"
    );
    let (bytes, _) = pack_dir(&report.dir).unwrap();
    let archive = PackArchive::from_bytes(&bytes).unwrap();

    let (second, second_user, second_access) =
        gents_inference::test_support::signed_test_node_without_agent(
            "pack-export-plugin-second",
            gents_session::test_support::no_lifecycle_plugins(),
        )
        .await;
    let second_owner = second.node_identity.did().to_owned();
    apply(
        &second_access,
        vec![
            (
                Collection::InferenceBackend,
                json!({"node_did":second_owner,"backend_id":"backend","name":"Test","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}}),
            ),
            (
                Collection::InferenceProfile,
                json!({"node_did":second_owner,"profile_id":"profile","backend_id":"backend","model_name":"test-model"}),
            ),
        ],
    )
    .await;
    let home = tempfile::tempdir().unwrap();
    let slot = report.inference_slots[0]["name"].as_str().unwrap();
    install_in(
        &second,
        &second_user,
        &second_access,
        &archive,
        &BTreeMap::from([(slot.to_owned(), "profile".to_owned())]),
        Some(home.path()),
        &BTreeMap::new(),
    )
    .await
    .unwrap();
    let live = rows(&second_access, "{ Tools { integrations } }").await;
    assert_eq!(
        live["Tools"][0]["integrations"]["plugins"][0]["plugin"], "fixture/double",
        "{live}"
    );
    let executor =
        gents_toolcalls::plugin::executor::PluginExecutor::new(Some(home.path().to_owned()));
    let installed = executor.resolve("fixture/double", None).unwrap();
    let call = executor.call(&installed, json!({"n": 2})).await.unwrap();
    assert_eq!(call.coordinate, "fixture/double");
    assert!(
        format!("{:?}", call.outcome).contains("4"),
        "{:?}",
        call.outcome
    );

    // A selection with no source on the node, and one another pack ships,
    // are each refused by name.
    apply(
        &access,
        vec![(
            Collection::Tools,
            json!({"node_did":owner,"tools_id":tools_id,"integrations":{"plugins":[
                {"plugin":"node/absent"},{"plugin":"gents/web_search"}
            ]}}),
        )],
    )
    .await;
    let refused = export_pack(
        &access,
        &owner,
        &out.path().join("refused"),
        "refused",
        "fixture",
        &selection(vec![(Collection::Tools, tools_id.clone())]),
    )
    .await
    .unwrap_err();
    let refused = format!("{refused:#}");
    assert!(
        refused.contains("selects plugin node/absent, and PluginSource/absent"),
        "{refused}"
    );
    assert!(
        refused.contains("selects plugin gents/web_search, which an installed pack ships"),
        "{refused}"
    );
}
