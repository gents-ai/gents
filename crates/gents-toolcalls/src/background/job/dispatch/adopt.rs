use super::*;

use gents_store::config_client::ConfigApplyTxn;

struct Prepared {
    ctx: StepContext,
    out: JobStepOutput,
    row: StepRow,
    adoption: Adoption,
}

struct Recorded {
    flipped: ToolCallLifecycle,
    pin: JobPin,
    progress: JobProgress,
}

pub(super) async fn commit_adoption(
    lifecycle: &ToolCallLifecycle,
    c: StepZero<'_>,
) -> Result<Adopted> {
    lifecycle
        .access()?
        .transact_write("plugin_job.adopt_step0", move |txn| {
            Box::pin(async move { adopt_in_txn(txn, lifecycle, c).await })
        })
        .await
}

async fn adopt_in_txn(
    txn: &ConfigApplyTxn<'_>,
    lifecycle: &ToolCallLifecycle,
    c: StepZero<'_>,
) -> Result<Adopted> {
    let prepared = match prepare_in_txn(txn, lifecycle, c).await? {
        Ok(prepared) => prepared,
        Err(refused) => return Ok(refused),
    };
    let recorded = match record_in_txn(txn, lifecycle, c, &prepared).await? {
        Ok(recorded) => recorded,
        Err(settled) => return Ok(settled),
    };
    stage_in_txn(txn, c, prepared, recorded).await
}

async fn prepare_in_txn(
    txn: &ConfigApplyTxn<'_>,
    lifecycle: &ToolCallLifecycle,
    c: StepZero<'_>,
) -> Result<Result<Prepared, Adopted>> {
    let StepZero {
        runtime,
        installed,
        input,
        output,
        effects,
        guest,
        resume,
        coordinate,
        row_doc_id,
        args_digest,
        ..
    } = c;
    let caller = &input.caller;
    let adopted_at = u64::try_from(chrono::Utc::now().timestamp_millis())?;
    let provisional = HostFacts {
        engine_digest: installed.digest.clone(),
        plugin_ref: coordinate.to_owned(),
        cwd: caller.cwd.clone(),
        args_digest: args_digest.clone(),
        chain_root: row_doc_id.clone(),
        origin: JobOrigin::Dynamic,
        adopted_at,
        resume_of: None,
        replayed_from: None,
        ceiling_at_pin: effective_ceiling(None),
    };
    let (mut ctx, _) =
        step_context_for(txn, runtime, input, provisional.clone(), output, lifecycle).await?;
    ctx.started_by_job = caller.started_by_job;
    if effects.is_some() {
        ctx.live_background =
            u32::try_from(count_live_backgrounded_rows_in(txn, &caller.request_doc_id).await?)
                .unwrap_or(u32::MAX);
    }
    let facts = HostFacts {
        origin: call_origin(input.script.as_deref()),
        ceiling_at_pin: ctx.live.clone(),
        ..provisional
    };
    ctx.adoption_facts = facts.clone();
    ctx.resume_facts = facts;
    let mut resumed_by = None;
    if let Some(resume) = resume {
        let (target, by) = resume_target_in_txn(txn, coordinate, &resume.row_doc_id).await?;
        ctx.resume_targets = vec![target];
        resumed_by = by;
        ctx.resume_facts.resume_of = Some(resume.row_doc_id.clone());
        if let Err(refusal) = validate_resume(
            txn,
            &ctx.caller_scope,
            coordinate,
            resume,
            guest,
            &ctx.resume_facts,
        )
        .await?
        {
            return Ok(Err(Adopted::Refused(refusal, None, resumed_by)));
        }
    }
    let mut out = output.clone();
    if resume.is_none() {
        let taken =
            taken_handles(txn, &caller.session_id, lifecycle.tool_name(), row_doc_id).await?;
        if let Some(pin) = pin_mut(&mut out) {
            pin.handle = unique_handle(&taken, &pin.handle);
        }
    }
    let view = ToolCallView {
        state: lifecycle.state(),
        kind: lifecycle.kind(),
        await_mode: lifecycle.await_mode(),
    };
    let row = read_step_row_in_txn(txn, row_doc_id).await?;
    let adoption = match adopt(&ctx, &view, &row, &out) {
        Ok(adoption) => adoption,
        Err(refusal) => {
            return Ok(Err(Adopted::Refused(
                refusal.refusal,
                refusal.detail,
                resumed_by,
            )))
        }
    };
    Ok(Ok(Prepared {
        ctx,
        out,
        row,
        adoption,
    }))
}

async fn record_in_txn(
    txn: &ConfigApplyTxn<'_>,
    lifecycle: &ToolCallLifecycle,
    c: StepZero<'_>,
    prepared: &Prepared,
) -> Result<Result<Recorded, Adopted>> {
    let StepZero {
        runtime,
        input,
        output,
        step0_receipt,
        writes,
        grant,
        row_doc_id,
        ..
    } = c;
    let caller = &input.caller;
    let Prepared { out, adoption, .. } = prepared;
    let pin = adoption
        .row
        .pin
        .clone()
        .context("an adopted row has a pin")?;
    // (b) the flip, with the job's wall deadline
    let deadline_at = chrono::DateTime::from_timestamp_millis(i64::try_from(
        pin.adopted_at + pin.ceiling_at_pin.max_wall_secs * 1000,
    )?)
    .context("the job deadline is out of range")?;
    let flipped = lifecycle
        .adopt_plugin_job_in_txn(txn, deadline_at)
        .await?
        .context("the call is no longer a running foreground call")?;
    // (c) the pin
    write_pin_in_txn(txn, row_doc_id, &pin).await?;
    // (e) initial progress, then step 0's evidence and resume block
    let budget = JobBudget {
        total: pin.agent_budget,
        spent: adoption.row.spent,
        remaining: pin.agent_budget.saturating_sub(adoption.row.spent),
    };
    let origin = serde_json::to_value(pin.origin)?;
    let started = progress_of(output).cloned().unwrap_or_else(|| JobProgress {
        title: pin.script_id.clone(),
        state: JobState::Running,
        pause_reason: None,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 0,
        budget: budget.clone(),
        last_event: format!("origin: {}", origin.as_str().unwrap_or_default()),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 0,
    });
    let progress = write_progress_in_txn(
        txn,
        row_doc_id,
        0,
        started,
        HostProgressFacts {
            spawned: adoption.row.spent,
            budget,
            observed_terminal_children: adoption.row.observed_terminal,
            state: JobState::Running,
            pause_reason: None,
        },
    )
    .await?;
    // The guest state a resume continues from is the prior row's, unless step 0 wrote its own.
    let guest = match (guest_state_of(output), adoption.row.resume.as_ref()) {
        (Some(state), _) => Some(state.clone()),
        (None, Some(resume)) => read_step_state(txn, &resume.row_doc_id)
            .await?
            .and_then(|state| state.guest),
        (None, None) => None,
    };
    write_step_state_in_txn(
        txn,
        row_doc_id,
        step0_receipt,
        adoption.row.resume.as_ref(),
        guest.as_ref(),
    )
    .await?;
    // (d) step 0's data writes
    if !writes.is_empty() {
        let grant = grant
            .as_ref()
            .context("a plugin without data cannot write")?;
        let stamps = crate::plugin::data::StampContext::JobStep {
            caller_did: caller.agent_did.clone(),
            session_id: caller.session_id.clone(),
            request_doc_id: caller.request_doc_id.clone(),
            tool_call_doc_id: row_doc_id.clone(),
            job_row_doc_id: row_doc_id.clone(),
            chain_root_doc_id: pin.chain_root_doc_id.clone(),
        };
        let agent_identity = runtime.agent_identity(&caller.node_did, &caller.agent_did)?;
        let actor = ::identity::Did::new(agent_identity.did().to_owned())?;
        crate::plugin::application_write::apply_plugin_writes_in_txn(
            txn,
            &actor,
            grant,
            &stamps,
            writes.to_vec(),
        )
        .await?;
    }
    if let Some(settlement) = settlement_of(out) {
        // The pinned terminal settles in this transaction; the
        // flip wrote `background`, but the settled payload is
        // the call's own reply.
        let inline = adoption.delivery == Some(Delivery::Inline);
        let (_, payload) = terminal_text(&flipped, settlement.clone())
            .context("a step 0 answer has no terminal")?;
        let terminal = settle_plugin_job_in_txn(txn, &flipped, settlement, inline)
            .await?
            .context("a step 0 answer has no terminal")?;
        return Ok(Err(Adopted::Settled {
            lifecycle: flipped,
            terminal,
            payload,
        }));
    }
    Ok(Ok(Recorded {
        flipped,
        pin,
        progress,
    }))
}

async fn stage_in_txn(
    txn: &ConfigApplyTxn<'_>,
    c: StepZero<'_>,
    prepared: Prepared,
    recorded: Recorded,
) -> Result<Adopted> {
    let StepZero {
        runtime,
        input,
        effects,
        cause,
        ..
    } = c;
    let caller = &input.caller;
    let Prepared { ctx, out, row, .. } = prepared;
    let Recorded {
        flipped,
        pin,
        progress,
    } = recorded;
    let effects = effects.context("only an effects output stays a job")?;
    // (f) the admitted effects
    let staged_row = StepRow {
        pin: Some(pin.clone()),
        caller_cwd: pin.cwd.clone(),
        ..step_row(&ctx, &row, &out)
    };
    let fresh = fresh_spawns(&ctx, &staged_row, &out);
    let job = JobContext {
        ctx: &ctx,
        row: &staged_row,
        cause,
        goals: runtime.session_goals.as_ref(),
    };
    stage_effects_in_txn(txn, &job, &fresh, effects).await?;
    // (g) the answer-first background receipt
    let address = format!("{}:{}", caller.session_id, caller.tool_call_id);
    let receipt = json!({
        "status": "running",
        "address": address,
        "handle": pin.handle,
        "script_id": pin.script_id,
        "tier": progress.last_event,
        "next": format!("Next: `gents automation job show {address}`"),
    })
    .to_string();
    crate::call::lifecycle::publish_background_receipt_in_txn(
        txn,
        &flipped.background_receipt_binding()?,
        &receipt,
    )
    .await?;
    Ok(Adopted::Done {
        lifecycle: flipped,
        receipt,
    })
}
