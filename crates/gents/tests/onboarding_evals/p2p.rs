use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Result};
use gents::config_client::{apply_desired_state_plan, ConfigAccess, DesiredStateApplyPlan};
use gents::document_config::{DatastoreTools, InferenceSampling};
use gents::eval::runner::embedded::EmbeddedHome;
use gents::graphql::escape_graphql_string;
use gents::llm::tool::Tool;
use gents::p2p_tool::P2pTool;
use gents::pack::{load_pack_config, PackInstallOptions, PackManifest};
use gents::tool_surface::ToolRuntimeContext;
use gents::tool_surface::{BashMode, EndpointScope, FileToolMode, ToolPolicySurface};
use gents::{DocumentRuntimeOptions, ToolCeiling};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::support::enrollment::{authorize_enrollment_peer, wait_for_peer_identity};
use crate::support::interrupt::create_runtime_request;
use crate::support::live_inference::{
    bind_target, boot_live_agent_with_options, terminal_assistant_answer,
    wait_for_request_terminal, InferenceTarget,
};
use crate::support::{test_db_from_home, test_p2p_config, TestDb, TestP2pAdmission};

#[derive(Deserialize)]
struct Case {
    grading_version: u32,
    case_id: String,
    split: String,
    prompt: String,
    expectation: String,
}

fn cases() -> Vec<Case> {
    serde_json::from_str(include_str!(
        "../fixtures/configurator_evals/p2p/cases.json"
    ))
    .unwrap()
}

const NATIVE_GRADING_VERSION: u32 = 3;

fn native_outcome(report: &Value, trace: &[Value]) -> bool {
    if report["terminal"] != "completed" || report["enrollment_preserved"] != true {
        return false;
    }
    let peer = &report["peer_id"];
    let did = &report["peer_did"];
    let owned: Vec<_> = report["desired_after"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["peer_id"] == *peer && row["source"] == "engineer")
        .collect();
    let replies: Vec<Value> = trace
        .iter()
        .filter(|entry| entry["tool_call"]["lifecycle_state"] == "completed")
        .filter_map(|entry| serde_json::from_str(entry["result"].as_str()?).ok())
        .collect();
    let identity_observed = replies.iter().any(|reply| {
        [
            (&reply["outcome"]["peers"], "agent_did"),
            (&reply["outcome"]["registered_peers"], "agent_did"),
            (&reply["observations"]["enrolled_peers"], "peer_did"),
        ]
        .into_iter()
        .any(|(rows, field)| {
            rows.as_array().is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["peer_id"] == *peer && row[field] == *did)
            })
        }) || (reply["collection"] == "PeerRegistry"
            && reply["results"].as_array().is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["peer_id"] == *peer && row["agent_did"] == *did)
            }))
    });
    let connection_observed = replies.iter().any(|reply| {
        reply["outcome"]["connected"].is_boolean()
            || reply["outcome"]["connected_peers"]["items"].is_array()
            || reply["observations"]["connected_peers"]["items"].is_array()
    });
    match report["expectation"].as_str() {
        Some("peer_observation") => identity_observed && connection_observed,
        Some("document_arrival") => {
            owned.len() == 1
                && report["local_documents"]
                    .as_array()
                    .is_some_and(|rows| rows.iter().any(|document| {
                        document["_docID"] == report["document_id"] && document["text"] == "Keep the previous rollout image pinned until the owner approves replacement." &&
                        replies.iter().any(|reply| reply["collection"] == "DeploymentNote" && reply["results"].as_array().is_some_and(|results| results.iter().any(|row| row["text"] == document["text"])))
                    }))
        }
        Some("idempotent_pairing") => {
            owned.len() == 1
                && report["desired_before"] == report["desired_after"]
                && report["applied"].as_array().is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row["peer_id"] == *peer
                            && row["collections"].as_array().is_some_and(|names| {
                                names.iter().any(|name| name == "DeploymentNote")
                            })
                    })
                })
        }
        Some("identity_refusal") => {
            owned.is_empty()
                && identity_observed
                && report["desired_before"] == report["desired_after"]
        }
        Some("collection_refusal") => {
            owned.is_empty() && report["desired_before"] == report["desired_after"]
        }
        Some("offline_diagnosis") => {
            owned.len() == 1
                && report["local_documents"].as_array().is_some_and(|rows| {
                    !rows
                        .iter()
                        .any(|row| row["_docID"] == report["document_id"])
                })
                && replies.iter().any(|reply| {
                    reply["outcome"]["connected"] == false
                        || reply["outcome"]["connected_peers"]["observed_count"] == 0
                        || reply["observations"]["connected_peers"]["observed_count"] == 0
                })
        }
        Some("overlay_revoked") => {
            owned.is_empty()
                && report["applied"].as_array().is_some_and(|rows| {
                    rows.iter()
                        .filter(|row| row["peer_id"] == *peer)
                        .all(|row| {
                            !row["collections"].as_array().is_some_and(|names| {
                                names.iter().any(|name| name == "DeploymentNote")
                            })
                        })
                })
        }
        Some("identifier_recovery") => {
            owned.is_empty()
                && trace.iter().any(|entry| {
                    entry["tool_call"]["lifecycle_state"] == "failed"
                        && entry["result"]
                            .as_str()
                            .is_some_and(|result| result.contains("invalid transport peer ID"))
                })
        }
        _ => false,
    }
}

#[test]
fn native_grading_preserves_refusals_and_rejects_identity_substitution() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../fixtures/configurator_evals/p2p/grading_regressions.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        assert_eq!(
            native_outcome(&case["report"], case["trace"].as_array().unwrap()),
            case["expected_native_pass"].as_bool().unwrap(),
            "trial {}",
            case["trial"]
        );
        if case["trial"] == 0 || case["trial"] == 14 {
            assert_eq!(case["original_v2_passed"], false);
            assert!(case["report"]["answer"]
                .as_str()
                .is_some_and(|answer| !answer.is_empty()));
            assert_eq!(case["expected_native_pass"], true);
        }
    }
}

#[test]
#[ignore = "regrades retained JSON evidence without opening or modifying native homes"]
fn regrade_retained_p2p_outcomes() -> Result<()> {
    let directory = PathBuf::from(std::env::var("GENTS_P2P_REPLAY_INPUT")?);
    let output = PathBuf::from(std::env::var("GENTS_P2P_REPLAY_OUTPUT")?);
    ensure!(!output.exists(), "regrade output already exists");
    let mut grades = Vec::new();
    for entry in std::fs::read_dir(&directory)? {
        let trial = entry?.path();
        let path = trial.join("report.json");
        if !path.exists() {
            continue;
        }
        let reports: Vec<Value> = serde_json::from_slice(&std::fs::read(&path)?)?;
        for report in reports {
            let case = report["case_id"].as_str().unwrap();
            let trace: Vec<Value> =
                serde_json::from_slice(&std::fs::read(trial.join(case).join("tool-trace.json"))?)?;
            grades.push(json!({"trial":trial.file_name().unwrap().to_str(),"case_id":case,"grading_version":NATIVE_GRADING_VERSION,"original_v2_passed":report.get("original_v2_passed").unwrap_or(&report["passed"]),"passed":native_outcome(&report,&trace),"answer_audit":"unassessed"}));
        }
    }
    std::fs::write(output, serde_json::to_vec_pretty(&grades)?)?;
    Ok(())
}

#[test]
fn p2p_case_prompts_cover_distinct_native_outcomes() {
    let cases = cases();
    assert_eq!(cases.len(), 8);
    assert!(cases.iter().all(|case| case.grading_version == 2));
    for split in ["train", "validation", "held_out"] {
        assert!(cases.iter().any(|c| c.split == split));
    }
    let outcomes: std::collections::BTreeSet<_> = cases.iter().map(|c| &c.expectation).collect();
    assert_eq!(outcomes.len(), cases.len());
}

async fn retained(path: &Path) -> Result<TestDb> {
    let home = EmbeddedHome::create_retained_with_p2p(
        path,
        Some(Arc::new(|path| {
            test_p2p_config(&TestP2pAdmission::default(), path)
        })),
    )
    .await?;
    Ok(test_db_from_home(home))
}

async fn rows(db: &TestDb, collection: &str, fields: &str) -> Result<Vec<Value>> {
    let value = ConfigAccess::Local(db.node.clone())
        .execute(&format!("{{{collection}(limit: 100) {{{fields}}}}}"))
        .await?;
    ensure!(
        value.get("errors").is_none(),
        "native observation failed: {value}"
    );
    Ok(value["data"][collection]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

#[tokio::test]
#[ignore = "requires workstation target and GENTS_P2P_EVAL_OUTPUT; retains two real node homes per case"]
async fn engineer_p2p_live_comparison() -> Result<()> {
    let output = PathBuf::from(std::env::var("GENTS_P2P_EVAL_OUTPUT")?);
    ensure!(!output.exists(), "comparison output already exists");
    std::fs::create_dir_all(&output)?;
    let target = InferenceTarget::selected()?;
    let selector = std::env::var("GENTS_P2P_EVAL_CASES").unwrap_or_default();
    let baseline = std::env::var("GENTS_P2P_EVAL_BASELINE").is_ok_and(|v| v == "1");
    let mut reports = Vec::new();
    for case in cases()
        .into_iter()
        .filter(|c| selector.is_empty() || selector.split(',').any(|id| id == c.case_id))
    {
        let setup = std::time::Instant::now();
        let directory = output.join(&case.case_id);
        let local = retained(&directory.join("home")).await?;
        let remote = retained(&directory.join("remote-home")).await?;
        let did = local.node_identity.did().to_owned();
        let remote_did = remote.node_identity.did().to_owned();
        for db in [&local, &remote] {
            ConfigAccess::Local(db.node.clone())
                .add_schema("type DeploymentNote @branchable { text: String }")
                .await?;
        }
        let (peer, address) = wait_for_peer_identity(&remote.node).await;
        let enrollment = authorize_enrollment_peer(
            local.node.clone(),
            "engineer-eval",
            "Engineer eval",
            local.node_identity.clone(),
            remote.node_identity.clone(),
            &peer,
            &address,
        )
        .await;
        ConfigAccess::Local(remote.node.clone()).write("eval.p2p.note", "mutation {create_DeploymentNote(input: {text: \"Keep the previous rollout image pinned until the owner approves replacement.\"}) {_docID}}").await?;
        let doc_id = rows(&remote, "DeploymentNote", "_docID").await?[0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        ConfigAccess::Local(local.node.clone()).write("eval.p2p.registry", &format!("mutation {{create_PeerRegistry(input: {{peer_id: \"{}\", agent_did: \"{}\", display_name: \"Cedar\", network_id: \"engineer-eval\", addresses: [\"{}\"], status: \"discovered\"}}) {{_docID}}}}", escape_graphql_string(&peer), escape_graphql_string(&remote_did), escape_graphql_string(&address))).await?;
        let target_ids = bind_target(&local.node, local.node_identity.as_ref(), &target).await;
        let subject = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/configurator_evals/ladder/engineer_subject");
        let manifest: PackManifest =
            serde_json::from_slice(&std::fs::read(subject.join("manifest.json"))?)?;
        let mut config = load_pack_config(
            &manifest,
            &PackInstallOptions {
                agent_did: did.clone(),
            },
            &|p| Ok(std::fs::read(subject.join(p))?),
            &|_| None,
        )?;
        config.agent_behaviors[0].inference_profile_id =
            gents::default_inference_profile_id_for_behavior(&target_ids.1);
        let mut profile = target.profile(&did);
        profile.profile_id = config.agent_behaviors[0].inference_profile_id.clone();
        profile.reasoning_effort = Some(gents::config::ReasoningEffort::High);
        profile.sampling_id = Some("p2p-eval-sampling".into());
        config.inference_profiles = vec![profile];
        config.inference_sampling = vec![InferenceSampling {
            agent_did: did.clone(),
            sampling_id: "p2p-eval-sampling".into(),
            temperature: Some(1.0),
            top_p: Some(0.95),
            ..Default::default()
        }];
        let built_ins = config.tools[0].built_ins.as_mut().unwrap();
        built_ins.enable_p2p_tool = Some(true);
        built_ins.enable_p2p_mutations = Some(true);
        built_ins.p2p_collections = vec!["DeploymentNote".into()];
        config.tools[0].datastore = Some(DatastoreTools {
            enable_defra_query: Some(true),
            defra_query_collections: Some(vec![
                "DeploymentNote".into(),
                "PeerRegistry".into(),
                "DataPlanePairingDesired".into(),
                "PeerPairingApplied".into(),
            ]),
            ..Default::default()
        });
        let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
        ConfigAccess::transact_local(&local.node, None, "eval.p2p.subject", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await?;
        let native_tool = P2pTool::new(
            local.node.clone(),
            Some(local.node_identity.clone()),
            true,
            EndpointScope::<String, ()>::only_units(["DeploymentNote".into()]),
        );
        if matches!(
            case.expectation.as_str(),
            "idempotent_pairing" | "overlay_revoked" | "offline_diagnosis"
        ) {
            Tool::call(&native_tool,serde_json::from_value(json!({"argv":["pairings","apply"],"options":{"peer_id":peer,"peer_did":remote_did,"collections":["DeploymentNote"]}}))?).await?;
        }
        if matches!(
            case.expectation.as_str(),
            "idempotent_pairing" | "overlay_revoked"
        ) {
            use gents::agent::p2p_reconcile::{
                reconcile_peer_tick, EmbeddedRemoteP2pAdmin, EnrollmentEndpointEntry,
                GraphqlEnrollmentStore, GraphqlPairingStateStore, PairingStateStore,
            };
            let projection =
                GraphqlEnrollmentStore::new(local.node.clone(), local.node_identity.clone())
                    .load_projection()
                    .await?;
            let active = projection
                .active
                .iter()
                .find(|e| e.request.request_id == enrollment.request_id)
                .unwrap();
            let endpoint = EnrollmentEndpointEntry {
                desired_id: peer.clone(),
                peer_id: peer.clone(),
                agent_did: remote_did.clone(),
                address: address.clone(),
                request_digest: active.request.request_digest.clone(),
                authorization_sequence: active.revision.sequence,
                authorization_expires_at: active.revision.authorization_expires_at.clone(),
            };
            let store = GraphqlPairingStateStore::for_enrollment_materialization(
                local.node.clone(),
                local.node_identity.clone(),
                endpoint,
            );
            let tick = reconcile_peer_tick(
                &EmbeddedRemoteP2pAdmin::new(local.node.clone()),
                &store,
                &peer,
            )
            .await?;
            ensure!(
                tick.live_route_matches
                    && store
                        .load_applied(&peer)
                        .await?
                        .state
                        .collections
                        .contains("DeploymentNote"),
                "fixture did not apply its pre-existing pairing"
            );
        }
        if case.expectation == "offline_diagnosis" {
            remote.node.shutdown().await;
        }
        let before = rows(
            &local,
            "DataPlanePairingDesired",
            "_docID peer_id agent_did collections source",
        )
        .await?;
        let mut policy =
            ToolPolicySurface::ceiling_with_host_modes(FileToolMode::Off, BashMode::Off);
        if baseline {
            policy.p2p_read = false;
            policy.p2p_mutate = false;
            policy.p2p_collections = EndpointScope::none();
        }
        let (runtime, agent) = boot_live_agent_with_options(
            &local,
            local.node_identity.clone(),
            DocumentRuntimeOptions {
                tool_ceiling: ToolCeiling::meta_only().with_policy(policy.clone()),
                ..Default::default()
            },
        )
        .await?;
        let ready_agent = gents::Gents::from_default_behavior_documents(
            local.node.clone(),
            local.node_identity.clone(),
            DocumentRuntimeOptions {
                tool_ceiling: ToolCeiling::meta_only().with_policy(policy),
                backend_health: Some(agent.backend_health()),
                ..Default::default()
            },
        )
        .await?;
        let behavior = ready_agent
            .behaviors()
            .iter()
            .find(|b| b.behavior_id == "engineer")
            .ok_or_else(|| {
                anyhow::anyhow!("ready runtime did not resolve the Engineer behavior")
            })?;
        let surface = behavior.tools.resolve(&local.node, &did).await?;
        let tool_context = ToolRuntimeContext::new_with_agent_did(
            local.node.clone(),
            Default::default(),
            Default::default(),
            "localhost",
            None,
            &did,
            Some(local.node_identity.clone()),
        );
        let tools = surface.build_tools(&tool_context).await?;
        let mut definitions = Vec::new();
        for tool in tools {
            definitions.push(tool.definition(String::new()).await);
        }
        std::fs::write(
            directory.join("tool-definitions.json"),
            serde_json::to_vec_pretty(&definitions)?,
        )?;
        let prompt = case
            .prompt
            .replace("{peer_id}", &peer)
            .replace("{peer_did}", &remote_did)
            .replace("{peer_address}", &address)
            .replace("{request_id}", &enrollment.request_id)
            .replace("{doc_id}", &doc_id);
        let setup_ms = setup.elapsed().as_millis();
        let inference = std::time::Instant::now();
        create_runtime_request(
            &local.node,
            &did,
            "engineer",
            &case.case_id,
            &format!("p2p-{}", case.case_id),
            &prompt,
        )
        .await;
        let terminal =
            wait_for_request_terminal(&local.node, &case.case_id, Duration::from_secs(240)).await;
        let answer = terminal_assistant_answer(&local.node, &case.case_id).await;
        let after = rows(
            &local,
            "DataPlanePairingDesired",
            "_docID peer_id agent_did collections source",
        )
        .await?;
        let notes = rows(&local, "DeploymentNote", "_docID text").await?;
        let applied = rows(
            &local,
            "PeerPairingApplied",
            "peer_id collections replicator_addresses",
        )
        .await?;
        let lower = answer.to_lowercase();
        let arrived = notes.iter().any(|n| {
            n["_docID"] == doc_id
                && n["text"]
                    .as_str()
                    .is_some_and(|t| t.contains("previous rollout image"))
        });
        let owned: Vec<_> = after
            .iter()
            .filter(|r| r["peer_id"] == peer && r["source"] == "engineer")
            .collect();
        let enrollment_active = gents::agent::p2p_reconcile::GraphqlEnrollmentStore::new(
            local.node.clone(),
            local.node_identity.clone(),
        )
        .load_projection()
        .await?
        .active
        .iter()
        .any(|e| e.request.request_id == enrollment.request_id);
        let calls = rows(
            &local,
            "AgentToolCall",
            "_docID tool_call_id tool_name request_id session_id requester_did lifecycle_state started_at completed_at",
        )
        .await?;
        let access = ConfigAccess::Local(local.node.clone());
        let mut trace = Vec::new();
        let mut output_bytes = 0;
        let mut failed_calls = 0;
        let mut actionable_errors = 0;
        for call in &calls {
            let presentation = gents::session::load_tool_call_presentation(
                &access,
                call["_docID"].as_str().unwrap(),
                &did,
                call["session_id"].as_str().unwrap(),
                call["requester_did"].as_str(),
            )
            .await?;
            let failed = call["lifecycle_state"] == "failed";
            failed_calls += usize::from(failed);
            if let Some(result) = &presentation.result {
                output_bytes += result.len();
                actionable_errors += usize::from(failed && result.contains("next call:"));
            }
            trace.push(json!({"tool_call":call,"arguments":presentation.arguments,"result":presentation.result,"live_output":presentation.live_output}));
        }
        let observed_connection = trace.iter().any(|entry| {
            entry["tool_call"]["tool_name"] == "p2p"
                && entry["tool_call"]["lifecycle_state"] == "completed"
                && entry["result"].as_str().is_some_and(|result| {
                    serde_json::from_str::<Value>(result).is_ok_and(|value| {
                        value["outcome"].get("connected_peers").is_some()
                            || value["observations"].get("connected_peers").is_some()
                            || value["outcome"].get("connected").is_some()
                    })
                })
        });
        let passed = terminal
            == gents_protocol::request_lifecycle::RequestLifecycleState::Completed.as_str()
            && enrollment_active
            && match case.expectation.as_str() {
                "peer_observation" => {
                    observed_connection
                        && answer.contains(&peer)
                        && answer.contains(&remote_did)
                        && lower.contains("connect")
                }
                "document_arrival" => arrived && owned.len() == 1 && lower.contains("pinned"),
                "idempotent_pairing" => {
                    before == after && owned.len() == 1 && lower.contains("applied")
                }
                "identity_refusal" => {
                    owned.is_empty()
                        && (lower.contains("mismatch")
                            || lower.contains("does not match")
                            || lower.contains("wrong did"))
                }
                "collection_refusal" => {
                    owned.is_empty()
                        && lower.contains("oauthcredential")
                        && (lower.contains("not permitted")
                            || lower.contains("not allowed")
                            || lower.contains("protocol")
                            || lower.contains("scope"))
                }
                "offline_diagnosis" => {
                    !arrived
                        && owned.len() == 1
                        && (lower.contains("offline")
                            || lower.contains("not connected")
                            || lower.contains("unavailable"))
                }
                "overlay_revoked" => owned.is_empty() && lower.contains("enrollment"),
                "identifier_recovery" => {
                    owned.is_empty()
                        && (lower.contains("invalid") || lower.contains("malformed"))
                        && lower.contains("peer")
                }
                _ => false,
            };
        std::fs::write(
            directory.join("tool-trace.json"),
            serde_json::to_vec_pretty(&trace)?,
        )?;
        let usage = rows(&local,"InferenceCall","call_id request_id call_state prompt_tokens completion_tokens cached_input_tokens context_accounting_json").await;
        let remote_documents = if case.expectation == "offline_diagnosis" {
            None
        } else {
            Some(rows(&remote, "DeploymentNote", "_docID text").await?)
        };
        let metrics = json!({"tool_calls":calls.len(),"failed_tool_calls":failed_calls,"errors_naming_next_call":actionable_errors,"tool_output_bytes":output_bytes});
        let capability_evidence = json!({
            "protocol_sync_supported": gents::agent::p2p_reconcile::templates::admit_app_collections(["OAuthCredential".into()].into()).is_some(),
            "application_collection_grant": ["DeploymentNote"],
            "source_peer_certified": false,
            "sync_requires_known_document_ids": true,
            "registry_is_enrollment_authority": false
        });
        let answer_audit = json!({"version":1,"status":"unassessed","method":"independent semantic audit","criteria":[
            "Recovery must be possible through the supported interface; broader grants or ACP do not enable protocol collection sync.",
            "Distinguish registry, signed enrollment, desired pairing, applied pairing and observed connectivity; do not certify the document supplier.",
            "Preserve supplied user inputs and identify only genuinely missing inputs."
        ]});
        let mut report = json!({"case_id":case.case_id,"grading_version":NATIVE_GRADING_VERSION,"prompt_fixture_version":case.grading_version,"original_v2_passed":passed,"split":case.split,"expectation":case.expectation,"baseline_grant_disabled":baseline,"enrollment_preserved":enrollment_active,"observed_connection":observed_connection,"passed":passed,"deterministic_grade":"native effects and successful read receipts; not semantic answer correctness","capability_evidence":capability_evidence,"answer_audit":answer_audit,"terminal":terminal,"answer":answer,"setup_ms":setup_ms,"inference_ms":inference.elapsed().as_millis(),"agent_did":did,"peer_id":peer,"peer_did":remote_did,"document_id":doc_id,"desired_before":before,"desired_after":after,"applied":applied,"local_documents":notes,"remote_documents":remote_documents,"metrics":metrics,"tool_calls":calls,"usage":usage.ok(),"home":local.data_path()});
        report["passed"] = json!(native_outcome(&report, &trace));
        std::fs::write(
            directory.join("evidence.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        reports.push(report);
        std::fs::write(
            output.join("report.json"),
            serde_json::to_vec_pretty(&reports)?,
        )?;
        runtime.shutdown().await;
        local.node.shutdown().await;
        remote.node.shutdown().await;
    }
    Ok(())
}
