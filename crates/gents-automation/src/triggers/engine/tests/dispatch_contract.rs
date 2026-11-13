use super::*;

pub(super) async fn event_group_eligibility_matches_lean_generated_contract_cases() {
    gents_lean_vocab::families::event_group_contracts_eligibility::replay(|case| {
        gents_lean_vocab::families::event_group_contracts_eligibility::EligibilityOutput {
            eligible: crate::triggers::engine::event_source::group_candidate_eligible(
                case.actual_count as usize,
                case.expected_count.map(|count| count as usize),
                case.minimum_count as usize,
                case.timed_out,
                case.well_formed,
            ),
        }
    });
}

pub(super) async fn trigger_engine_dispatch_matches_lean_generated_contract_cases() {
    for case in gents_lean_vocab::families::trigger_contracts_engine_dispatch::cases::<
        String,
        String,
        String,
    >() {
        let input = &case.input;
        let trigger_kind = trigger_kind_from_lean(&input.trigger_kind);
        let concurrency = concurrency_from_lean(&input.concurrency);
        let task = spy_resolved_task(&format!("lean case {}", input.name));
        let snapshot = snapshot_from_trigger_contract(
            &input.active_schedule_ids,
            &input.active_event_trigger_ids,
            &task,
            concurrency,
        );
        let (_tx, rx) = watch::channel(snapshot);
        let materializer = SpyMaterializer::new();
        materializer.track_materialized_nonterminal();
        for prior in &input.prior_requests {
            if let (Some(prior_trigger_id), false) = (prior.caused_by.as_deref(), prior.is_terminal)
            {
                materializer.mark_nonterminal_request(
                    prior_trigger_id,
                    trigger_kind,
                    prior.id.clone(),
                );
            }
        }
        let engine = TriggerEngine::new(rx, materializer.clone());
        let event_vars = if matches!(trigger_kind, TriggerKind::Event) {
            serde_json::json!({"source_doc_id": "lean-event-source-doc"})
        } else {
            serde_json::json!({})
        };

        let intent = FireIntent {
            trigger_id: input.trigger_id.clone(),
            trigger_kind,
            task,
            concurrency,
            event_vars,
            doc_vars: None,
            correlation: None,
            group_vars: None,
            trigger_context: None,
            args_vars: None,
            durable_fire_key: "contract-fire".to_string(),
            pre_materialized_request_id: None,
            event_source_generation: None,
            on_result: Box::new(|_| {}),
        };

        let (result, skip_reason) = match engine.dispatch(intent).await {
            FireResult::Fired { .. } => ("fired".to_owned(), None),
            FireResult::Skipped { reason } => ("skipped".to_owned(), Some(reason)),
            other => (format!("{other:?}"), None),
        };

        let calls = materializer.calls();
        let materialized = calls.first().map(|(trigger_id, kind, rendered)| {
            let body = format!("lean case {}", input.name);
            let prompt_matches = match kind {
                TriggerKind::Manual => *rendered == body,
                kind => {
                    let label = if kind == &TriggerKind::Event {
                        "event"
                    } else {
                        "scheduled"
                    };
                    rendered.starts_with(&format!(
                        "[{label} trigger:{} · ",
                        trigger_id.as_deref().unwrap_or_default()
                    )) && rendered.ends_with(&format!("]\n\n{body}"))
                }
            };
            assert!(
                prompt_matches,
                "Lean case {} rendered prompt drifted: {rendered}",
                input.name
            );
            (trigger_id.clone(), *kind)
        });
        assert_eq!(
            materializer.source_doc_ids(),
            materialized
                .iter()
                .map(|_| matches!(trigger_kind, TriggerKind::Event)
                    .then(|| "lean-event-source-doc".to_string()))
                .collect::<Vec<_>>(),
            "Lean case {} source document lineage drifted",
            input.name
        );

        case.check(
            gents_lean_vocab::families::trigger_contracts_engine_dispatch::DispatchOutput {
                result,
                skip_reason,
                materialize_trigger_id: materialized
                    .as_ref()
                    .and_then(|(trigger_id, _)| trigger_id.clone()),
                materialize_trigger_kind: materialized
                    .as_ref()
                    .map(|(_, kind)| kind.as_str().to_owned()),
                request_caused_by_id: materialized
                    .as_ref()
                    .and_then(|(trigger_id, _)| trigger_id.clone()),
                request_caused_by_kind: materialized.as_ref().and_then(|(trigger_id, kind)| {
                    trigger_id.as_ref().map(|_| kind.as_str().to_owned())
                }),
                execution_origin: materialized
                    .as_ref()
                    .map(|(_, kind)| execution_origin_for_trigger_kind(*kind).as_str().to_owned()),
                supersede_call_keys: materializer.supersede_calls(),
                superseded_prior_ids: materializer.superseded_request_ids(),
                target_nonterminal_count_after: input.trigger_id.as_deref().map(|trigger_id| {
                    materializer.nonterminal_count_for(trigger_id, trigger_kind) as u64
                }),
                request_count_after: input.request_count_before + calls.len() as u64,
            },
        );
    }
}
