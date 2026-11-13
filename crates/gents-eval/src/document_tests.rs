use crate::test_support::{completion, draft, identity, origin};
use anyhow::Context;
use gents::defra_node::Node as EmbeddedNode;
use gents::eval::documents::*;
use gents_documents::EvalSplit;
use gents_store::config_client::ConfigAccess;
use serde_json::json;
use std::sync::Arc;
#[tokio::test]
async fn model_driven_held_client_eval_and_optimization_records_are_private() -> anyhow::Result<()>
{
    use crate::optimization::job::{self, Budgets, JobOrigin, JournalEntry};
    use crate::optimization::policy::PolicyV2;
    use crate::optimization::target::{Target, TargetField};
    use acp::nac::NodePermission;
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    use gents_lean_vocab::families::eval_records_published::PublishOutput;
    use gents_store::config_client::GraphqlEndpoint;
    use gents_store::identity::NodeIdentity;
    let directory = tempfile::tempdir()?;
    let user =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("user"), None)?;
    let runtime =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("runtime"), None)?;
    let outsider = gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("outsider"),
        None,
    )?;
    let binding = gents_store::identity::provision_agent_identity(&runtime)?;
    let managed = gents_store::identity::resolve_agent_identity(&runtime, &binding)?;
    let writer =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("writer"), None)?;
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
    let mut required: Vec<_> = gents_store::collection::Collection::ALL
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
    let endpoint = |actor: &gents_store::identity::KeyIdentity| -> anyhow::Result<ConfigAccess> {
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
        collection: gents_store::collection::Collection::EvalDefinition,
        add: definition.clone(),
        update: definition,
    }])?;
    access
        .transact_write("test.eval.definition", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
                .map(|_| ())
            })
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
    let mut agent_user_read = None;
    for case in gents_lean_vocab::families::eval_records_published::cases() {
        let name = case.input.name.as_str();
        let output = match name {
            "agent-creator" | "runtime-creator" => {
                let (actor, creator) = if name == "agent-creator" {
                    (&managed_access, managed.did())
                } else {
                    (&runtime_access, runtime.did())
                };
                create_run(
                    actor,
                    name,
                    runtime.did(),
                    creator,
                    &origin(EvalSplit::Train),
                )
                .await
                .with_context(|| format!("creating {name} run"))?;
                let output = PublishOutput {
                    created: load_run(actor, runtime.did(), name).await?.is_some(),
                    user_read: load_run(&access, runtime.did(), name).await?.is_some(),
                    stranger_read: load_run(&outsider_access, runtime.did(), name)
                        .await?
                        .is_some(),
                };
                assert!(
                    invalidate_run(&access, runtime.did(), name, user.did(), "not creator")
                        .await
                        .is_err()
                );
                invalidate_run(actor, runtime.did(), name, creator, "creator").await?;
                if name == "agent-creator" {
                    agent_user_read = Some(output.user_read);
                }
                output
            }
            "foreign-node" => {
                assert!(create_run(
                    &outsider_access,
                    name,
                    runtime.did(),
                    outsider.did(),
                    &origin(EvalSplit::Train)
                )
                .await
                .is_err());
                let created = load_run(&outsider_access, runtime.did(), name)
                    .await?
                    .is_some();
                PublishOutput {
                    created,
                    user_read: load_run(&access, runtime.did(), name).await?.is_some(),
                    stranger_read: created,
                }
            }
            _ => {
                let (collection, id_field) = match name {
                    "definition" => ("EvalDefinition", "definition_id"),
                    "run" => ("EvalRun", "run_id"),
                    "trial" => ("EvalTrial", "trial_id"),
                    "verdict" => ("EvalVerdict", "verdict_id"),
                    "optimization" => ("OptimizationJob", "job_id"),
                    other => panic!("no held fixture creates {other}"),
                };
                let query = format!(
                    "{{ {collection}(filter: {{ {id_field}: {{ _eq: \"{name}\" }} }}) {{ _docID }} }}"
                );
                let own = access.execute(&query).await?;
                let visible = own["data"][collection]
                    .as_array()
                    .is_some_and(|rows| !rows.is_empty());
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
                PublishOutput {
                    created: visible,
                    user_read: visible,
                    stranger_read: foreign["data"][collection]
                        .as_array()
                        .is_some_and(|rows| !rows.is_empty()),
                }
            }
        };
        case.check(output);
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
    assert_eq!(
        load_run(&access, runtime.did(), "agent-run")
            .await?
            .is_some(),
        agent_user_read.expect("the agent-creator case ran")
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
    node.shutdown().await?;
    Ok(())
}
