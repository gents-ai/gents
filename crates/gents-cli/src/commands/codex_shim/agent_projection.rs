use rapidhash::{HashMapExt, RapidHashMap};

use gents::toolset::{AGENT_MESSAGE_TOOL_NAME, AGENT_NEW_TOOL_NAME};
use gents_codex_protocol as codex;
use serde_json::Value;

use super::progress::{observed_tool_status, GentsToolCallProgress};
use super::projection_state::{AgentProjection, AgentTool, ProjectionStatus};

pub(super) fn agent_projection(tool: &GentsToolCallProgress) -> Option<AgentProjection> {
    let kind = match tool.tool_name.as_str() {
        AGENT_NEW_TOOL_NAME => AgentTool::New,
        AGENT_MESSAGE_TOOL_NAME => AgentTool::Message,
        _ => return None,
    };
    let receiver = session_id(&tool.result).or_else(|| session_id(&tool.args));
    Some(AgentProjection {
        tool: kind,
        status: observed_tool_status(tool),
        receiver_thread_id: receiver,
    })
}

pub(super) fn agent_tool_item(
    sender_thread_id: &str,
    tool: &GentsToolCallProgress,
    projection: &AgentProjection,
) -> codex::ThreadItem {
    let prompt_field = match projection.tool {
        AgentTool::New => "prompt",
        AgentTool::Message => "message",
    };
    let prompt = serde_json::from_str::<Value>(&tool.args)
        .ok()
        .and_then(|args| args.get(prompt_field)?.as_str().map(ToOwned::to_owned));
    codex::ThreadItem::CollabAgentToolCall {
        id: tool.tool_call_key.clone(),
        tool: match projection.tool {
            AgentTool::New => codex::CollabAgentTool::SpawnAgent,
            AgentTool::Message => codex::CollabAgentTool::SendInput,
        },
        status: codex_agent_status(projection.status),
        sender_thread_id: sender_thread_id.to_string(),
        receiver_thread_ids: projection.receiver_thread_id.iter().cloned().collect(),
        prompt,
        model: None,
        reasoning_effort: None,
        agents_states: RapidHashMap::new(),
    }
}

pub(super) fn observed_agent_projection(item: &codex::ThreadItem) -> Option<AgentProjection> {
    let codex::ThreadItem::CollabAgentToolCall {
        tool,
        status,
        receiver_thread_ids,
        ..
    } = item
    else {
        return None;
    };
    let kind = match tool {
        codex::CollabAgentTool::SpawnAgent => AgentTool::New,
        codex::CollabAgentTool::SendInput => AgentTool::Message,
        _ => return None,
    };
    Some(AgentProjection {
        tool: kind,
        status: match status {
            codex::CollabAgentToolCallStatus::InProgress => ProjectionStatus::InProgress,
            codex::CollabAgentToolCallStatus::Completed => ProjectionStatus::Completed,
            codex::CollabAgentToolCallStatus::Failed => ProjectionStatus::Failed,
        },
        receiver_thread_id: receiver_thread_ids.first().cloned(),
    })
}

fn codex_agent_status(status: ProjectionStatus) -> codex::CollabAgentToolCallStatus {
    match status {
        ProjectionStatus::InProgress => codex::CollabAgentToolCallStatus::InProgress,
        ProjectionStatus::Completed => codex::CollabAgentToolCallStatus::Completed,
        ProjectionStatus::Failed => codex::CollabAgentToolCallStatus::Failed,
    }
}

fn session_id(json: &str) -> Option<String> {
    serde_json::from_str::<Value>(json)
        .ok()?
        .get("session_id")?
        .as_str()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, state: &str, args: &str, result: &str) -> GentsToolCallProgress {
        GentsToolCallProgress {
            tool_call_key: "session:call".into(),
            tool_name: name.into(),
            lifecycle_state: Some(state.into()),
            args: args.into(),
            result: result.into(),
            ..Default::default()
        }
    }

    #[test]
    fn agent_new_projects_a_spawn_item_addressed_to_the_started_session() {
        let started = tool(
            AGENT_NEW_TOOL_NAME,
            "running",
            r#"{"agent":"worker","prompt":"inspect"}"#,
            r#"{"session_id":"child","request_id":"r","tool_call_id":"c"}"#,
        );
        let projection = agent_projection(&started).expect("agent_new is an agent item");
        assert_eq!(projection.tool, AgentTool::New);
        assert_eq!(projection.receiver_thread_id.as_deref(), Some("child"));
        let item = agent_tool_item("parent", &started, &projection);
        assert_eq!(observed_agent_projection(&item), Some(projection));
        let codex::ThreadItem::CollabAgentToolCall { prompt, .. } = item else {
            unreachable!()
        };
        assert_eq!(prompt.as_deref(), Some("inspect"));
    }

    #[test]
    fn agent_message_addresses_its_target_and_other_agent_tools_stay_tools() {
        let message = tool(
            AGENT_MESSAGE_TOOL_NAME,
            "running",
            r#"{"session_id":"child","message":"continue"}"#,
            "",
        );
        let projection = agent_projection(&message).expect("agent_message is an agent item");
        assert_eq!(projection.tool, AgentTool::Message);
        assert_eq!(projection.receiver_thread_id.as_deref(), Some("child"));
        for name in [
            gents::toolset::AGENT_INTERRUPT_TOOL_NAME,
            gents::toolset::AGENT_LIST_TOOL_NAME,
            gents::toolset::AGENT_SESSION_LIST_TOOL_NAME,
            gents::toolset::AGENT_SESSION_READ_TOOL_NAME,
        ] {
            assert!(agent_projection(&tool(name, "completed", "{}", "")).is_none());
        }
    }
}
