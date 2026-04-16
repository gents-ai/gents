use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoverageStatus {
    LiveClick,
    LivePersisted,
    LiveInference,
    LiveMultiAgent,
    SeededArtifact,
    Blocked,
}

#[derive(Debug)]
struct CoverageEntry {
    area: &'static str,
    target: &'static str,
    interaction: &'static str,
    status: CoverageStatus,
    tests: &'static [&'static str],
    next_step: &'static str,
}

const LIVE_INFERENCE_SMOKE: &str = "desktop_app_live_inference_smoke";
const LIVE_OPERATOR_CONFIG: &str = "desktop_app_live_operator_config_round_trips";
const LIVE_OPERATOR_IDENTITIES: &str = "desktop_app_live_operator_identity_field_round_trips";
const LIVE_CHAT_DISCLOSURES: &str = "desktop_app_live_chat_disclosure_artifacts";
const LIVE_CHAT_RETRY_EXPORT: &str = "desktop_app_live_chat_retry_and_export";
const LIVE_OPERATOR_SCHEDULED: &str = "desktop_app_live_operator_scheduled_task_and_failures";
const LIVE_LOGS: &str = "desktop_app_live_logs_event_classification";
const LIVE_MULTI_AGENT_CONFIG: &str =
    "desktop_app_live_multi_agent_server_switching_and_config_inference";

const COVERAGE_MATRIX: &[CoverageEntry] = &[
    CoverageEntry {
        area: "Harness",
        target: "live.multi_server_remote_replication",
        interaction: "Run two remote ClientCore replicas, mutate through the desktop UI, and assert Chat/Operator never show stale data from another server",
        status: CoverageStatus::Blocked,
        tests: &[LIVE_MULTI_AGENT_CONFIG],
        next_step: "Blocked by unreliable local P2P replication in the current harness: true remote-core attempt timed out dialing Alpha and hit ToolSelection snapshot deserialization while replication retried/rate-limited. Keep the same-node multi-agent live journey green until the replica fixture is reliable.",
    },
    CoverageEntry {
        area: "Activity",
        target: audit::targets::ACTIVITY_CHAT,
        interaction: "Open Chat from another activity",
        status: CoverageStatus::LiveClick,
        tests: &[
            LIVE_INFERENCE_SMOKE,
            "desktop_app_clicks_through_activity_bar_navigation",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Activity",
        target: audit::targets::ACTIVITY_OPERATOR,
        interaction: "Open Operator",
        status: CoverageStatus::LiveClick,
        tests: &[
            LIVE_INFERENCE_SMOKE,
            LIVE_OPERATOR_CONFIG,
            LIVE_OPERATOR_SCHEDULED,
            LIVE_LOGS,
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Activity",
        target: audit::targets::ACTIVITY_PEERS,
        interaction: "Open Peers",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_INFERENCE_SMOKE, LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Activity",
        target: audit::targets::ACTIVITY_LOGS,
        interaction: "Open Logs",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_INFERENCE_SMOKE, LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Chat",
        target: audit::targets::CHAT_CREATE_CONVERSATION,
        interaction: "Create a conversation for the selected agent",
        status: CoverageStatus::LiveInference,
        tests: &[
            LIVE_INFERENCE_SMOKE,
            LIVE_MULTI_AGENT_CONFIG,
            LIVE_OPERATOR_CONFIG,
            LIVE_LOGS,
            "desktop_app_clicks_through_live_agent_multi_turn_conversation",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Chat",
        target: audit::targets::CHAT_COMPOSER_TEXT,
        interaction: "Focus and type in the composer",
        status: CoverageStatus::LiveInference,
        tests: &[
            LIVE_INFERENCE_SMOKE,
            LIVE_MULTI_AGENT_CONFIG,
            LIVE_OPERATOR_CONFIG,
            LIVE_LOGS,
            "desktop_app_clicks_through_live_agent_submission",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Chat",
        target: audit::targets::CHAT_SEND,
        interaction: "Submit prompts and observe persisted single-turn and multi-turn transcripts",
        status: CoverageStatus::LiveInference,
        tests: &[
            LIVE_INFERENCE_SMOKE,
            LIVE_MULTI_AGENT_CONFIG,
            LIVE_OPERATOR_CONFIG,
            LIVE_LOGS,
            "desktop_app_clicks_through_live_agent_multi_turn_conversation",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Chat",
        target: audit::targets::CHAT_OPEN_PEERS_SETUP,
        interaction: "Open Peers setup from an empty Chat sidebar",
        status: CoverageStatus::LiveClick,
        tests: &[
            LIVE_OPERATOR_CONFIG,
            "desktop_app_clicks_chat_open_peers_setup_from_empty_sidebar",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Chat",
        target: audit::targets::CHAT_RETRY,
        interaction: "Retry the active conversation turn",
        status: CoverageStatus::LivePersisted,
        tests: &[
            LIVE_CHAT_RETRY_EXPORT,
            "desktop_app_chat_header_retry_and_export_use_transcript_state",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Chat",
        target: audit::targets::CHAT_EXPORT,
        interaction: "Export the active conversation",
        status: CoverageStatus::LivePersisted,
        tests: &[
            LIVE_CHAT_RETRY_EXPORT,
            "desktop_app_chat_header_retry_and_export_use_transcript_state",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Chat",
        target: "chat.deployment.<peer_id>",
        interaction: "Select a deployment from the Chat sidebar",
        status: CoverageStatus::LiveMultiAgent,
        tests: &[
            LIVE_MULTI_AGENT_CONFIG,
            LIVE_LOGS,
            "desktop_app_clicks_through_chat_deployment_and_conversation_switching",
            "desktop_app_clicks_through_peers_selection_toggle_clear_and_remove",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Chat",
        target: "chat.agent.<agent_did>",
        interaction: "Select a nested agent row from the Chat deployment tree",
        status: CoverageStatus::LiveMultiAgent,
        tests: &[
            LIVE_MULTI_AGENT_CONFIG,
            "desktop_app_clicks_through_chat_deployment_and_conversation_switching",
            "desktop_app_clicks_through_peers_selection_toggle_clear_and_remove",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Chat",
        target: "chat.conversation.<session_id>",
        interaction: "Switch between conversations in the Chat sidebar",
        status: CoverageStatus::LiveInference,
        tests: &[
            LIVE_INFERENCE_SMOKE,
            LIVE_MULTI_AGENT_CONFIG,
            "desktop_app_clicks_through_chat_deployment_and_conversation_switching",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Chat",
        target: "chat.tool_card.<card_id>",
        interaction: "Expand a tool-call card in the transcript",
        status: CoverageStatus::SeededArtifact,
        tests: &[
            LIVE_CHAT_DISCLOSURES,
            "desktop_app_executes_tool_loop_and_renders_tool_output",
            "desktop_app_clicks_through_chat_reasoning_and_tool_card_disclosures",
        ],
        next_step: "Promote when the live endpoint reliably emits persisted AgentToolCall rows; deterministic runtime coverage currently verifies real local tool execution and rendered output.",
    },
    CoverageEntry {
        area: "Chat",
        target: "chat.reasoning.<response_key>",
        interaction: "Expand reasoning disclosure in the transcript",
        status: CoverageStatus::SeededArtifact,
        tests: &[
            LIVE_CHAT_DISCLOSURES,
            "desktop_app_clicks_through_chat_reasoning_and_tool_card_disclosures",
        ],
        next_step: "Promote when the live endpoint emits persisted reasoning rows.",
    },
    CoverageEntry {
        area: "Operator",
        target: "operator.deployment.<peer_id>",
        interaction: "Select an Operator deployment",
        status: CoverageStatus::LiveMultiAgent,
        tests: &[LIVE_MULTI_AGENT_CONFIG, LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: "operator.agent.<agent_did>",
        interaction: "Select an agent within the Operator deployment rail",
        status: CoverageStatus::LiveMultiAgent,
        tests: &[LIVE_MULTI_AGENT_CONFIG, LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: "operator.section.Runtime",
        interaction: "Open Runtime inspector",
        status: CoverageStatus::LiveInference,
        tests: &[LIVE_INFERENCE_SMOKE, LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: "operator.section.Behaviors",
        interaction: "Open Behaviors editor",
        status: CoverageStatus::LiveMultiAgent,
        tests: &[LIVE_MULTI_AGENT_CONFIG, LIVE_OPERATOR_CONFIG, LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: "operator.section.Backends",
        interaction: "Open Backends editor",
        status: CoverageStatus::LiveMultiAgent,
        tests: &[
            LIVE_MULTI_AGENT_CONFIG,
            LIVE_INFERENCE_SMOKE,
            LIVE_OPERATOR_CONFIG,
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: "operator.section.Tool Selections",
        interaction: "Open Tool Selections editor",
        status: CoverageStatus::LiveInference,
        tests: &[LIVE_MULTI_AGENT_CONFIG, LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: "operator.section.Inference Profiles",
        interaction: "Open Inference Profiles editor",
        status: CoverageStatus::LiveMultiAgent,
        tests: &[LIVE_MULTI_AGENT_CONFIG, LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: "operator.section.Scheduled Tasks",
        interaction: "Open Scheduled Tasks editor",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_OPERATOR_SCHEDULED],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: "operator.section.Request Timeline",
        interaction: "Open Request Timeline",
        status: CoverageStatus::LiveInference,
        tests: &[LIVE_INFERENCE_SMOKE, LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: "operator.section.Recent Failures",
        interaction: "Open Recent Failures",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_OPERATOR_SCHEDULED],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: "operator.entity.<entity_id>",
        interaction: "Select request/backend/behavior/tool/profile/task/failure rows",
        status: CoverageStatus::LiveMultiAgent,
        tests: &[
            LIVE_MULTI_AGENT_CONFIG,
            LIVE_INFERENCE_SMOKE,
            LIVE_OPERATOR_CONFIG,
            LIVE_OPERATOR_SCHEDULED,
            LIVE_LOGS,
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: audit::targets::OPERATOR_ENTITY_FILTER,
        interaction: "Filter Operator entity lists",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG, LIVE_OPERATOR_SCHEDULED],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: audit::targets::OPERATOR_APPLY,
        interaction: "Persist edited Operator drafts",
        status: CoverageStatus::LiveInference,
        tests: &[
            LIVE_MULTI_AGENT_CONFIG,
            LIVE_OPERATOR_CONFIG,
            LIVE_OPERATOR_SCHEDULED,
            LIVE_LOGS,
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: audit::targets::OPERATOR_DISCARD,
        interaction: "Discard edited Operator drafts",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator",
        target: audit::targets::OPERATOR_RUN_NOW,
        interaction: "Run a scheduled task immediately",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_SCHEDULED],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Display Name",
        interaction: "Edit display names in Behavior, Tool Selection, and Inference Profile drafts",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG, LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.System Prompt",
        interaction: "Edit behavior system prompt",
        status: CoverageStatus::LiveInference,
        tests: &[LIVE_MULTI_AGENT_CONFIG, LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Compaction Threshold",
        interaction: "Edit behavior compaction threshold",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Name",
        interaction: "Edit backend and scheduled task names",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG, LIVE_OPERATOR_SCHEDULED],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Max Concurrent",
        interaction: "Edit backend concurrency",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Max Queue Depth",
        interaction: "Edit backend queue depth",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Probe Status",
        interaction: "Edit backend probe status",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.File Tools Mode",
        interaction: "Edit tool selection file mode",
        status: CoverageStatus::LiveInference,
        tests: &[LIVE_MULTI_AGENT_CONFIG, LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Bash Mode",
        interaction: "Edit tool selection bash mode",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.CLI Tool Names",
        interaction: "Edit tool selection CLI names",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Delegate To",
        interaction: "Edit tool selection delegates",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Max Turns",
        interaction: "Edit inference profile max turns",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Temperature",
        interaction: "Edit inference profile temperature",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Stream Batch Ms",
        interaction: "Edit inference profile stream batch window",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Interval Secs",
        interaction: "Edit scheduled task interval, including validation failure",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_SCHEDULED],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Prompt",
        interaction: "Edit scheduled task prompt",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_SCHEDULED],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Behavior ID",
        interaction: "Edit isolated behavior identity and scheduled-task behavior linkage",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_IDENTITIES],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Agent DID",
        interaction: "Edit isolated behavior agent DID linkage",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_IDENTITIES],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Backend ID",
        interaction: "Edit behavior backend linkage",
        status: CoverageStatus::LiveInference,
        tests: &[LIVE_MULTI_AGENT_CONFIG, LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Model Name",
        interaction: "Edit behavior model binding",
        status: CoverageStatus::LiveInference,
        tests: &[LIVE_MULTI_AGENT_CONFIG, LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Tool Selection ID",
        interaction: "Edit behavior tool-selection binding",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Inference Profile ID",
        interaction: "Edit behavior inference-profile binding",
        status: CoverageStatus::LiveInference,
        tests: &[LIVE_MULTI_AGENT_CONFIG, LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Compaction Strategy",
        interaction: "Edit behavior compaction strategy",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.API Key",
        interaction: "Edit backend inline API key with an isolated placeholder secret",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.API Key Env Var",
        interaction: "Edit backend API key env-var reference",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Selection ID",
        interaction: "Edit tool-selection identity",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_IDENTITIES],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Profile ID",
        interaction: "Edit inference-profile identity",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_IDENTITIES],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Task ID",
        interaction: "Edit scheduled task identity",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_IDENTITIES],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Next Run At",
        interaction: "Edit scheduled task next-run timestamp",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_SCHEDULED],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Context Window",
        interaction: "Edit inference profile context window",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Max Output Tokens",
        interaction: "Edit inference profile max output tokens",
        status: CoverageStatus::LiveInference,
        tests: &[LIVE_MULTI_AGENT_CONFIG, LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Deadline Duration Secs",
        interaction: "Edit inference profile deadline duration",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Provider Kind",
        interaction: "Edit backend provider kind",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Endpoint",
        interaction: "Edit backend endpoint",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.field.Models",
        interaction: "Edit backend model list",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.toggle.Enable File Tools",
        interaction: "Toggle file tools",
        status: CoverageStatus::LiveInference,
        tests: &[LIVE_MULTI_AGENT_CONFIG, LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.toggle.Enable Meta Tools",
        interaction: "Toggle meta tools",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.toggle.Enabled",
        interaction: "Toggle behavior or scheduled task enabled state",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG, LIVE_OPERATOR_SCHEDULED],
        next_step: "",
    },
    CoverageEntry {
        area: "Operator fields",
        target: "operator.toggle.Enable Bash",
        interaction: "Toggle bash tools",
        status: CoverageStatus::LivePersisted,
        tests: &[LIVE_OPERATOR_CONFIG],
        next_step: "",
    },
    CoverageEntry {
        area: "Logs",
        target: audit::targets::LOGS_FILTER_ALL,
        interaction: "Click All logs filter",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Logs",
        target: audit::targets::LOGS_FILTER_REPLICATION,
        interaction: "Filter replication logs",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_INFERENCE_SMOKE, LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Logs",
        target: audit::targets::LOGS_FILTER_PEERING,
        interaction: "Filter peering logs",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Logs",
        target: audit::targets::LOGS_FILTER_TURNS,
        interaction: "Filter turn logs",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Logs",
        target: audit::targets::LOGS_FILTER_WRITES,
        interaction: "Filter write logs",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Logs",
        target: audit::targets::LOGS_FILTER_WARNINGS,
        interaction: "Filter warning logs",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_INFERENCE_SMOKE, LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Peers",
        target: audit::targets::PEERS_ADD_LABEL,
        interaction: "Type peer label",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_INFERENCE_SMOKE, LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Peers",
        target: audit::targets::PEERS_ADD_ADDR,
        interaction: "Type peer address",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_INFERENCE_SMOKE, LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Peers",
        target: audit::targets::PEERS_ADD_AGENT_DID,
        interaction: "Type peer agent DID",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_INFERENCE_SMOKE, LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Peers",
        target: audit::targets::PEERS_SAVE,
        interaction: "Save a valid and invalid peer",
        status: CoverageStatus::LiveClick,
        tests: &[LIVE_INFERENCE_SMOKE, LIVE_LOGS],
        next_step: "",
    },
    CoverageEntry {
        area: "Peers",
        target: audit::targets::PEERS_REMOVE,
        interaction: "Remove a saved peer",
        status: CoverageStatus::LiveClick,
        tests: &[
            LIVE_INFERENCE_SMOKE,
            "desktop_app_clicks_through_peers_selection_toggle_clear_and_remove",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Peers",
        target: audit::targets::PEERS_TOGGLE_ADD_FORM,
        interaction: "Toggle peer add form",
        status: CoverageStatus::LiveClick,
        tests: &[
            LIVE_LOGS,
            "desktop_app_clicks_through_peers_selection_toggle_clear_and_remove",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Peers",
        target: audit::targets::PEERS_CLEAR,
        interaction: "Clear peer add form",
        status: CoverageStatus::LiveClick,
        tests: &[
            LIVE_INFERENCE_SMOKE,
            "desktop_app_clicks_through_peers_selection_toggle_clear_and_remove",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Peers",
        target: audit::targets::PEERS_MAIN_COPY_DID,
        interaction: "Copy desktop DID from main Peers view",
        status: CoverageStatus::LiveClick,
        tests: &[
            LIVE_INFERENCE_SMOKE,
            "desktop_app_clicks_through_peers_selection_toggle_clear_and_remove",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Peers",
        target: audit::targets::PEERS_REPAIR_NOW,
        interaction: "Queue an immediate P2P repair cycle",
        status: CoverageStatus::LiveClick,
        tests: &["desktop_app_peers_transport_actions_queue_repair_and_restart"],
        next_step: "",
    },
    CoverageEntry {
        area: "Peers",
        target: audit::targets::PEERS_RESTART_CLIENT,
        interaction: "Restart the desktop client core from the Peers view",
        status: CoverageStatus::LiveClick,
        tests: &[
            "desktop_app_peers_transport_actions_queue_repair_and_restart",
            "desktop_app_restart_client_rebinds_same_workspace",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Peers",
        target: audit::targets::PEERS_ONBOARDING_COPY_DID,
        interaction: "Copy desktop DID from onboarding",
        status: CoverageStatus::LiveClick,
        tests: &[
            LIVE_INFERENCE_SMOKE,
            "desktop_app_clicks_through_first_launch_add_peer_flow",
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Peers",
        target: "peers.peer.<record_id>",
        interaction: "Select a saved peer row",
        status: CoverageStatus::LiveClick,
        tests: &[
            "desktop_app_clicks_through_peers_selection_toggle_clear_and_remove",
            LIVE_INFERENCE_SMOKE,
        ],
        next_step: "",
    },
    CoverageEntry {
        area: "Peers",
        target: "peers.agent.<record_id>",
        interaction: "Select a nested agent row from the saved peer tree",
        status: CoverageStatus::LiveClick,
        tests: &[
            "desktop_app_clicks_through_peers_selection_toggle_clear_and_remove",
            LIVE_INFERENCE_SMOKE,
        ],
        next_step: "",
    },
];

#[test]
fn desktop_app_coverage_matrix_lists_every_static_audit_target() {
    let static_targets = [
        audit::targets::ACTIVITY_CHAT,
        audit::targets::ACTIVITY_OPERATOR,
        audit::targets::ACTIVITY_PEERS,
        audit::targets::ACTIVITY_LOGS,
        audit::targets::CHAT_COMPOSER_TEXT,
        audit::targets::CHAT_CREATE_CONVERSATION,
        audit::targets::CHAT_OPEN_PEERS_SETUP,
        audit::targets::CHAT_RETRY,
        audit::targets::CHAT_SEND,
        audit::targets::CHAT_EXPORT,
        audit::targets::LOGS_FILTER_ALL,
        audit::targets::LOGS_FILTER_REPLICATION,
        audit::targets::LOGS_FILTER_PEERING,
        audit::targets::LOGS_FILTER_TURNS,
        audit::targets::LOGS_FILTER_WRITES,
        audit::targets::LOGS_FILTER_WARNINGS,
        audit::targets::OPERATOR_APPLY,
        audit::targets::OPERATOR_DISCARD,
        audit::targets::OPERATOR_ENTITY_FILTER,
        audit::targets::OPERATOR_RUN_NOW,
        audit::targets::PEERS_ADD_ADDR,
        audit::targets::PEERS_ADD_AGENT_DID,
        audit::targets::PEERS_ADD_LABEL,
        audit::targets::PEERS_CLEAR,
        audit::targets::PEERS_MAIN_COPY_DID,
        audit::targets::PEERS_REPAIR_NOW,
        audit::targets::PEERS_REMOVE,
        audit::targets::PEERS_RESTART_CLIENT,
        audit::targets::PEERS_TOGGLE_ADD_FORM,
        audit::targets::PEERS_ONBOARDING_COPY_DID,
        audit::targets::PEERS_SAVE,
    ];

    for target in static_targets {
        assert!(
            COVERAGE_MATRIX.iter().any(|entry| entry.target == target),
            "missing static audit target in coverage matrix: {target}"
        );
    }
}

#[test]
fn desktop_app_coverage_matrix_has_unique_target_rows() {
    for (index, entry) in COVERAGE_MATRIX.iter().enumerate() {
        assert!(
            !entry.area.trim().is_empty(),
            "coverage row has empty area: {entry:?}"
        );
        assert!(
            !entry.target.trim().is_empty(),
            "coverage row has empty target: {entry:?}"
        );
        assert!(
            !entry.interaction.trim().is_empty(),
            "coverage row has empty interaction: {entry:?}"
        );
        match entry.status {
            CoverageStatus::SeededArtifact | CoverageStatus::Blocked => assert!(
                !entry.next_step.trim().is_empty(),
                "gap rows must name the live promotion path: {entry:?}"
            ),
            _ => assert!(
                entry.next_step.trim().is_empty(),
                "fully live rows should not carry stale next steps: {entry:?}"
            ),
        }

        for later in &COVERAGE_MATRIX[index + 1..] {
            assert_ne!(
                entry.target, later.target,
                "duplicate coverage target row: {}",
                entry.target
            );
        }
    }
}

#[test]
fn desktop_app_coverage_matrix_keeps_gaps_actionable() {
    let mut live_click_rows = 0;
    let mut live_persisted_rows = 0;
    let mut live_inference_rows = 0;
    let mut live_multi_agent_rows = 0;
    let mut seeded_artifact_rows = 0;
    let mut blocked_rows = 0;

    for entry in COVERAGE_MATRIX {
        match entry.status {
            CoverageStatus::LiveClick => {
                live_click_rows += 1;
                assert!(
                    entry
                        .tests
                        .iter()
                        .any(|test| test.starts_with("desktop_app_live_")),
                    "live click row must point at an ignored live journey: {entry:?}"
                );
            }
            CoverageStatus::LivePersisted => {
                live_persisted_rows += 1;
                assert!(
                    entry
                        .tests
                        .iter()
                        .any(|test| test.starts_with("desktop_app_live_")),
                    "live persistence row must point at an ignored live journey: {entry:?}"
                );
            }
            CoverageStatus::LiveInference => {
                live_inference_rows += 1;
                assert!(
                    entry
                        .tests
                        .iter()
                        .any(|test| test.starts_with("desktop_app_live_")),
                    "live inference row must point at an ignored live journey: {entry:?}"
                );
            }
            CoverageStatus::LiveMultiAgent => {
                live_multi_agent_rows += 1;
                assert!(
                    entry.tests.contains(&LIVE_MULTI_AGENT_CONFIG),
                    "multi-agent row must point at the multi-agent live journey: {entry:?}"
                );
            }
            CoverageStatus::SeededArtifact => {
                seeded_artifact_rows += 1;
                assert!(
                    !entry.next_step.trim().is_empty(),
                    "seeded artifact row must keep a concrete promotion step: {entry:?}"
                );
            }
            CoverageStatus::Blocked => {
                blocked_rows += 1;
                assert!(
                    !entry.next_step.trim().is_empty(),
                    "blocked row must keep a concrete unblock step: {entry:?}"
                );
            }
        }
    }

    assert!(
        live_click_rows > 0,
        "coverage matrix should track live click rows"
    );
    assert!(
        live_persisted_rows > 0,
        "coverage matrix should track live persistence rows"
    );
    assert!(
        live_inference_rows > 0,
        "coverage matrix should track live inference rows"
    );
    assert!(
        live_multi_agent_rows > 0,
        "coverage matrix should track live multi-agent rows"
    );
    assert!(
        seeded_artifact_rows > 0,
        "coverage matrix should track seeded artifact rows"
    );
    assert!(
        blocked_rows > 0,
        "coverage matrix should track blocked harness rows"
    );
}
