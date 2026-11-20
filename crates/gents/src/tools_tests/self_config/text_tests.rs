use super::*;

#[tokio::test]
async fn saved_config_audit_reports_broken_references_without_mutation_and_refuses_repair() {
    let (fixture, tools) = authenticated_setup("saved-audit", &["node", "tools", "agent"]).await;
    let context_id = format!("{}:context", fixture.agent_did());
    let tools_id = format!("{}:tools", fixture.agent_did());
    let failure = structured_failure(&tools, json!({"argv":["datastore","validate"]})).await;
    assert_eq!(failure["config_execution"]["mutation_entered"], false);
    assert!(!failure["error"].as_str().unwrap().contains("sessions"));
    let recovery = failure["recovery"]["next_call"].clone();
    assert_eq!(recovery, json!({"argv":["validate"]}));
    let valid = ok(&tools, recovery).await;
    assert_eq!(valid["valid"], true);
    assert!(valid["checked_documents"].as_u64().unwrap() > 0);
    assert_eq!(valid["collections"]["Agent"], 1);
    fixture
        .update_signed_configuration(
            gents_store::collection::Collection::AgentContext,
            &context_id,
            json!({"tools_id": "absent-tools"}),
        )
        .await;
    let owner = gents_store::graphql::escape_graphql_string(fixture.node_did());
    let access = fixture.user_access();
    let query = format!(
        r#"{{ AgentContext(filter: {{node_did: {{_eq: "{owner}"}}}}) {{context_id tools_id}} }}"#
    );
    let before = access.execute(&query).await.unwrap();
    let invalid = ok(&tools, json!({"argv":["validate"]})).await;
    assert_eq!(invalid["valid"], false, "{invalid}");
    assert_eq!(invalid["committed"], false);
    assert_eq!(invalid["config_execution"]["mutation_entered"], false);
    assert_eq!(invalid["errors"][0]["collection"], "AgentContext");
    assert_eq!(invalid["errors"][0]["field"], "tools_id");
    assert_eq!(invalid["errors"][0]["missing"]["id"], "absent-tools");
    assert_eq!(before, access.execute(&query).await.unwrap());
    ok(&tools, invalid["errors"][0]["inspect_with"].clone()).await;
    // An ordinary edit away from the dangling source fails closed.
    let refused_edit = refused(
        &tools,
        json!({"argv":["context","update"],"target_id":context_id,"set":{"tools_id":tools_id}}),
    )
    .await;
    assert!(refused_edit.contains("is unavailable"), "{refused_edit}");
    assert_eq!(before, access.execute(&query).await.unwrap());
    fixture.grant_user_native_admin().await;
    let agent_access = fixture.agent_access();
    let mut nac_disabled = false;
    for case in gents_lean_vocab::families::household_dangling_selection_repair::cases() {
        let modeled = &case.input;
        assert!(!modeled.complete_inventory, "{modeled:?}");
        assert!(
            !(modeled.nac_enabled && nac_disabled),
            "disabling NAC is the fixture's last change: {modeled:?}"
        );
        if !modeled.nac_enabled && !nac_disabled {
            assert_eq!(
                ok(&tools, json!({"argv":["validate"]})).await["valid"],
                false
            );
            fixture.disable_native_nac().await;
            nac_disabled = true;
        }
        let (actor_access, actor) = match modeled.actor.as_str() {
            "user" => (access, fixture.user_access().actor_did().unwrap()),
            "agent" => (&agent_access, fixture.agent_did().to_owned()),
            other => panic!("unmodeled repair actor {other}"),
        };
        let repair = modeled.repair;
        let observed_refusal = std::sync::Mutex::new(None);
        let applied = actor_access
            .transact_write("test.saved_audit.repair", |txn| {
                let context_id = &context_id;
                let tools_id = &tools_id;
                let actor = &actor;
                let node_did = fixture.node_did();
                let observed_refusal = &observed_refusal;
                Box::pin(async move {
                    let authority = txn.destructive_authority().await?;
                    assert_eq!(
                        (authority.nac_enabled, authority.admin),
                        (modeled.nac_enabled, modeled.admin),
                        "{actor}"
                    );
                    assert_eq!(
                        authority.native_admin(),
                        modeled.nac_enabled && modeled.admin,
                        "with NAC disabled no actor is the native admin: {actor}"
                    );
                    if repair {
                        *observed_refusal.lock().unwrap() = authority
                            .decide()
                            .err()
                            .map(|refusal| refusal.name().to_owned());
                    }
                    let (_, mut context) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::AgentContext,
                        node_did,
                        context_id,
                    )
                    .await?
                    .expect("saved context exists");
                    context
                        .as_object_mut()
                        .unwrap()
                        .retain(|key, _| !key.starts_with('_'));
                    context["tools_id"] = json!(tools_id);
                    let plan = gents_documents::DesiredStateApplyPlan::new(vec![
                        gents_documents::DesiredStateApplyDocument {
                            collection: gents_store::collection::Collection::AgentContext,
                            add: context.clone(),
                            update: context,
                        },
                    ])?;
                    let plan = if repair {
                        plan.with_dangling_selection_repair()
                    } else {
                        plan
                    };
                    gents_documents::apply_desired_state_plan(
                        txn,
                        &plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await;
        let refusal_observed = observed_refusal.into_inner().unwrap();
        let outcome = gents_lean_vocab::families::household_dangling_selection_repair::DanglingRepairOutcome {
            refusal: refusal_observed.clone(),
            applied: applied.is_ok(),
        };
        let Err(error) = applied else {
            case.check(outcome);
            continue;
        };
        let refusal = error.chain().find_map(|cause| {
            match cause.downcast_ref::<gents_store::config_client::ConfigClientError>() {
                Some(
                    gents_store::config_client::ConfigClientError::DanglingSelectionRepairRefused {
                        refusal,
                        ..
                    },
                ) => Some(refusal.name()),
                _ => None,
            }
        });
        if modeled.nac_enabled {
            assert_eq!(
                refusal.map(str::to_owned),
                refusal_observed,
                "{modeled:?}: {error:#}"
            );
            assert!(format!("{error:#}").contains("is unavailable"), "{error:#}");
        } else {
            // While NAC is disabled native refuses the document's source
            // projection before publication reaches the selection repair.
            assert!(
                format!("{error:#}").contains("source projection requires an authorized actor"),
                "{modeled:?}: {error:#}"
            );
        }
        assert_eq!(before, access.execute(&query).await.unwrap(), "{modeled:?}");
        case.check(outcome);
    }
    assert!(nac_disabled, "the NAC-disabled case ran");
}

use gents_automation::test_support::self_config_text::*;
