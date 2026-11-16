use gents_store::collection::Collection;
use serde_json::json;
#[tokio::test]
async fn event_args_rejection_rolls_back_the_whole_configuration_plan() {
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{read_desired_state_record_in_txn, DesiredStateApplyPlan};
    use gents_store::identity::NodeIdentity;
    let (db, user, access, default) = gents_inference::test_support::signed_test_node(
        "event-args-rollback",
        gents_store::node_extension::Extensions::default(),
    )
    .await;
    let owner = db.node_identity.did().to_owned();
    let binding = gents_store::identity::provision_agent_identity_authorized(
        &db.node,
        db.node_identity.as_ref(),
        &identity::Did::new(user.did()).unwrap(),
    )
    .await
    .unwrap();
    let mut config = json!({
        "node":{"node_did":owner},
        "default_agent_selection":{"node_did":owner,"default_agent_did":default.agent_did},
        "agents":[{"node_did":owner,"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,"inference_profile_id":"profile"}],
        "inference_profiles":[{"node_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}],
        "inference_backends":[{"node_did":owner,"backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
        "tasks":[{"node_did":owner,"task_id":"task","agent_did":binding.agent_did,"prompt_template":"{{ args.name }}"}],
        "event_sources":[{"node_did":owner,"event_source_id":"event","source_collection":"Records"}]
    });
    let plan = DesiredStateApplyPlan::from_pack_config(
        &serde_json::from_value::<gents_documents::PackConfig>(config.clone()).unwrap(),
    )
    .unwrap();
    access
        .transact_write("test.manual_task", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_documents::test_support::AdmitAllProfiles,
                )
                .await
            })
        })
        .await
        .unwrap();
    config["tasks"][0]["prompt_template"] = json!("{{ args.other }}");
    config["triggers"] = json!([{"node_did":owner,"trigger_id":"trigger","task_id":"task","source":{"kind":"event","event_source_id":"event"}}]);
    let plan = DesiredStateApplyPlan::from_pack_config(
        &serde_json::from_value::<gents_documents::PackConfig>(config.clone()).unwrap(),
    )
    .unwrap();
    assert!(access
        .transact_write("test.invalid_event_task", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_documents::test_support::AdmitAllProfiles,
                )
                .await
            })
        })
        .await
        .is_err());
    access
        .transact_write("test.rollback", |txn| {
            let owner = &owner;
            Box::pin(async move {
                let (_, task) =
                    read_desired_state_record_in_txn(txn, Collection::Task, owner, "task")
                        .await?
                        .unwrap();
                assert_eq!(task["prompt_template"], json!("{{ args.name }}"));
                assert!(read_desired_state_record_in_txn(
                    txn,
                    Collection::Trigger,
                    owner,
                    "trigger"
                )
                .await?
                .is_none());
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    config["tasks"][0]["prompt_template"] = json!("{{ doc.name }}");
    let plan = DesiredStateApplyPlan::from_pack_config(
        &serde_json::from_value::<gents_documents::PackConfig>(config).unwrap(),
    )
    .unwrap();
    access
        .transact_write("test.valid_event_task", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_documents::test_support::AdmitAllProfiles,
                )
                .await
            })
        })
        .await
        .unwrap();
}
