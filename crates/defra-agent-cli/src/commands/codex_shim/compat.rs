use anyhow::Result;
use codex_app_server_protocol as codex;

mod empty_local;

use empty_local::send_empty_local_stub;

use super::protocol::send_error;
use super::{trace, Outbound, ShimState, JSONRPC_METHOD_NOT_FOUND};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompatDifficulty {
    LocalStateProjection,
    DefraBackedWorkflow,
    HostRuntimeIntegration,
    ExternalOrLargeFeature,
}

impl CompatDifficulty {
    fn label(self) -> &'static str {
        match self {
            Self::LocalStateProjection => "medium: local Codex state projection",
            Self::DefraBackedWorkflow => "medium-hard: DEFRA-backed workflow",
            Self::HostRuntimeIntegration => "hard: host runtime integration",
            Self::ExternalOrLargeFeature => "hard: external or large Codex feature",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CompatGap {
    pub(super) difficulty: CompatDifficulty,
    pub(super) area: &'static str,
    pub(super) plan: &'static str,
}

impl CompatGap {
    fn message(self, method: &str) -> String {
        format!(
            "unsupported Codex shim method `{method}` ({difficulty}; area: {area}). Plan: {plan}",
            difficulty = self.difficulty.label(),
            area = self.area,
            plan = self.plan
        )
    }
}

pub(super) async fn send_planned_stub(
    outbound: &Outbound,
    state: &ShimState,
    request: codex::ClientRequest,
) -> Result<()> {
    if send_empty_local_stub(outbound, state, &request).await? {
        return Ok(());
    }

    let request_id = request.id().clone();
    let method = request.method();
    let gap = compat_gap_for_request(&request).unwrap_or(CompatGap {
        difficulty: CompatDifficulty::ExternalOrLargeFeature,
        area: "uncategorized",
        plan: "classify this new Codex protocol method before implementing behavior",
    });
    trace::shim_event(
        &state.trace_path,
        format!(
            "compat_gap {request_id} {method} {} {}",
            gap.difficulty.label(),
            gap.area
        ),
    );
    tracing::warn!(
        %request_id,
        %method,
        difficulty = gap.difficulty.label(),
        area = gap.area,
        plan = gap.plan,
        "Codex shim compatibility gap"
    );
    send_error(
        outbound,
        request_id,
        JSONRPC_METHOD_NOT_FOUND,
        gap.message(&method),
    )
    .await
}

pub(super) fn compat_gap_for_request(request: &codex::ClientRequest) -> Option<CompatGap> {
    match request {
        codex::ClientRequest::Initialize { .. }
        | codex::ClientRequest::GetAccount { .. }
        | codex::ClientRequest::GetAccountRateLimits { .. }
        | codex::ClientRequest::ModelList { .. }
        | codex::ClientRequest::ModelProviderCapabilitiesRead { .. }
        | codex::ClientRequest::ConfigRead { .. }
        | codex::ClientRequest::ConfigBatchWrite { .. }
        | codex::ClientRequest::ConfigValueWrite { .. }
        | codex::ClientRequest::ConfigRequirementsRead { .. }
        | codex::ClientRequest::ExternalAgentConfigDetect { .. }
        | codex::ClientRequest::ExternalAgentConfigImport { .. }
        | codex::ClientRequest::ExperimentalFeatureList { .. }
        | codex::ClientRequest::PermissionProfileList { .. }
        | codex::ClientRequest::CollaborationModeList { .. }
        | codex::ClientRequest::SkillsList { .. }
        | codex::ClientRequest::HooksList { .. }
        | codex::ClientRequest::PluginList { .. }
        | codex::ClientRequest::McpServerStatusList { .. }
        | codex::ClientRequest::ThreadStart { .. }
        | codex::ClientRequest::ThreadResume { .. }
        | codex::ClientRequest::ThreadList { .. }
        | codex::ClientRequest::ThreadLoadedList { .. }
        | codex::ClientRequest::ThreadRead { .. }
        | codex::ClientRequest::ThreadUnsubscribe { .. }
        | codex::ClientRequest::TurnStart { .. }
        | codex::ClientRequest::TurnSteer { .. }
        | codex::ClientRequest::TurnInterrupt { .. }
        | codex::ClientRequest::ThreadBackgroundTerminalsClean { .. }
        | codex::ClientRequest::ThreadArchive { .. }
        | codex::ClientRequest::ThreadUnarchive { .. }
        | codex::ClientRequest::ThreadIncrementElicitation { .. }
        | codex::ClientRequest::ThreadDecrementElicitation { .. }
        | codex::ClientRequest::ThreadSetName { .. }
        | codex::ClientRequest::ThreadMetadataUpdate { .. }
        | codex::ClientRequest::ThreadMemoryModeSet { .. }
        | codex::ClientRequest::ThreadGoalSet { .. }
        | codex::ClientRequest::ThreadGoalGet { .. }
        | codex::ClientRequest::ThreadGoalClear { .. }
        | codex::ClientRequest::ThreadSettingsUpdate { .. }
        | codex::ClientRequest::MemoryReset { .. }
        | codex::ClientRequest::ThreadApproveGuardianDeniedAction { .. }
        | codex::ClientRequest::MarketplaceAdd { .. }
        | codex::ClientRequest::MarketplaceRemove { .. }
        | codex::ClientRequest::MarketplaceUpgrade { .. }
        | codex::ClientRequest::PluginInstalled { .. }
        | codex::ClientRequest::PluginShareSave { .. }
        | codex::ClientRequest::PluginShareUpdateTargets { .. }
        | codex::ClientRequest::PluginShareList { .. }
        | codex::ClientRequest::PluginShareCheckout { .. }
        | codex::ClientRequest::PluginShareDelete { .. }
        | codex::ClientRequest::AppsList { .. }
        | codex::ClientRequest::SkillsConfigWrite { .. }
        | codex::ClientRequest::ExperimentalFeatureEnablementSet { .. }
        | codex::ClientRequest::RemoteControlStatusRead { .. }
        | codex::ClientRequest::MockExperimentalMethod { .. }
        | codex::ClientRequest::WindowsSandboxReadiness { .. }
        | codex::ClientRequest::LoginAccount { .. }
        | codex::ClientRequest::CancelLoginAccount { .. }
        | codex::ClientRequest::LogoutAccount { .. }
        | codex::ClientRequest::SendAddCreditsNudgeEmail { .. }
        | codex::ClientRequest::FeedbackUpload { .. }
        | codex::ClientRequest::GetAuthStatus { .. } => None,

        codex::ClientRequest::ThreadFork { .. }
        | codex::ClientRequest::ThreadRollback { .. }
        | codex::ClientRequest::ThreadSearch { .. }
        | codex::ClientRequest::ThreadTurnsList { .. }
        | codex::ClientRequest::ThreadTurnsItemsList { .. }
        | codex::ClientRequest::GetConversationSummary { .. } => Some(CompatGap {
            difficulty: CompatDifficulty::LocalStateProjection,
            area: "thread/session projection",
            plan: "persist enough Codex thread and turn view state to replay, resume, fork, rename, and list turns",
        }),

        codex::ClientRequest::ThreadCompactStart { .. }
        | codex::ClientRequest::ThreadInjectItems { .. }
        | codex::ClientRequest::ReviewStart { .. } => Some(CompatGap {
            difficulty: CompatDifficulty::DefraBackedWorkflow,
            area: "turn workflow",
            plan: "map the Codex workflow onto DEFRA requests, turn metadata, and streamed turn notifications",
        }),

        codex::ClientRequest::ThreadShellCommand { .. }
        | codex::ClientRequest::FsReadFile { .. }
        | codex::ClientRequest::FsWriteFile { .. }
        | codex::ClientRequest::FsCreateDirectory { .. }
        | codex::ClientRequest::FsGetMetadata { .. }
        | codex::ClientRequest::FsReadDirectory { .. }
        | codex::ClientRequest::FsRemove { .. }
        | codex::ClientRequest::FsCopy { .. }
        | codex::ClientRequest::FsWatch { .. }
        | codex::ClientRequest::FsUnwatch { .. }
        | codex::ClientRequest::OneOffCommandExec { .. }
        | codex::ClientRequest::CommandExecWrite { .. }
        | codex::ClientRequest::CommandExecTerminate { .. }
        | codex::ClientRequest::CommandExecResize { .. }
        | codex::ClientRequest::ProcessSpawn { .. }
        | codex::ClientRequest::ProcessWriteStdin { .. }
        | codex::ClientRequest::ProcessKill { .. }
        | codex::ClientRequest::ProcessResizePty { .. }
        | codex::ClientRequest::GitDiffToRemote { .. }
        | codex::ClientRequest::FuzzyFileSearch { .. }
        | codex::ClientRequest::FuzzyFileSearchSessionStart { .. }
        | codex::ClientRequest::FuzzyFileSearchSessionUpdate { .. }
        | codex::ClientRequest::FuzzyFileSearchSessionStop { .. } => Some(CompatGap {
            difficulty: CompatDifficulty::HostRuntimeIntegration,
            area: "host filesystem/process runtime",
            plan: "reuse native filesystem and managed exec primitives while preserving Codex item and terminal notifications",
        }),

        codex::ClientRequest::PluginRead { .. }
        | codex::ClientRequest::PluginSkillRead { .. }
        | codex::ClientRequest::PluginInstall { .. }
        | codex::ClientRequest::PluginUninstall { .. }
        | codex::ClientRequest::ThreadRealtimeStart { .. }
        | codex::ClientRequest::ThreadRealtimeAppendAudio { .. }
        | codex::ClientRequest::ThreadRealtimeAppendText { .. }
        | codex::ClientRequest::ThreadRealtimeStop { .. }
        | codex::ClientRequest::ThreadRealtimeListVoices { .. }
        | codex::ClientRequest::RemoteControlEnable { .. }
        | codex::ClientRequest::RemoteControlDisable { .. }
        | codex::ClientRequest::EnvironmentAdd { .. }
        | codex::ClientRequest::McpServerOauthLogin { .. }
        | codex::ClientRequest::McpServerRefresh { .. }
        | codex::ClientRequest::McpResourceRead { .. }
        | codex::ClientRequest::McpServerToolCall { .. }
        | codex::ClientRequest::WindowsSandboxSetupStart { .. } => Some(CompatGap {
            difficulty: CompatDifficulty::ExternalOrLargeFeature,
            area: "extended Codex feature",
            plan: "decide whether DEFRA should emulate this protocol path or explicitly advertise it as unavailable",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn difficulty_labels_are_stable() {
        assert_eq!(
            CompatDifficulty::LocalStateProjection.label(),
            "medium: local Codex state projection"
        );
        assert_eq!(
            CompatDifficulty::DefraBackedWorkflow.label(),
            "medium-hard: DEFRA-backed workflow"
        );
        assert_eq!(
            CompatDifficulty::HostRuntimeIntegration.label(),
            "hard: host runtime integration"
        );
        assert_eq!(
            CompatDifficulty::ExternalOrLargeFeature.label(),
            "hard: external or large Codex feature"
        );
    }
}
