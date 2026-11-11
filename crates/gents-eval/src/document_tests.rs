use crate::test_support::{completion, draft, identity, origin};
use anyhow::Context;
use gents::defra_node::Node as EmbeddedNode;
use gents::documents::EvalSplit;
use gents::eval::documents::*;
use gents::ConfigAccess;
use serde_json::json;
use std::sync::Arc;
#[tokio::test]
async fn model_driven_held_client_eval_and_optimization_records_are_private() -> anyhow::Result<()>
{
    use crate::optimization::job::{self, Budgets, JobOrigin, JournalEntry};
    use crate::optimization::policy::PolicyV2;
    use crate::optimization::target::{Target, TargetField};
    use acp::nac::NodePermission;
    use gents::documents::apply_desired_state_plan;
    use gents::documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    use gents::store::config_client::GraphqlEndpoint;
    use gents::store::identity::NodeIdentity;
    let modeled = &gents_lean_vocab::lean_contract_snapshot().identity_eval_record_cases;
    let directory = tempfile::tempdir()?;
    let user = gents::KeyIdentity::load_or_create(directory.path().join("user"), None)?;
    let runtime = gents::KeyIdentity::load_or_create(directory.path().join("runtime"), None)?;
    let outsider = gents::KeyIdentity::load_or_create(directory.path().join("outsider"), None)?;
    let binding = gents::store::identity::provision_agent_identity(&runtime)?;
    let managed = gents::store::identity::resolve_agent_identity(&runtime, &binding)?;
    let writer = gents::KeyIdentity::load_or_create(directory.path().join("writer"), None)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        EmbeddedNode::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(user.did().to_owned())
            .with_http(gents::defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let context = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    node.add_dac_policy_with_counter_as(
        &context,
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    for permission in [NodePermission::DocumentRead, NodePermission::DocumentUpdate] {
        node.add_nac_actor_relationship_as(&context, permission.as_str(), outsider.did())
            .await?;
    }
    for permission in [
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DacRelationAdd,
    ] {
        node.add_nac_actor_relationship_as(&context, permission.as_str(), writer.did())
            .await?;
    }
    for actor in [managed.did(), runtime.did()] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
        ] {
            node.add_nac_actor_relationship_as(&context, permission.as_str(), actor)
                .await?;
        }
    }
    let mut required: Vec<_> = gents::Collection::ALL
        .iter()
        .map(|collection| collection.graphql_type())
        .collect();
    required.extend(["EvalRun", "EvalTrial", "EvalVerdict", "OptimizationJob"]);
    let schemas = required
        .iter()
        .map(|name| {
            let declaration = format!("type {name} ");
            gents_protocol::schemas::ALL
                .iter()
                .chain(gents_protocol::schemas::RUNTIME_ALL.iter())
                .find(|schema| schema.lines().any(|line| line.starts_with(&declaration)))
                .copied()
                .with_context(|| format!("missing fixture schema {name}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ConfigAccess::local_as(node.clone(), identity::Did::new(user.did())?)
        .add_collection(&schemas.join("\n"))
        .await?;
    let endpoint = |actor: &gents::KeyIdentity| -> anyhow::Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), actor.did())
                .with_client_signing_capability(actor.client_signing_capability()?)?
                .with_client_signed_agent_creates(),
        ))
    };
    let access = endpoint(&user)?;
    let outsider_access = endpoint(&outsider)?;
    let node_input = json!({"input":{"node_did":runtime.did()}});
    let parent = access.transact_write("test.eval.node", |txn| {
            let node_input = &node_input;
            Box::pin(async move {
                let response = txn.execute_client_signed_create("mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }", node_input, gents_protocol::schemas::NODE).await?;
                Ok::<_, anyhow::Error>(gents_protocol::graphql::extract_mutation_doc_id(&response, "Node")?)
            })
        }).await?;
    access
        .add_actor_relationship(
            "Node",
            &parent,
            "runtime_reader",
            &identity::Did::new(writer.did())?,
        )
        .await?;
    for actor in [managed.did(), runtime.did()] {
        access
            .add_actor_relationship(
                "Node",
                &parent,
                "runtime_reader",
                &identity::Did::new(actor)?,
            )
            .await?;
    }
    let managed_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(managed.did())?,
        managed.as_ref(),
    )?;
    let runtime_access = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    for (name, actor, creator) in [
        ("agent-creator", &managed_access, managed.did()),
        ("runtime-creator", &runtime_access, runtime.did()),
    ] {
        let case = modeled
            .iter()
            .find(|case| case["name"] == name)
            .expect("modeled creator");
        create_run(
            actor,
            name,
            runtime.did(),
            creator,
            &origin(EvalSplit::Train),
        )
        .await
        .with_context(|| format!("creating {name} run"))?;
        assert_eq!(
            load_run(&access, runtime.did(), name).await?.is_some(),
            case["expected_user_read"].as_bool().unwrap()
        );
        assert!(
            invalidate_run(&access, runtime.did(), name, user.did(), "not creator")
                .await
                .is_err()
        );
        invalidate_run(actor, runtime.did(), name, creator, "creator").await?;
    }
    let writer_access = endpoint(&writer)?;
    create_run(
        &writer_access,
        "agent-run",
        runtime.did(),
        writer.did(),
        &origin(EvalSplit::Train),
    )
    .await
    .context("creating held non-owner writer run")?;
    let agent_model = modeled
        .iter()
        .find(|case| case["name"] == "agent-creator")
        .expect("modeled creator");
    assert_eq!(
        load_run(&access, runtime.did(), "agent-run")
            .await?
            .is_some(),
        agent_model["expected_user_read"].as_bool().unwrap()
    );
    assert!(invalidate_run(
        &access,
        runtime.did(),
        "agent-run",
        user.did(),
        "not creator"
    )
    .await
    .is_err());
    invalidate_run(
        &writer_access,
        runtime.did(),
        "agent-run",
        writer.did(),
        "creator",
    )
    .await?;
    let foreign_model = modeled
        .iter()
        .find(|case| case["name"] == "foreign-node")
        .expect("modeled unavailable Node");
    assert!(create_run(
        &outsider_access,
        "orphan",
        runtime.did(),
        outsider.did(),
        &origin(EvalSplit::Train)
    )
    .await
    .is_err());
    assert_eq!(
        load_run(&outsider_access, runtime.did(), "orphan")
            .await?
            .is_some(),
        foreign_model["expected_created"].as_bool().unwrap()
    );
    let definition = json!({
        "node_did":runtime.did(), "definition_id":"definition",
        "comparability_version":1, "title":"private",
        "subject":{"kind":"agent"},
        "cases":[{"case_id":"case", "split":"train", "stages":[{
            "stage_id":"stage", "prompt":"Respond with ok.",
            "checks":[{"check":"final_message_matches", "params":{"all":["ok"]}, "tier":"acceptance"}]
        }]}],
    });
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: gents::Collection::EvalDefinition,
        add: definition.clone(),
        update: definition,
    }])?;
    access
        .transact_write("test.eval.definition", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await?;
    create_run(
        &access,
        "run",
        runtime.did(),
        user.did(),
        &origin(EvalSplit::Train),
    )
    .await
    .context("creating held Node owner run")?;
    create_trial(&access, runtime.did(), &identity("run", "trial")).await?;
    complete_trial(&access, runtime.did(), "trial", &completion()).await?;
    assert!(
        complete_trial(&access, runtime.did(), "trial", &completion())
            .await
            .is_err()
    );
    assert!(load_trials(&access, runtime.did(), "run").await?[0]
        .completion
        .is_some());
    append_verdict(
        &access,
        runtime.did(),
        EvalSplit::Train,
        &draft("run", "trial", "verdict", Some("private")),
    )
    .await?;
    let job_origin = JobOrigin {
        target: Target {
            field: TargetField::AgentContextSystemPrompt,
            owner: runtime.did().into(),
            id: "context".into(),
        },
        closure: vec![],
        subject: SubjectRef {
            pack_digest: "sha256:baseline".into(),
            agent_slot: "monitor".into(),
        },
        definition: DefinitionRef {
            definition_id: "definition".into(),
            comparability_version: 1,
            digest: "sha256:definition".into(),
        },
        policy: PolicyV2::uncalibrated(),
        trials_per_case: 1,
        budgets: Budgets {
            max_rounds: 1,
            max_case_trials: 1,
            max_tokens: 100,
            deadline_unix_secs: None,
        },
        baseline_text: "private".into(),
        owner: runtime.did().into(),
        seed_base: 1,
        inference_profile_id: "profile".into(),
        max_text_bytes: 1024,
        jobs_dir: directory.path().to_owned(),
        captures: vec![],
    };
    let mut job = job::create_job(&access, "optimization", runtime.did(), &job_origin).await?;
    job::append(&access, &mut job, JournalEntry::Frozen).await?;
    assert_eq!(
        job::load_job(&access, runtime.did(), "optimization")
            .await?
            .unwrap()
            .journal
            .len(),
        1
    );
    for (name, collection, id_field) in [
        ("definition", "EvalDefinition", "definition_id"),
        ("run", "EvalRun", "run_id"),
        ("trial", "EvalTrial", "trial_id"),
        ("verdict", "EvalVerdict", "verdict_id"),
        ("optimization", "OptimizationJob", "job_id"),
    ] {
        let case = modeled
            .iter()
            .find(|case| case["name"] == name)
            .expect("modeled case");
        let query = format!(
            "{{ {collection}(filter: {{ {id_field}: {{ _eq: \"{name}\" }} }}) {{ _docID }} }}"
        );
        let own = access.execute(&query).await?;
        let visible = own["data"][collection]
            .as_array()
            .is_some_and(|rows| !rows.is_empty());
        assert_eq!(
            visible,
            case["expected_user_read"].as_bool().unwrap(),
            "{name} user visibility"
        );
        let doc_id = own["data"][collection][0]["_docID"]
            .as_str()
            .context("created record ID")?
            .to_owned();
        let owners = access
            .transact_readonly("test.eval.owners", |txn| {
                let doc_id = &doc_id;
                Box::pin(async move {
                    txn.document_owners(collection, std::slice::from_ref(doc_id))
                        .await
                })
            })
            .await?;
        assert_eq!(owners.len(), 1);
        assert_eq!(
            owners[0].owner.as_ref().map(|did| did.as_str()),
            Some(user.did()),
            "{name} native owner"
        );
        let foreign = outsider_access.execute(&query).await?;
        let foreign_visible = foreign["data"][collection]
            .as_array()
            .is_some_and(|rows| !rows.is_empty());
        assert_eq!(
            foreign_visible,
            case["expected_stranger_read"].as_bool().unwrap(),
            "{name} outsider visibility"
        );
    }
    node.shutdown().await?;
    Ok(())
}
