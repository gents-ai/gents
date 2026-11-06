use std::path::Path;

use anyhow::Result;
use gents_protocol::job::{JobPin, JobStepInput, JobStepOutput};

use super::super::admit::{HostFacts, StepContext};
use super::super::origin::engine_guard;
use super::super::settings::effective_ceiling;
use super::JobRuntime;
use crate::plugin::store::{parse_coordinate, read_published_record, InstalledPlugin};
use crate::store::config_client::ConfigApplyTxn;
use crate::tools::background::status::ProcessControlScope;
use crate::tools::call::lifecycle::ToolCallLifecycle;
use crate::tools::surface::EndpointScope;

/// Every fact admission reads about the caller, the plugin and the host,
/// read through `txn`. The step-0 facts are inert: a pinned row never
/// consults them.
pub(super) async fn step_context(
    txn: &ConfigApplyTxn<'_>,
    runtime: &JobRuntime,
    input: &JobStepInput,
    pin: &JobPin,
    output: &JobStepOutput,
    lifecycle: &ToolCallLifecycle,
) -> Result<(StepContext, Option<InstalledPlugin>)> {
    let facts = HostFacts {
        engine_digest: pin.engine_digest.clone(),
        plugin_ref: pin.plugin_ref.clone(),
        cwd: pin.cwd.clone(),
        args_digest: pin.args_digest.clone(),
        chain_root: pin.chain_root_doc_id.clone(),
        origin: pin.origin,
        adopted_at: pin.adopted_at,
        resume_of: pin.resume_of.clone(),
        replayed_from: pin.replayed_from.clone(),
        ceiling_at_pin: pin.ceiling_at_pin.clone(),
    };
    step_context_for(txn, runtime, input, facts, output, lifecycle).await
}

/// [`step_context`] over the host `facts` a step is admitted with; they name
/// the plugin and the working folder the live settings are read for.
pub(in crate::tools::background::job) async fn step_context_for(
    txn: &ConfigApplyTxn<'_>,
    runtime: &JobRuntime,
    input: &JobStepInput,
    facts: HostFacts,
    output: &JobStepOutput,
    lifecycle: &ToolCallLifecycle,
) -> Result<(StepContext, Option<InstalledPlugin>)> {
    let caller = &input.caller;
    let (_, tools, targets) = crate::request::admission::load_request_context_in_txn(
        txn,
        &caller.node_did,
        &caller.agent_did,
    )
    .await?;
    let settings = tools
        .and_then(|tools| tools.integrations)
        .and_then(|integrations| integrations.plugins)
        .into_iter()
        .flatten()
        .find(|plugin| plugin.plugin == facts.plugin_ref)
        .and_then(|plugin| plugin.job);
    let observed = match parse_coordinate(&facts.plugin_ref) {
        Ok((namespace, name)) => match read_published_record(&runtime.home, namespace, name) {
            Ok(record) => Some(record),
            Err(error) if error.is_busy() => return Err(error.into()),
            Err(_) => None,
        },
        Err(_) => None,
    };
    let (installed, _guard) = match observed {
        Some((record, guard)) => (Some(record), Some(guard)),
        None => (None, None),
    };
    let policy = settings.as_ref().map(|settings| &settings.policy);
    let cwd = Path::new(&facts.cwd);
    let key = |target: &crate::documents::AgentTargetDocument| {
        (target.target_node_did.clone(), target.agent_did.clone())
    };
    let job_ids = settings
        .as_ref()
        .map(|settings| settings.agent_targets.as_slice());
    let mut target_hops = Vec::new();
    let mut within = Vec::new();
    let mut cwd_allowed = Vec::new();
    if let JobStepOutput::Effects { effects } = output {
        for spawn in &effects.spawn {
            if let Some(target) = targets
                .iter()
                .find(|target| target.target_id == spawn.target)
            {
                if !target_hops.iter().any(|(pair, _)| pair == &key(target)) {
                    target_hops.push((key(target), 0));
                }
            }
            if let Some(dir) = &spawn.cwd {
                let resolved = cwd.join(dir).canonicalize().ok();
                if resolved
                    .as_deref()
                    .zip(cwd.canonicalize().ok())
                    .is_some_and(|(dir, base)| dir.starts_with(base))
                {
                    within.push((facts.cwd.clone(), dir.clone()));
                }
                if resolved.as_deref().is_some_and(|dir| {
                    crate::tools::workspace::require_under_ceiling(
                        dir,
                        runtime.tool_root.as_deref(),
                    )
                    .is_ok()
                }) {
                    cwd_allowed.push(dir.clone());
                }
            }
        }
    }
    let ctx = StepContext {
        live: effective_ceiling(settings.as_ref()),
        available: settings.is_some() && installed.is_some(),
        caller_targets: EndpointScope::<(String, String), ()>::only_units(targets.iter().map(key)),
        job_targets: match job_ids {
            Some(ids) => EndpointScope::<(String, String), ()>::only_units(
                targets
                    .iter()
                    .filter(|target| ids.contains(&target.target_id))
                    .map(key),
            ),
            None => EndpointScope::none(),
        },
        policy: policy.cloned().unwrap_or_default(),
        engine_digest: installed
            .as_ref()
            .map(|plugin| plugin.digest.clone())
            .unwrap_or_default(),
        engine_provenance_operator: installed
            .as_ref()
            .is_some_and(|plugin| engine_guard(&runtime.home, plugin).is_ok()),
        max_hop: crate::request::lifecycle::RequestHopCapObservation::of(
            txn.runtime_max_request_hop(&caller.node_did).await?,
        )
        .bound(),
        target_hops,
        target_ids: targets
            .iter()
            .map(|target| (target.target_id.clone(), key(target)))
            .collect(),
        live_background: 0,
        started_by_job: false,
        cwd_allowed,
        within,
        script_digest: super::super::admit::script_digest_of(input.script.as_deref()),
        adoption_facts: facts.clone(),
        caller_scope: ProcessControlScope {
            request_id: caller.request_id.clone(),
            session_id: caller.session_id.clone(),
            node_did: caller.node_did.clone(),
            requester_did: lifecycle.requester_did().map(str::to_owned),
        },
        control_targets: Vec::new(),
        resume_targets: Vec::new(),
        resume_facts: facts,
    };
    Ok((ctx, installed))
}
