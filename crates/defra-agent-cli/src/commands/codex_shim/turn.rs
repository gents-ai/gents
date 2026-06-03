mod active;
mod stream;
mod submission;

use anyhow::Result;
use codex_app_server_protocol as codex;
use tokio::sync::watch;

pub(super) use active::interrupt_active_turn;

use active::{
    cancel_abandoned_steering_request, clear_stream_control_if_current, install_stream_control,
    load_active_codex_turn,
};
use stream::stream_defra_turn;
use submission::create_agent_request_with_retry;

use super::protocol::{
    codex_steering_metadata, codex_turn_metadata, send_committed_user_message, send_error,
    send_notification, send_result, turn_value, user_text_from_input,
};
use super::store::query_node_json;
use super::turn_projection::TurnProjection;
use super::{
    ConnectionState, ShimState, JSONRPC_INTERNAL_ERROR, JSONRPC_INVALID_PARAMS,
    JSONRPC_INVALID_REQUEST,
};
use crate::RequestSubmitOptions;
use defra_agent::graphql::escape_graphql_string;
use serde_json::Value;

/// Resolve explicit Codex skill selections (`UserInput::Skill`, the skill
/// "pill") to their full bodies for DETERMINISTIC injection into the turn.
///
/// Both reference implementations honor an explicit user pick by injecting the
/// body, never by relying on the model to fetch it: Codex injects the full
/// `SKILL.md` as a user-role `<skill>` block per turn, and Hermes preloads the
/// body into the system prompt. The `load_skill` tool is for model-driven
/// discovery, not for an explicit selection — so we inject here. Disabled and
/// foreign-principal skills are skipped. Returns one rendered block per
/// resolved skill, in selection order.
async fn resolve_explicit_skill_injections(
    state: &ShimState,
    input: &[codex::UserInput],
) -> Vec<String> {
    let mut blocks = Vec::new();
    for item in input {
        let codex::UserInput::Skill { name, path } = item else {
            continue;
        };
        // The Codex UI sends our synthetic path (`/defra/skills/<skill_id>`);
        // the final segment is the skill_id. Fall back to the display name.
        let skill_id = path.file_name().and_then(|segment| segment.to_str());
        if let Some(block) = load_skill_injection_block(state, skill_id, name, path).await {
            blocks.push(block);
        }
    }
    blocks
}

async fn load_skill_injection_block(
    state: &ShimState,
    skill_id: Option<&str>,
    selected_name: &str,
    path: &std::path::Path,
) -> Option<String> {
    let did = escape_graphql_string(&state.agent_did);
    let selector = match skill_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => format!(r#"skill_id: {{ _eq: "{}" }}"#, escape_graphql_string(id)),
        None => {
            let name = selected_name.trim();
            if name.is_empty() {
                return None;
            }
            format!(r#"name: {{ _eq: "{}" }}"#, escape_graphql_string(name))
        }
    };
    let query = format!(
        r#"{{ Skill(filter: {{ agent_did: {{ _eq: "{did}" }}, {selector} }}, limit: 1) {{ name instructions enabled }} }}"#
    );
    let response = query_node_json(state.node.as_ref(), &query).await.ok()?;
    let row = response
        .get("data")?
        .get("Skill")?
        .as_array()?
        .first()?
        .clone();
    // Only an enabled skill activates (matches the effective-set rule and the
    // Codex disabled-paths filter).
    if !row.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let instructions = row
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if instructions.trim().is_empty() {
        return None;
    }
    let name = row
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(selected_name);
    Some(format!(
        "<skill>\n<name>{name}</name>\n<path>{}</path>\n{instructions}\n</skill>",
        path.display()
    ))
}

pub(super) async fn start_defra_turn(
    connection: &ConnectionState,
    state: &ShimState,
    request_id: codex::RequestId,
    thread_id: String,
    input: Vec<codex::UserInput>,
) -> Result<()> {
    let user_text = user_text_from_input(&input);
    // Explicit skill selections are injected as full bodies ahead of the user's
    // text (deterministic activation), so a skill-only turn is non-empty.
    let skill_blocks = resolve_explicit_skill_injections(state, &input).await;
    let user_text = if skill_blocks.is_empty() {
        user_text
    } else {
        let mut parts = skill_blocks;
        if !user_text.trim().is_empty() {
            parts.push(user_text);
        }
        parts.join("\n\n")
    };
    if user_text.trim().is_empty() {
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_REQUEST,
            "Codex turn input did not contain text for DEFRA".to_string(),
        )
        .await;
    }

    let cwd = connection
        .thread_cwds
        .lock()
        .await
        .get(&thread_id)
        .cloned()
        .unwrap_or_else(|| state.cwd.clone());
    let metadata = codex_turn_metadata(&cwd);

    let submitted = match create_agent_request_with_retry(
        state,
        &user_text,
        Some(&thread_id),
        RequestSubmitOptions {
            metadata: Some(metadata),
            ..RequestSubmitOptions::default()
        },
    )
    .await
    {
        Ok(submitted) => submitted,
        Err(err) => {
            return send_error(
                &connection.outbound,
                request_id,
                JSONRPC_INTERNAL_ERROR,
                format!("failed to submit DEFRA AgentRequest: {err}"),
            )
            .await;
        }
    };

    let turn_id = submitted.request_id.clone();
    let started_turn = turn_value(&turn_id, codex::TurnStatus::InProgress, Vec::new(), None);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    install_stream_control(connection, thread_id.clone(), turn_id.clone(), cancel_tx).await;

    if let Err(err) = send_result(
        &connection.outbound,
        request_id,
        codex::TurnStartResponse {
            turn: started_turn.clone(),
        },
    )
    .await
    {
        clear_stream_control_if_current(connection, &thread_id, &turn_id).await;
        return Err(err);
    }

    send_notification(
        &connection.outbound,
        state,
        codex::ServerNotification::TurnStarted(codex::TurnStartedNotification {
            thread_id: thread_id.clone(),
            turn: started_turn,
        }),
    )
    .await?;

    send_committed_user_message(&connection.outbound, state, &thread_id, &turn_id, &input).await?;

    let mut projection = TurnProjection::new(state, &thread_id, &turn_id, cwd.clone());
    let result =
        match stream_defra_turn(connection, state, &submitted, &mut projection, cancel_rx).await {
            Ok(()) => Ok(()),
            Err(err) => {
                let message = format!("DEFRA turn failed: {err}");
                projection
                    .append_agent_delta(&connection.outbound, &format!("[agent error] {message}\n"))
                    .await?;
                projection
                    .finish_turn(
                        &connection.outbound,
                        codex::TurnStatus::Failed,
                        Some(message),
                    )
                    .await
            }
        };

    clear_stream_control_if_current(connection, &thread_id, &turn_id).await;
    result
}

pub(super) async fn steer_defra_turn(
    connection: &ConnectionState,
    state: &ShimState,
    request_id: codex::RequestId,
    params: codex::TurnSteerParams,
) -> Result<()> {
    if params.expected_turn_id.trim().is_empty() {
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_REQUEST,
            "expectedTurnId must not be empty".to_string(),
        )
        .await;
    }

    let user_text = user_text_from_input(&params.input);
    if user_text.trim().is_empty() {
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_REQUEST,
            "input must not be empty".to_string(),
        )
        .await;
    }

    let cwd = connection
        .thread_cwds
        .lock()
        .await
        .get(&params.thread_id)
        .cloned()
        .unwrap_or_else(|| state.cwd.clone());

    let Some(active_turn) = load_active_codex_turn(state, &params.thread_id).await? else {
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_PARAMS,
            "no active turn to steer".to_string(),
        )
        .await;
    };
    if active_turn.turn_id != params.expected_turn_id {
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_PARAMS,
            format!(
                "expected active turn id `{}` but found `{}`",
                params.expected_turn_id, active_turn.turn_id
            ),
        )
        .await;
    }

    let turn_id = active_turn.turn_id.clone();
    let queued_after_request_id = active_turn.current_request_id.clone();
    let metadata = codex_steering_metadata(&cwd, &queued_after_request_id);
    let submitted = match create_agent_request_with_retry(
        state,
        &user_text,
        Some(&params.thread_id),
        RequestSubmitOptions {
            metadata: Some(metadata),
            ..RequestSubmitOptions::default()
        },
    )
    .await
    {
        Ok(submitted) => submitted,
        Err(err) => {
            return send_error(
                &connection.outbound,
                request_id,
                JSONRPC_INTERNAL_ERROR,
                format!("failed to submit DEFRA steering AgentRequest: {err}"),
            )
            .await;
        }
    };

    let Some(current_active) = load_active_codex_turn(state, &params.thread_id).await? else {
        cancel_abandoned_steering_request(state, submitted.request_id.clone());
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_PARAMS,
            "active turn ended while submitting steering request".to_string(),
        )
        .await;
    };
    if current_active.turn_id != turn_id {
        let current_turn_id = current_active.turn_id.clone();
        cancel_abandoned_steering_request(state, submitted.request_id.clone());
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_PARAMS,
            format!("active turn changed from `{turn_id}` to `{current_turn_id}`"),
        )
        .await;
    }

    connection
        .remember_steering_input(submitted.request_id.clone(), params.input.clone())
        .await;
    send_result(
        &connection.outbound,
        request_id,
        codex::TurnSteerResponse {
            turn_id: turn_id.clone(),
        },
    )
    .await?;
    tracing::info!(
        turn_id,
        queued_after_request_id,
        steering_request_id = %submitted.request_id,
        "Codex shim accepted active-turn steering request"
    );
    Ok(())
}
