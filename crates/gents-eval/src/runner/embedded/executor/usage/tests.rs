use super::*;
use gents_lean_vocab::families::runtime_eval_usage_charges as family;

fn at(millis: Option<u64>) -> Option<String> {
    millis.map(|millis| {
        chrono::DateTime::from_timestamp_millis(i64::try_from(millis).unwrap())
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    })
}

fn doc(request: u64) -> String {
    format!("doc-{request}")
}

fn request_id(request: u64) -> String {
    format!("req-{request}")
}

fn rows(home: &family::Home) -> HomeRows {
    HomeRows {
        requests: home
            .requests
            .iter()
            .map(|request| RequestRow {
                doc_id: doc(request.id),
                request_id: Some(request_id(request.id)),
                purpose: Some(request.purpose.clone()),
                session_id: Some(request.session.clone()),
                claimed_at: at(request.claimed),
                terminalized_at: at(request.terminal),
            })
            .collect(),
        calls: home
            .calls
            .iter()
            .map(|call| CallRow {
                request_doc_id: call.request.map(doc),
                call_kind: Some(call.kind.clone()),
                call_seq: Some(i64::try_from(call.seq).unwrap()),
                started_at: at(call.started),
                ended_at: at(call.ended),
                first_token_at: at(call.first_token),
                prompt_tokens: call.input,
                completion_tokens: call.output,
                cached_input_tokens: call.cached,
                context_accounting_json: None,
            })
            .collect(),
        tools: home
            .tools
            .iter()
            .map(|tool| ToolRow {
                request_doc_id: tool.request.map(doc),
                tool_name: tool.name.clone(),
                lifecycle_state: Some(tool.state.clone()),
                started_at: at(tool.started),
                completed_at: at(tool.completed),
                plugin_parent_tool_call_doc_id: tool
                    .plugin_parent
                    .then(|| "script-call".to_owned()),
                spawned_by_tool_call_doc_id: tool.spawned.then(|| "spawn-call".to_owned()),
            })
            .collect(),
    }
}

fn totals(totals: CallTotals) -> family::Totals {
    family::Totals {
        calls: totals.calls,
        input: totals.input_tokens,
        output: totals.output_tokens,
        cached: totals.cached_input_tokens,
        unreported: totals.unreported,
    }
}

fn count(count: ToolCount) -> family::Count {
    family::Count {
        calls: count.calls,
        failed: count.failed,
    }
}

#[test]
fn generated_trial_charges_match_lean() {
    family::replay(|home| {
        let rows = rows(home);
        let stages: Vec<String> = home.stages.iter().copied().map(request_id).collect();
        let stages: Vec<&str> = stages.iter().map(String::as_str).collect();
        let charged = home_charge(&rows);
        family::Usage {
            home: family::HomeUsage {
                calls: charged.calls,
                input: charged.usage.input_tokens,
                output: charged.usage.output_tokens,
                reported_input: charged.reported_input_tokens,
                reported_output: charged.reported_output_tokens,
            },
            subject: subject_usage(&rows, &home.session, &stages).map(|subject| family::Subject {
                requests: subject.requests,
                first_request: subject.first_request.is_some(),
                first_prompt_tokens: subject
                    .first_request
                    .as_ref()
                    .and_then(|first| first.prompt_tokens),
                first_request_first_token_ms: subject
                    .first_request
                    .and_then(|first| first.first_token_ms),
                median_first_token_ms: subject.median_first_token_ms,
                turns: totals(subject.turns),
                compaction: totals(subject.compaction),
                direct: count(subject.tool_calls.direct),
                script: count(subject.tool_calls.script),
                inside_script: count(subject.tool_calls.inside_script),
                wall_ms: subject.wall_ms,
                model_ms: subject.model_ms,
                tool_ms: subject.tool_ms,
            }),
        }
    });
}
