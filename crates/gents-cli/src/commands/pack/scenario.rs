//! Apply the pack only after runtime readiness; event sources must observe the
//! seed collection before seeding because they process created/first-seen docs.
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::cli_process::{path_arg, run_cli_json};
use super::server::{wait_http, wait_runtime_ready};
use crate::cli::args::{PackInitArgs, PackRunArgs, PackSeedArgs};
use crate::graphql_access::post_graphql;
use background::await_background_completion;
use fan_in::verify_fan_in;
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents_protocol::client_protocol::RequestLifecycleState;
use load::{
    load_manifest, load_manifest_with, read_distribution_manifest, resolve_scenario_dir,
    trigger_source_collections,
};
use provenance::{verify_source_edges, verify_stage_provenance};
use results::{count_rows, load_result_documents, render_projection_artifacts, token_totals};
use setup::{
    allow_scenario_folders, default_job_id, install_dependencies, pack_init_cli_args,
    pre_store_with_packs, resolve_manifest_tool_root, resolve_prepare_within, seed_mutation,
    spawn_server_with_pack, stage_scenario_pack, DependencyInstall,
};
use stages::{await_stages, StageResult};
use tool_calls::verify_tool_call_expectations;
use wait::{
    event_backed_trigger_ready, has_correlated_request, wait_for_event_source,
    wait_for_server_ready_marker, wait_http_ok, wait_until,
};

mod background;
mod fan_in;
mod load;
mod manifest;
mod prepare;
mod provenance;
mod results;
mod setup;
mod stages;
mod tool_calls;
mod validate;
mod wait;

pub(super) fn validate_scenario_defaults(dir: &Path) -> Result<()> {
    let distribution = read_distribution_manifest(dir)?;
    load_manifest_with(dir, &distribution, &|_| None)?;
    Ok(())
}

pub(crate) async fn init_pack(args: PackInitArgs) -> Result<()> {
    let bin = std::env::current_exe().context("resolving the gents binary path")?;
    let (pack, _cache_lease, distribution) = resolve_scenario_dir(&args.pack, None).await?;
    let manifest = load_manifest(&pack, &distribution)?;
    tracing::info!(pack = %manifest.name, description = %manifest.description, "initializing pack scenario");
    let home = args.home;
    if home.join("init.json").is_file() && !args.overwrite {
        bail!(
            "home {} already initialized; pass --overwrite to replace it",
            home.display()
        );
    }
    std::fs::create_dir_all(&home)
        .with_context(|| format!("creating pack home {}", home.display()))?;
    let tool_root = resolve_manifest_tool_root(&pack, &manifest)?;
    let init = run_cli_json(
        &bin,
        &pack_init_cli_args(&home, &manifest, tool_root.as_deref()),
    )
    .await?;
    allow_scenario_folders(&pack, &manifest, &home)?;
    let node_did = init
        .get("node_did")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    println!(
        "initialized pack {} at {} ({node_did})",
        manifest.name,
        home.display()
    );
    Ok(())
}

pub(crate) async fn seed(args: PackSeedArgs) -> Result<()> {
    let (pack, _cache_lease, distribution) = resolve_scenario_dir(&args.pack, None).await?;
    let manifest = load_manifest(&pack, &distribution)?;

    let port = args.http_port;
    // The pack node refuses anonymous writes; the seed signs as the default
    // home's node identity.
    let graphql = crate::resolve_graphql_endpoint(
        Some(&format!("http://127.0.0.1:{port}/api/v0/graphql")),
        None,
    )?;
    let healthz = format!("http://127.0.0.1:{port}/healthz");
    wait_http_ok(&healthz, Duration::from_secs(120))
        .await
        .with_context(|| format!("start the pack node first (waiting on {healthz})"))?;
    wait_until(
        &format!("Trigger on {}", manifest.seed.collection),
        Duration::from_secs(60),
        || {
            let graphql = graphql.clone();
            let collection = manifest.seed.collection.clone();
            async move {
                let response = post_graphql(
                    &graphql,
                    "{ Trigger { trigger_id source enabled } EventSource { event_source_id source_collection } }",
                )
                .await?;
                Ok(event_backed_trigger_ready(&response, &collection))
            }
        },
    )
    .await
    .context("start the pack node first")?;

    let job_id = args.job_id.clone().unwrap_or_else(default_job_id);
    let prompt = args
        .prompt
        .clone()
        .unwrap_or_else(|| manifest.default_prompt.clone());
    if prompt.trim().is_empty() {
        bail!("no prompt: pass --prompt or give the pack a default_prompt");
    }

    let mutation = seed_mutation(&manifest.seed, &job_id, &prompt)?;
    ConfigAccess::Graphql(graphql.clone())
        .write("cli.pack.seed", &mutation)
        .await
        .context("seeding the pack")?;

    wait_until(
        &format!("request for {job_id}"),
        Duration::from_secs(60),
        || {
            let graphql = graphql.clone();
            let job_id = job_id.clone();
            async move {
                let query = format!(
                    "{{ AgentRequest(filter: {{ caused_by_correlation: {{ _eq: \"{}\" }} }}) {{ request_id }} }}",
                    escape_graphql_string(&job_id)
                );
                let response = post_graphql(&graphql, &query).await?;
                Ok(has_correlated_request(&response))
            }
        },
    )
    .await
    .with_context(|| {
        format!(
            "the {} was written but no request fired; the event source was not observing yet",
            manifest.seed.collection
        )
    })?;

    println!("seeded {} run_id={job_id}", manifest.seed.collection);
    if let Some(page_port) = args.page_port {
        println!("page     http://127.0.0.1:{page_port}/?run={job_id}");
    }
    Ok(())
}

pub(crate) async fn run(args: PackRunArgs) -> Result<()> {
    let bin = std::env::current_exe().context("resolving the gents binary path")?;
    let (pack, _cache_lease, distribution) =
        resolve_scenario_dir(&args.pack, args.registry.as_deref()).await?;
    let mut manifest = load_manifest(&pack, &distribution)?;
    let observed_collections = trigger_source_collections(&manifest, &manifest.expect.trigger_ids)?;
    let job_id = args.job_id.clone().unwrap_or_else(default_job_id);
    let prompt = args
        .prompt
        .clone()
        .unwrap_or_else(|| manifest.default_prompt.clone());
    if prompt.trim().is_empty() {
        bail!("no prompt: pass --prompt or give the pack a default_prompt");
    }

    let run_dir = pack.join("runs").join(&job_id);
    std::fs::create_dir_all(&run_dir)
        .with_context(|| format!("creating run directory {}", run_dir.display()))?;

    // Triggers are first-seen, so a reused home can silently skip a stage
    // whose source rows already existed.
    let owned_home = args.home.is_none();
    let home = args.home.clone().unwrap_or_else(|| run_dir.join("home"));
    if owned_home && home.exists() {
        std::fs::remove_dir_all(&home).ok();
    }
    std::fs::create_dir_all(&home)
        .with_context(|| format!("creating pack home {}", home.display()))?;

    println!("pack     {} ({})", manifest.name, pack.display());
    println!("job_id   {job_id}");
    println!("run dir  {}", run_dir.display());
    println!("endpoint {}", manifest.init.inference_url);
    println!("model    {}", manifest.init.model_name);

    let tool_root = resolve_manifest_tool_root(&pack, &manifest)?;
    if let Some(root) = tool_root.as_ref() {
        println!(
            "tool     {} @ {}",
            manifest.init.tool_package,
            root.display()
        );
    } else {
        println!("tool     {}", manifest.init.tool_package);
    }

    let init = run_cli_json(
        &bin,
        &pack_init_cli_args(&home, &manifest, tool_root.as_deref()),
    )
    .await?;
    pre_store_with_packs(&home, &args.with_pack)?;
    let node_did = init
        .get("node_did")
        .and_then(Value::as_str)
        .context("init did not return node_did")?
        .to_string();
    let inference_profile_id = init
        .get("inference_profile_id")
        .and_then(Value::as_str)
        .context("init did not return inference_profile_id")?
        .to_string();
    let default_agent_id = init
        .get("default_agent_id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .context("init did not return default_agent_id")?;
    // Scenario homes are initialized with exactly one profile. Bind every
    // declared role explicitly for deterministic role binding.
    let staged_pack = stage_scenario_pack(
        &pack,
        &distribution,
        &node_did,
        &inference_profile_id,
        default_agent_id,
    )?;

    for allowed in allow_scenario_folders(&pack, &manifest, &home)? {
        tracing::info!(path = %allowed.path.display(), access = allowed.access.as_str(), "scenario allowed folder");
    }

    let port = args.http_port;
    let graphql = crate::resolve_graphql_endpoint(
        Some(&format!("http://127.0.0.1:{port}/api/v0/graphql")),
        Some(&home),
    )?;
    let log = run_dir.join("server.log");
    let started = Instant::now();

    // Dependencies install before the node applies the scenario pack, so the
    // pack's documents may name theirs and their plugins are on the host
    // when the agents that call them start.
    install_dependencies(
        &DependencyInstall {
            bin: &bin,
            home: &home,
            registry: args.registry.as_deref(),
            node_did: &node_did,
            inference_profile_id: &inference_profile_id,
            grant_authority: args.grant_authority,
        },
        &manifest.dependencies,
        &manifest.dependency_environment,
    )
    .await?;

    let mut server = spawn_server_with_pack(
        &bin,
        &home,
        port,
        &log,
        staged_pack.path(),
        tool_root.as_deref(),
        manifest.init.tool_root_env_var.as_deref(),
    )?;
    let outcome = async {
        wait_http(&format!("http://127.0.0.1:{port}/healthz"), &mut server).await?;
        wait_for_server_ready_marker(
            &log,
            &mut server,
            Duration::from_secs(manifest.await_timeout_secs),
        )
        .await?;
        wait_runtime_ready(&graphql, &node_did, &mut server).await?;
        println!(
            "runtime  ready; waiting for {} event source collection(s)…",
            observed_collections.len()
        );
        for collection in &observed_collections {
            wait_for_event_source(
                &log,
                collection,
                Duration::from_secs(manifest.await_timeout_secs),
            )
            .await?;
            println!("observing {collection}");
        }

        let prepare_within = resolve_prepare_within(&pack, &manifest)?;
        let prepared_fields = prepare::run_prepare_steps(
            pack.clone(),
            distribution.clone(),
            manifest.prepare.clone(),
            prepare_within,
            args.grant_authority,
        )
        .await?;
        manifest.seed.fields.extend(prepared_fields);

        let mutation = seed_mutation(&manifest.seed, &job_id, &prompt)?;
        ConfigAccess::Graphql(graphql.clone())
            .write("cli.pack.seed", &mutation)
            .await
            .context("seeding the pack")?;
        println!("seeded   1 {} document", manifest.seed.collection);

        let stages = await_stages(
            &graphql,
            &manifest.expect.trigger_ids,
            &manifest.expect.trigger_request_counts,
            &manifest.expect.trigger_request_count_sources,
            &manifest.expect.stage_tool_sequences,
            manifest.expect.workspace_receipt_paths.as_ref(),
            &job_id,
            Duration::from_secs(manifest.await_timeout_secs),
        )
        .await?;

        let background_completion = match &manifest.expect.background_completion {
            Some(expected) => {
                println!(
                    "background waiting for durable caused-request completion acknowledgement…"
                );
                Some(
                    await_background_completion(
                        &bin,
                        &home,
                        &graphql,
                        &node_did,
                        expected,
                        Duration::from_secs(manifest.await_timeout_secs),
                    )
                    .await?,
                )
            }
            None => None,
        };

        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for collection in manifest.expect.collection_counts.keys() {
            counts.insert(collection.clone(), count_rows(&graphql, collection).await);
        }
        let signer_identity = gents::identity::commit_signer_identity_for_did(&node_did)?;
        let provenance = if manifest.expect.signed_provenance {
            let mut evidence = Vec::with_capacity(stages.len());
            for stage in &stages {
                evidence.push(
                    verify_stage_provenance(
                        &graphql,
                        stage,
                        &signer_identity,
                        manifest
                            .expect
                            .required_tool_call_trigger_ids
                            .contains(&stage.trigger_id),
                    )
                    .await
                    .with_context(|| {
                        format!("verifying signed provenance for {}", stage.request_id)
                    })?,
                );
            }
            evidence
        } else {
            Vec::new()
        };
        let source_edges = verify_source_edges(
            &graphql,
            &manifest.expect.source_edges,
            &stages,
            &provenance,
            &signer_identity,
        )
        .await
        .context("verifying durable source edges")?;
        let fan_in = match manifest.expect.fan_in.as_ref() {
            Some(expected) => verify_fan_in(&graphql, expected, &job_id, &node_did).await?,
            None => None,
        };
        let mut projection_requests = stages.clone();
        if let Some(evidence) = &background_completion {
            projection_requests.extend(evidence.completed_wake_request_ids.iter().map(
                |request_id| StageResult {
                    trigger_id: "background_completion".to_string(),
                    request_id: request_id.clone(),
                    lifecycle_state: RequestLifecycleState::Completed,
                    caused_by_source_doc_id: None,
                },
            ));
        }
        let projection_artifacts = render_projection_artifacts(
            &bin,
            &home,
            &graphql,
            &run_dir,
            &projection_requests,
            &manifest.expect.projections,
        )
        .await?;
        verify_tool_call_expectations(&graphql, &stages, &manifest.expect.tool_calls)
            .await
            .context("verifying persisted tool calls")?;
        let result_documents =
            load_result_documents(&graphql, &manifest.expect.result_documents, &job_id)
                .await
                .context("loading configured result documents")?;
        let (prompt_tokens, completion_tokens) = token_totals(&graphql).await;
        Ok::<_, anyhow::Error>((
            stages,
            background_completion,
            counts,
            provenance,
            source_edges,
            fan_in,
            projection_artifacts,
            result_documents,
            prompt_tokens,
            completion_tokens,
        ))
    }
    .await;

    let _ = server.start_kill();

    let (
        stages,
        background_completion,
        counts,
        provenance,
        source_edges,
        fan_in,
        projection_artifacts,
        result_documents,
        prompt_tokens,
        completion_tokens,
    ) = match outcome {
        Ok(values) => values,
        Err(error) => {
            eprintln!("\nrun failed: {error:#}");
            eprintln!("server log: {}", log.display());
            return Err(error);
        }
    };

    let elapsed = started.elapsed();
    let result_path = if result_documents.is_empty() {
        None
    } else {
        let path = run_dir.join("results.json");
        let text = serde_json::to_string_pretty(&result_documents)?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Some(path)
    };
    let mut failures: Vec<String> = Vec::new();
    for stage in &stages {
        if stage.lifecycle_state != RequestLifecycleState::Completed {
            failures.push(format!(
                "{} ended {}",
                stage.trigger_id, stage.lifecycle_state
            ));
        }
    }
    for (collection, expected) in &manifest.expect.collection_counts {
        let actual = counts.get(collection).copied().unwrap_or(0);
        if actual < *expected {
            failures.push(format!(
                "{collection}: expected at least {expected}, found {actual}"
            ));
        }
    }

    let meta = json!({
        "pack": manifest.name,
        "job_id": job_id,
        "node_did": node_did,
        "endpoint": manifest.init.inference_url,
        "model": manifest.init.model_name,
        "elapsed_secs": elapsed.as_secs(),
        "prompt": prompt,
        "stages": stages.iter().map(|s| json!({
            "trigger_id": s.trigger_id,
            "request_id": s.request_id,
            "lifecycle_state": s.lifecycle_state,
            "caused_by_source_doc_id": s.caused_by_source_doc_id,
        })).collect::<Vec<_>>(),
        "background_completion": background_completion.as_ref().map(|evidence| json!({
            "completed_caused_request_ids": evidence.completed_caused_request_ids,
            "failed_caused_request_ids": evidence.failed_caused_request_ids,
            "completed_wake_request_ids": evidence.completed_wake_request_ids,
            "pending_notifications": evidence.pending_notifications,
            "acknowledged_notifications": evidence.acknowledged_notifications,
            "stranded_notifications": evidence.stranded_notifications,
            "diagnostics": evidence.diagnostics,
        })),
        "collection_counts": counts,
        "provenance": provenance.iter().map(|evidence| json!({
            "request_id": evidence.request_id,
            "request_doc_id": evidence.request_doc_id,
            "rendered_request_count": evidence.rendered_request_count,
            "request_commit_cids": evidence.request_commit_cids,
            "request_fact_counts": evidence.request_fact_counts,
            "terminal_output": evidence.terminal_output,
            "signer_identity": evidence.signer_identity,
        })).collect::<Vec<_>>(),
        "source_edges": source_edges.iter().map(|edge| json!({
            "producer_trigger_id": edge.producer_trigger_id,
            "producer_request_id": edge.producer_request_id,
            "producer_request_doc_id": edge.producer_request_doc_id,
            "producer_tool_name": edge.producer_tool_name,
            "producer_tool_call_doc_id": edge.producer_tool_call_doc_id,
            "source_collection": edge.source_collection,
            "source_doc_id": edge.source_doc_id,
            "source_commit_cids": edge.source_commit_cids,
            "consumer_trigger_id": edge.consumer_trigger_id,
            "consumer_request_id": edge.consumer_request_id,
            "consumer_request_doc_id": edge.consumer_request_doc_id,
        })).collect::<Vec<_>>(),
        "fan_in": fan_in.as_ref().map(|evidence| json!({
            "correlation": evidence.correlation,
            "expected_count": evidence.expected_count,
            "member_count": evidence.member_count,
            "result_count": evidence.result_count,
            "consumer_request_id": evidence.consumer_request_id,
            "report_count": evidence.report_count,
            "candidate_count": evidence.candidate_count,
            "confirmed_count": evidence.confirmed_count,
            "refuted_count": evidence.refuted_count,
            "decision_count": evidence.decision_count,
            "verification_summary_count": evidence.verification_summary_count,
            "final_consumer_request_id": evidence.final_consumer_request_id,
        })),
        "projection_artifacts": projection_artifacts,
        "result_artifact": result_path.as_ref().map(|path| path_arg(path)),
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "ok": failures.is_empty(),
        "failures": failures,
    });
    let meta_path = run_dir.join("meta.json");
    if let Ok(text) = serde_json::to_string_pretty(&meta) {
        let _ = std::fs::write(&meta_path, text);
    }

    println!();
    for stage in &stages {
        println!(
            "  {:<12} {:<10} {}",
            stage.trigger_id, stage.lifecycle_state, stage.request_id
        );
    }
    for (collection, actual) in &counts {
        println!("  {collection:<12} {actual} document(s)");
    }
    if let Some(evidence) = &background_completion {
        println!(
            "  background   {} child request(s), {} wake(s), {} acknowledged, {} pending, {} stranded",
            evidence.completed_caused_request_ids.len(),
            evidence.completed_wake_request_ids.len(),
            evidence.acknowledged_notifications,
            evidence.pending_notifications,
            evidence.stranded_notifications,
        );
    }
    println!(
        "  tokens       {prompt_tokens} prompt + {completion_tokens} completion in {}s",
        elapsed.as_secs()
    );
    println!("  artifacts    {}", meta_path.display());
    if let Some(path) = &result_path {
        println!("  results      {}", path.display());
    }
    if owned_home && !args.keep_home {
        std::fs::remove_dir_all(&home).ok();
    } else {
        println!("  home         {}", home.display());
    }

    if failures.is_empty() {
        println!("\nok");
        Ok(())
    } else {
        bail!(
            "pack scenario run did not meet expectations: {}",
            failures.join("; ")
        )
    }
}

#[cfg(test)]
mod tests;
