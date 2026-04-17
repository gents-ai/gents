use super::*;

pub(crate) fn tool_loop_prompt(label: &str, directory: &str, file_paths: &[String]) -> String {
    format!(
        "This is live desktop tool-loop request {label} for {directory}. Call read_file separately for each of these files, in this exact order: {}. Reply with only the file tokens in that same order, separated by a single space. Do not guess or reuse tokens from another conversation.",
        file_paths.join(", ")
    )
}

pub(crate) fn assert_response_contains_tokens(
    label: &str,
    response: &str,
    expected_tokens: &[String],
) -> Result<()> {
    let mut search_from = 0;
    for token in expected_tokens {
        let offset = response[search_from..].find(token.as_str()).ok_or_else(|| {
            anyhow!(
                "{label} response missing expected token {token} after offset {search_from}: {response}"
            )
        })?;
        search_from += offset + token.len();
    }
    Ok(())
}

pub(crate) fn wait_for_session_tool_activity(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    session_id: &str,
    expected_list_files: usize,
    expected_read_files: usize,
    expected_tokens: &[String],
) -> Result<String> {
    wait_for_value(label, Duration::from_secs(90), || {
        runtime.block_on(core.refresh_store()).ok()?;
        let snapshot = core.store().snapshot();
        let transcript = snapshot.transcript(session_id);

        let list_calls = transcript
            .tool_calls
            .iter()
            .filter(|row| {
                row.tool_name.as_deref() == Some("list_files")
                    && row.status.as_deref() == Some("completed")
            })
            .count();
        let read_calls = transcript
            .tool_calls
            .iter()
            .filter(|row| {
                row.tool_name.as_deref() == Some("read_file")
                    && row.status.as_deref() == Some("completed")
            })
            .collect::<Vec<_>>();
        if list_calls < expected_list_files || read_calls.len() < expected_read_files {
            return None;
        }

        let read_results = transcript
            .tool_results
            .iter()
            .filter(|row| row.tool_name.as_deref() == Some("read_file"))
            .filter_map(|row| row.output_text.clone())
            .collect::<Vec<_>>();
        if !expected_tokens.iter().all(|token| {
            read_results
                .iter()
                .any(|result| result.contains(token.as_str()))
        }) {
            return None;
        }

        read_calls.last().map(|tool_call| {
            tool_call
                .tool_call_id
                .clone()
                .unwrap_or_else(|| tool_call.tool_call_key.clone())
        })
    })
}
