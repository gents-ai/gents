use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::{export_pack, ExportSelection};
use crate::pack::archive::{pack_dir, PackArchive};
use crate::pack::{
    install_prepared_document_pack, prepare_document_pack_archive_with_runtime, remove_pack,
    DriftPolicy, InstallReport, InstalledVia,
};
use crate::store::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
use crate::{Collection, NodeIdentity as _};

const SDL: &str = "type Ticket {\n  subject: String @index\n}\ntype TicketNote {\n  ticket: String @index\n  body: String\n}\n";

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
            Box::pin(async move { crate::documents::apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
}

async fn install(
    db: &crate::support::TestDb,
    user: &crate::KeyIdentity,
    access: &ConfigAccess,
    archive: &PackArchive,
    bindings: &BTreeMap<String, String>,
) -> anyhow::Result<InstallReport> {
    let runtime = db.node_identity.clone();
    let local = ConfigAccess::local_as(db.node.clone(), identity::Did::new(user.did()).unwrap());
    let prepared = prepare_document_pack_archive_with_runtime(
        &local,
        runtime.did(),
        archive,
        bindings,
        &[],
        None,
        &|_| None,
        runtime.as_ref(),
    )
    .await?;
    install_prepared_document_pack(
        access,
        runtime.did(),
        archive,
        &[],
        &prepared,
        None,
        false,
        InstalledVia::Operator,
        DriftPolicy::Refuse,
    )
    .await
}

/// Publishing and reading application schemas are node permissions the
/// fixture User does not hold by default.
async fn grant_schema(db: &crate::support::TestDb, user: &crate::KeyIdentity) {
    let admin = ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    );
    let user = identity::Did::new(user.did()).unwrap();
    for permission in [
        crate::store::config_client::NodePermission::CollectionGet,
        crate::store::config_client::NodePermission::CollectionPatch,
    ] {
        admin
            .grant_node_permission(&user, permission)
            .await
            .unwrap();
    }
}

async fn rows(access: &ConfigAccess, query: &str) -> Value {
    access.execute(query).await.unwrap()["data"].clone()
}

#[tokio::test]
async fn a_live_app_exports_installs_on_a_second_node_and_is_adopted_where_it_came_from() {
    let (origin, origin_user, origin_access, worker) =
        crate::test_support::signed_test_node("pack-export-origin").await;
    let owner = origin.node_identity.did().to_owned();
    let context_id = format!("{}:context", worker.agent_did);
    let tools_id = format!("{}:tools", worker.agent_did);
    grant_schema(&origin, &origin_user).await;
    origin_access.add_collection(SDL).await.unwrap();
    apply(
        &origin_access,
        vec![
            (
                Collection::Skill,
                json!({"node_did":owner,"skill_id":"triage","name":"triage","description":"Sort tickets","instructions":"# Triage\n\nSort by urgency; a rush costs $5.\n"}),
            ),
            (
                Collection::DatastoreToolSurface,
                json!({"node_did":owner,"surface_id":"ticket-notes","display_name":"Ticket notes","entries":[
                    {"kind":"query","tool_name":"list_notes","collection":"TicketNote","fields":["ticket","body"]},
                    {"tool_name":"add_note","collection":"TicketNote","fields":[{"name":"ticket","required":true},{"name":"body"}]}
                ]}),
            ),
            (
                Collection::Tools,
                json!({"node_did":owner,"tools_id":tools_id,"datastore":{"datastore_tool_surface_ids":["ticket-notes"]}}),
            ),
            (
                Collection::AgentContext,
                json!({"node_did":owner,"context_id":context_id,"tools_id":tools_id,"system_prompt":"You file ticket notes.\n","skill_ids":["triage"]}),
            ),
            (
                Collection::Task,
                json!({"node_did":owner,"task_id":"note-ticket","agent_did":worker.agent_did,"prompt_template":"File a note for the new ticket."}),
            ),
            (
                Collection::EventSource,
                json!({"node_did":owner,"event_source_id":"ticket-created","source_collection":"Ticket","event_kind":"created"}),
            ),
            (
                Collection::Trigger,
                json!({"node_did":owner,"trigger_id":"on-ticket","task_id":"note-ticket","source":{"kind":"event","event_source_id":"ticket-created"},"concurrency":"serial"}),
            ),
        ],
    )
    .await;

    let out = tempfile::tempdir().unwrap();
    let selection = ExportSelection {
        documents: [(Collection::Trigger, "on-ticket".to_owned())].into(),
        ..ExportSelection::default()
    };
    let report = export_pack(
        &origin_access,
        &owner,
        &out.path().join("ticket_desk"),
        "ticket_desk",
        "fixture",
        &selection,
    )
    .await
    .unwrap();
    assert!(
        report.check.problems.is_empty() && report.check.unverified.is_empty(),
        "{:#?}",
        report.check
    );
    assert_eq!(report.selected, ["Trigger/on-ticket"]);
    assert_eq!(
        report
            .by_reference
            .iter()
            .map(|pulled| pulled.part.as_str())
            .collect::<Vec<_>>(),
        [
            format!("Agent/{}", worker.agent_did).as_str(),
            format!("AgentContext/{context_id}").as_str(),
            "DatastoreToolSurface/ticket-notes",
            "EventSource/ticket-created",
            "Skill/triage",
            "Task/note-ticket",
            format!("Tools/{tools_id}").as_str(),
            "collection/Ticket",
            "collection/TicketNote",
        ]
    );
    let slot = "default-fixture-agent";
    assert_eq!(report.agents[slot], worker.agent_did);
    let config = std::fs::read_to_string(report.dir.join("pack_config.json")).unwrap();
    assert!(
        !config.contains(&owner) && !config.contains(&worker.agent_did),
        "the pack names no DID of the node it came from: {config}"
    );
    assert!(
        config.contains("gents:agent-slot:default-fixture-agent"),
        "{config}"
    );
    assert!(
        config.contains("${GENTS_PACK_AGENT_DID.default-fixture-agent}:context"),
        "{config}"
    );
    for (file, text) in [
        (
            "contexts/default_fixture_agent_context/system_prompt.md",
            "You file ticket notes.\n",
        ),
        (
            "skills/triage/SKILL.md",
            "# Triage\n\nSort by urgency; a rush costs $5.\n",
        ),
        (
            "tasks/note_ticket/prompt_template.md",
            "File a note for the new ticket.",
        ),
        (
            "schemas/ticket.graphql",
            "type Ticket {\n  subject: String @index\n}\n",
        ),
    ] {
        assert_eq!(
            std::fs::read_to_string(report.dir.join(file)).unwrap(),
            text,
            "{file}"
        );
    }
    let inference_slot = "default_fixture_agent".to_owned();
    assert_eq!(
        report.inference_slots,
        [
            json!({"name":inference_slot,"description":"The model default-fixture-agent runs on.","agents":[slot]})
        ]
    );
    for file in &report.files {
        let text = std::fs::read_to_string(report.dir.join(file)).unwrap();
        assert!(
            !text.contains("did:key:"),
            "{file} names a DID of the node it came from: {text}"
        );
    }
    assert!(
        !config.contains("null"),
        "absent fields stay absent: {config}"
    );

    let (bytes, _) = pack_dir(&report.dir).unwrap();
    let archive = PackArchive::from_bytes(&bytes).unwrap();

    // A second node holds none of it: the install creates the whole app.
    let (second, second_user, second_access) =
        crate::test_support::signed_test_node_without_agent("pack-export-second").await;
    let second_owner = second.node_identity.did().to_owned();
    grant_schema(&second, &second_user).await;
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
    let created = install(
        &second,
        &second_user,
        &second_access,
        &archive,
        &BTreeMap::from([(inference_slot.clone(), "profile".to_owned())]),
    )
    .await
    .unwrap();
    assert!(created.adopted.is_empty(), "{created:#?}");
    assert_eq!(created.created.len(), 8, "{created:#?}");
    let installed_agent = created.agents[slot].clone();
    assert_ne!(installed_agent, worker.agent_did);
    let live = rows(
        &second_access,
        "{ Agent { agent_did context_id inference_profile_id display_name } AgentContext { context_id tools_id system_prompt skill_ids } Tools { tools_id datastore } Skill { skill_id instructions } DatastoreToolSurface { surface_id entries } Task { task_id agent_did prompt_template } EventSource { event_source_id source_collection } Trigger { trigger_id task_id source } }",
    )
    .await;
    assert_eq!(live["Agent"][0]["agent_did"], installed_agent);
    assert_eq!(live["Agent"][0]["inference_profile_id"], "profile");
    assert_eq!(
        live["Agent"][0]["context_id"],
        format!("{installed_agent}:context")
    );
    assert_eq!(
        live["AgentContext"][0]["system_prompt"],
        "You file ticket notes.\n"
    );
    assert_eq!(live["AgentContext"][0]["skill_ids"], json!(["triage"]));
    assert_eq!(
        live["AgentContext"][0]["tools_id"],
        format!("{installed_agent}:tools")
    );
    assert_eq!(
        live["Tools"][0]["tools_id"],
        format!("{installed_agent}:tools")
    );
    assert_eq!(
        live["Skill"][0]["instructions"],
        "# Triage\n\nSort by urgency; a rush costs $5.\n"
    );
    assert_eq!(live["Task"][0]["agent_did"], installed_agent);
    assert_eq!(live["EventSource"][0]["source_collection"], "Ticket");
    assert_eq!(live["Trigger"][0]["task_id"], "note-ticket");
    let origin_live = rows(
        &origin_access,
        "{ DatastoreToolSurface { entries } Tools { datastore } }",
    )
    .await;
    assert_eq!(
        live["DatastoreToolSurface"][0]["entries"],
        origin_live["DatastoreToolSurface"][0]["entries"]
    );
    assert_eq!(
        live["Tools"][0]["datastore"],
        origin_live["Tools"][0]["datastore"]
    );
    for collection in ["Ticket", "TicketNote"] {
        assert!(
            second_access
                .collection_version(collection)
                .await
                .unwrap()
                .is_some(),
            "{collection} is published on the second node"
        );
    }

    // Where it came from, the live originals become the pack's documents.
    let origin_binding = BTreeMap::from([(
        inference_slot.clone(),
        format!("{}:inference", worker.agent_did),
    )]);
    let adopted = install(
        &origin,
        &origin_user,
        &origin_access,
        &archive,
        &origin_binding,
    )
    .await
    .unwrap();
    assert!(adopted.created.is_empty(), "{adopted:#?}");
    assert_eq!(adopted.adopted.len(), 8, "{adopted:#?}");
    assert_eq!(adopted.agents[slot], worker.agent_did);
    let agents = rows(&origin_access, "{ Agent { agent_did } }").await;
    assert_eq!(agents["Agent"].as_array().unwrap().len(), 1);

    // A live document that no longer matches the pack is the node's own.
    remove_pack(
        &origin_access,
        &owner,
        "fixture/ticket_desk",
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    apply(
        &origin_access,
        vec![(
            Collection::Task,
            json!({"node_did":owner,"task_id":"note-ticket","agent_did":worker.agent_did,"prompt_template":"Rewritten on the node."}),
        )],
    )
    .await;
    let refused = install(
        &origin,
        &origin_user,
        &origin_access,
        &archive,
        &origin_binding,
    )
    .await
    .unwrap_err();
    let refused = format!("{refused:#}");
    assert!(
        refused.contains("Task/note-ticket") && refused.contains("different content"),
        "{refused}"
    );
    assert!(!refused.contains("Trigger/on-ticket"), "{refused}");
}

#[tokio::test]
async fn a_reference_a_pack_cannot_carry_refuses_the_export_by_name() {
    let (origin, _user, access, worker) =
        crate::test_support::signed_test_node("pack-export-refusal").await;
    let owner = origin.node_identity.did().to_owned();
    let context_id = format!("{}:context", worker.agent_did);
    apply(
        &access,
        vec![
            (
                Collection::Compaction,
                json!({"node_did":owner,"compaction_id":"tight","inference_profile_id":format!("{}:inference", worker.agent_did)}),
            ),
            (
                Collection::AgentContext,
                json!({"node_did":owner,"context_id":context_id,"tools_id":format!("{}:tools", worker.agent_did),"compaction_id":"tight"}),
            ),
            (
                Collection::Tools,
                json!({"node_did":owner,"tools_id":format!("{}:tools", worker.agent_did),"host":{"root":"/srv/desk","files":{"mode":"ReadOnly"}}}),
            ),
        ],
    )
    .await;
    let out = tempfile::tempdir().unwrap();
    let dir = out.path().join("refused");
    let error = export_pack(
        &access,
        &owner,
        &dir,
        "refused",
        "fixture",
        &ExportSelection {
            documents: [
                (Collection::Agent, worker.agent_did.clone()),
                (Collection::Task, "absent".to_owned()),
            ]
            .into(),
            ..ExportSelection::default()
        },
    )
    .await
    .unwrap_err();
    let error = format!("{error:#}");
    assert!(
        error.contains("CompactionConfig/tight inference_profile_id references InferenceProfile/"),
        "{error}"
    );
    assert!(error.contains("Task/absent is not on this node"), "{error}");
    assert!(
        error.contains("host.root is /srv/desk, a path on this node"),
        "{error}"
    );
    assert!(!dir.exists(), "a refused export writes nothing");
}

#[tokio::test]
async fn a_page_and_a_plugin_source_export_with_the_surface_the_page_is_granted() {
    let (origin, user, access, _worker) =
        crate::test_support::signed_test_node("pack-export-live-documents").await;
    let owner = origin.node_identity.did().to_owned();
    grant_schema(&origin, &user).await;
    access.add_collection(SDL).await.unwrap();
    access
        .add_collection("type DeskTicket { subject: String }")
        .await
        .unwrap();
    apply(
        &access,
        vec![
            (
                Collection::DatastoreToolSurface,
                json!({"node_did":owner,"surface_id":"ticket-notes","display_name":"Ticket notes","entries":[
                    {"kind":"query","tool_name":"list_notes","collection":"TicketNote","fields":["ticket","body"]}
                ]}),
            ),
            (
                Collection::Page,
                json!({"node_did":owner,"page_id":"notes","display_name":"Notes","module":"export default { id: 'notes', name: 'Notes', register() {} };\n","data":[{"surface":"ticket-notes","tools":["list_notes"]}]}),
            ),
            (
                Collection::PluginSource,
                json!({"node_did":owner,"plugin_id":"double","language":"js","source":"module.exports = (input) => ({ n: input.n * 2 });\n","data":{"collections":["DeskTicket"],"ops":["query"]}}),
            ),
        ],
    )
    .await;
    let out = tempfile::tempdir().unwrap();
    let dir = out.path().join("notes_app");
    let report = export_pack(
        &access,
        &owner,
        &dir,
        "notes_app",
        "fixture",
        &ExportSelection {
            documents: [
                (Collection::Page, "notes".to_owned()),
                (Collection::PluginSource, "double".to_owned()),
            ]
            .into(),
            ..ExportSelection::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        report
            .by_reference
            .iter()
            .map(|pulled| (pulled.part.as_str(), pulled.via.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("DatastoreToolSurface/ticket-notes", "Page/notes data"),
            (
                "collection/DeskTicket",
                "PluginSource/double data.collections"
            ),
            (
                "collection/TicketNote",
                "DatastoreToolSurface/ticket-notes tool list_notes"
            ),
        ],
    );
    let config: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("pack_config.json")).unwrap())
            .unwrap();
    assert_eq!(config["pages"][0]["page_id"], "notes");
    assert_eq!(config["plugin_sources"][0]["plugin_id"], "double");
    assert_eq!(
        config["datastore_tool_surfaces"][0]["surface_id"],
        "ticket-notes"
    );
    assert!(report.check.problems.is_empty(), "{:?}", report.check);
    assert!(
        std::fs::read_dir(dir.join("schemas"))
            .unwrap()
            .any(|entry| {
                std::fs::read_to_string(entry.unwrap().path())
                    .unwrap()
                    .contains("type DeskTicket")
            }),
        "the plugin source's data collection ships its SDL: {:?}",
        report.schemas
    );
}

#[tokio::test]
async fn a_collection_with_defaults_and_a_full_text_index_exports_and_searches_where_it_installs() {
    let (origin, user, access, _worker) =
        crate::test_support::signed_test_node("pack-export-fulltext").await;
    let owner = origin.node_identity.did().to_owned();
    grant_schema(&origin, &user).await;
    access
        .add_collection(
            "type HelpArticle { title: String @index body: String @fulltext(language: \"english\") status: String @default(value: \"\") views: Int @default(value: 20) }",
        )
        .await
        .unwrap();
    let out = tempfile::tempdir().unwrap();
    let report = export_pack(
        &access,
        &owner,
        &out.path().join("help_desk"),
        "help_desk",
        "fixture",
        &ExportSelection {
            collections: ["HelpArticle".to_owned()].into(),
            ..ExportSelection::default()
        },
    )
    .await
    .unwrap();
    assert!(report.check.problems.is_empty(), "{:#?}", report.check);
    let sdl = std::fs::read_to_string(report.dir.join(&report.schemas[0])).unwrap();
    for directive in [
        "body: String @fulltext(language: \"english\"",
        "status: String @default(value: \"\")",
        "views: Int @default(value: 20)",
    ] {
        assert!(sdl.contains(directive), "{directive}: {sdl}");
    }

    let (bytes, _) = pack_dir(&report.dir).unwrap();
    let archive = PackArchive::from_bytes(&bytes).unwrap();
    let (second, second_user, second_access) =
        crate::test_support::signed_test_node_without_agent("pack-export-fulltext-second").await;
    grant_schema(&second, &second_user).await;
    install(
        &second,
        &second_user,
        &second_access,
        &archive,
        &BTreeMap::new(),
    )
    .await
    .unwrap();
    for (title, body) in [
        ("Printer", "The printer jams when the paper tray is full"),
        ("Badge", "Request a new door badge from the front desk"),
    ] {
        ConfigAccess::write_local(
            &second.node,
            "test.pack_export.fulltext",
            &format!(
                "mutation {{ add_HelpArticle(input: {{title: \"{title}\", body: \"{body}\"}}) {{ _docID }} }}"
            ),
        )
        .await
        .unwrap();
    }
    let found = rows(
        &second_access,
        "{ HelpArticle(order: [{_alias: {_score: DESC}}]) { title status views _score: BM25(query: \"printer paper\", fields: [\"body\"]) } }",
    )
    .await;
    assert_eq!(found["HelpArticle"][0]["title"], "Printer", "{found}");
    assert!(
        found["HelpArticle"][0]["_score"].as_f64().unwrap() > 0.0,
        "{found}"
    );
    assert_eq!(found["HelpArticle"][1]["_score"], 0.0, "{found}");
    assert_eq!(found["HelpArticle"][0]["status"], "", "{found}");
    assert_eq!(found["HelpArticle"][0]["views"], 20, "{found}");
}
