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
    let modeled = &gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration model")
        .dangling_selection_repair_cases;
    assert_eq!(modeled.len(), 5);
    assert!(
        modeled.iter().all(|case| case["applied"] == false),
        "every available read is ACP-filtered, so no modeled repair applies"
    );
    let agent_access = fixture.agent_access();
    // NAC-enabled cases run first; disabling NAC is the last observation.
    let mut ordered = modeled.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|case| !case["nac_enabled"].as_bool().unwrap());
    let mut nac_disabled = false;
    for case in ordered {
        assert_eq!(case["complete_inventory"], false, "{case}");
        if !case["nac_enabled"].as_bool().unwrap() && !nac_disabled {
            assert_eq!(
                ok(&tools, json!({"argv":["validate"]})).await["valid"],
                false
            );
            fixture.disable_native_nac().await;
            nac_disabled = true;
        }
        let (actor_access, actor) = match case["actor"].as_str().unwrap() {
            "user" => (access, fixture.user_access().actor_did().unwrap()),
            "agent" => (&agent_access, fixture.agent_did().to_owned()),
            other => panic!("unmodeled repair actor {other}"),
        };
        let repair = case["repair"].as_bool().unwrap();
        let applied = actor_access
            .transact_write("test.saved_audit.repair", |txn| {
                let context_id = &context_id;
                let tools_id = &tools_id;
                let actor = &actor;
                let node_did = fixture.node_did();
                Box::pin(async move {
                    let authority = txn.destructive_authority().await?;
                    assert_eq!(
                        (authority.nac_enabled, authority.admin),
                        (
                            case["nac_enabled"].as_bool().unwrap(),
                            case["admin"].as_bool().unwrap()
                        ),
                        "{actor}"
                    );
                    assert_eq!(
                        authority.native_admin(),
                        case["nac_enabled"].as_bool().unwrap() && case["admin"].as_bool().unwrap(),
                        "with NAC disabled no actor is the native admin: {actor}"
                    );
                    if repair {
                        assert_eq!(
                            authority.decide().err().map(|refusal| refusal.name()),
                            case["refusal"].as_str(),
                            "{actor}"
                        );
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
        assert_eq!(
            applied.is_ok(),
            case["applied"].as_bool().unwrap(),
            "{case}: {applied:?}"
        );
        let error = applied.expect_err("modeled repairs are refused");
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
        if case["nac_enabled"].as_bool().unwrap() {
            assert_eq!(refusal, case["refusal"].as_str(), "{case}: {error:#}");
            assert!(format!("{error:#}").contains("is unavailable"), "{error:#}");
        } else {
            // While NAC is disabled native refuses the document's source
            // projection before publication reaches the selection repair.
            assert!(
                format!("{error:#}").contains("source projection requires an authorized actor"),
                "{case}: {error:#}"
            );
        }
        assert_eq!(before, access.execute(&query).await.unwrap(), "{case}");
    }
    assert!(nac_disabled, "the NAC-disabled case ran");
}

use gents_automation::test_support::self_config_text::*;
