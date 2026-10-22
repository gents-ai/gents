use std::path::{Path, PathBuf};

mod commands;

use gents_desktop_core::local_runtime::DesktopInitSummary;
use ts_rs::TS;

use crate::error::{BridgeError, BridgeErrorCode};
use crate::inference_setup::InferenceSetupState;
use crate::tauri_commands::chat::RequestResendResultView;
use crate::tauri_commands::inference_setup::{
    BackendUsageView, ClaudeLoginRequest, ClaudeLoginResult, ClaudeLoginUrl, CodexLoginRequest,
    CodexLoginResult, CodexLoginUrl, GrokLoginRequest, GrokLoginResult, GrokLoginUrl,
    InferenceBackendRecommendationRequest, InferenceDiscoveryFailure, InferenceDiscoveryRequest,
    InferenceDiscoveryResult, InferenceProbeRequest, InferenceProbeResult,
    InferenceRecommendationRequest, ProviderAccountDisconnectRequest, ProviderAccountRemoveRequest,
    ProviderAccountRenameRequest, ProviderAccountRetrySaveRequest, ProviderAccountView,
    ProviderAccountsRequest, ProviderUsageReadRequest, SignInView, UsageWindowView,
};
use crate::tauri_commands::lifecycle::DesktopObserverMetrics;
use crate::tauri_commands::workspace::WorkspaceListingView;
use crate::types::*;

fn bindings_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/gents-desktop-client/src/generated")
}

fn replace_typescript_identifier(source: &str, from: &str, to: &str) -> String {
    fn is_identifier_char(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
    }

    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find(from) {
        let start = cursor + offset;
        let end = start + from.len();
        let left_is_identifier = start > 0 && is_identifier_char(bytes[start - 1]);
        let right_is_identifier = end < bytes.len() && is_identifier_char(bytes[end]);
        output.push_str(&source[cursor..start]);
        if left_is_identifier || right_is_identifier {
            output.push_str(from);
        } else {
            output.push_str(to);
        }
        cursor = end;
    }
    output.push_str(&source[cursor..]);
    output
}

fn normalize_generated_types(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            normalize_generated_types(&path)?;
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("ts") {
            continue;
        }
        let source = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let normalized = replace_typescript_identifier(&source, "bigint", "number")
            .lines()
            .map(|line| {
                let trimmed = line.trim_end();
                if trimmed.starts_with("import type ")
                    && trimmed.ends_with("\";")
                    && !trimmed.ends_with(".js\";")
                {
                    format!("{}.js\";", trimmed.trim_end_matches("\";"))
                } else {
                    trimmed.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(path, normalized).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn bridge_visible_derived_types(source: &str, derive_marker: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut derive = String::new();
    let mut collecting_derive = false;
    let mut pending_match = false;

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[derive(") {
            derive.clear();
            collecting_derive = true;
        }
        if collecting_derive {
            derive.push_str(trimmed);
            if trimmed.ends_with(")]") {
                collecting_derive = false;
                pending_match = derive
                    .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                    .any(|token| token == derive_marker);
            }
            continue;
        }
        if !pending_match {
            continue;
        }
        if trimmed.is_empty()
            || trimmed.starts_with("///")
            || trimmed.starts_with("#[")
            || trimmed.starts_with("//")
        {
            continue;
        }

        let declaration = trimmed
            .strip_prefix("pub struct ")
            .or_else(|| trimmed.strip_prefix("pub enum "))
            .or_else(|| trimmed.strip_prefix("pub(crate) struct "))
            .or_else(|| trimmed.strip_prefix("pub(crate) enum "));
        if let Some(declaration) = declaration {
            let name = declaration
                .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .next()
                .unwrap_or_default();
            if !name.is_empty() {
                names.push(name.to_string());
            }
        }
        pending_match = false;
    }

    names
}

fn contract_source_paths() -> Vec<PathBuf> {
    fn collect_rust_files(directory: &Path, output: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).expect("read contract source directory") {
            let path = entry.expect("contract source entry").path();
            if path.is_dir() {
                collect_rust_files(&path, output);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                output.push(path);
            }
        }
    }

    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut paths = vec![
        source_root.join("contract.rs"),
        source_root.join("host/protocol.rs"),
        source_root.join("inference_setup.rs"),
        source_root.join("../../gents-desktop-core/src/error.rs"),
        source_root.join("../../gents-desktop-core/src/home_reset.rs"),
        source_root.join("tauri_commands/chat.rs"),
        source_root.join("tauri_commands/inference_setup.rs"),
        source_root.join("tauri_commands/lifecycle.rs"),
        source_root.join("tauri_commands/workspace.rs"),
        source_root.join("tauri_commands/packs.rs"),
        source_root.join("tauri_commands/allowed_dirs.rs"),
        source_root.join("tauri_commands/e2e.rs"),
    ];
    collect_rust_files(&source_root.join("types"), &mut paths);
    paths.sort();
    paths
}

fn export_all(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;

    macro_rules! export_types {
        ($($type:ty),+ $(,)?) => {
            $(
                <$type>::export_all_to(dir).map_err(|e| e.to_string())?;
            )+
        };
    }

    export_types!(
        crate::accounts::BackendAuthenticationView,
        ConfiguredActionView,
        crate::host::settings::ConfigEditorEntry,
        gents_desktop_core::client::ConfigEditorDocument,
        RequestOutputView,
        gents_protocol::file_changes::ToolFileChanges,
        gents_protocol::client_media::MediaCall,
        gents_protocol::client_media::MediaOperation,
        gents_protocol::client_media::MediaPcmClip,
        gents_protocol::client_media::MediaMessage,
        gents_protocol::client_media::MediaDevices,
        crate::host::speech::SpeechServiceView,
        crate::host::speech::SpeechToolReference,
        crate::host::speech::SpeechRecognizeRequest,
        crate::host::speech::SpeechSynthesizeRequest,
        crate::host::permissions::WorkspacePermissionsRequest,
        crate::host::permissions::WorkspacePermissionsSaveRequest,
        crate::host::permissions::WorkspacePermissionsState,
        crate::host::protocol::ClientMessage,
        crate::host::protocol::ClientCommand,
        crate::host::protocol::HostMessage,
        DesktopInitRequest,
        ManagedServerStartRequest,
        ManagedServerRestartRequest,
        ManagedServerResetRequest,
        ManagedServerRootValidationRequest,
        PeerStatusFetchRequest,
        EnrollmentStatusRequest,
        ChatSendRequest,
        PendingMessageEditRequest,
        PendingQueueEditRequest,
        PendingMessageSteerRequest,
        PendingQueueEditResult,
        SessionGoalView,
        GoalControlView,
        GoalControlAction,
        GoalControlRequest,
        GoalControlResult,
        PendingQueueEntryView,
        MailboxItemRequest,
        gents_protocol::mailbox_question::MailboxQuestion,
        SessionForkBeforeRequest,
        SessionForkResult,
        SessionRenameRequest,
        NodeConfigSaveRequest,
        DefaultAgentSetRequest,
        ConfigComponentsApplyRequest,
        ConfigComponentsPatchRequest,
        ConfigComponentPatch,
        AgentSaveRequest,
        SkillDeleteRequest,
        TaskDeleteRequest,
        ScheduleDeleteRequest,
        TriggerDeleteRequest,
        EventSourceDeleteRequest,
        BackendDeleteRequest,
        InferenceProfileDeleteRequest,
        ToolsDeleteRequest,
        ToolServiceDeleteRequest,
        AgentDeleteRequest,
        ContextDeleteRequest,
        BackendSaveRequest,
        InferenceProfileSaveRequest,
        ToolsSaveRequest,
        ToolServiceSaveRequest,
        ToolServiceTestRequest,
        TaskSaveRequest,
        SkillSaveRequest,
        TaskRunRequest,
        ScheduleSaveRequest,
        ScheduleRunRequest,
        TriggerSaveRequest,
        EventSourceSaveRequest,
        DesktopOperationsSnapshotRequest,
        DesktopSessionProvenanceRequest,
        DesktopInterruptRequest,
        DesktopBackgroundToolRequest,
        DesktopJobControlRequest,
        DesktopProbeMcpServiceRequest,
        InferenceProbeRequest,
        InferenceDiscoveryRequest,
        InferenceSetupState,
        AgentModelSelectionRequest,
        InferenceRecommendationRequest,
        InferenceBackendRecommendationRequest,
        CodexLoginRequest,
        GrokLoginRequest,
        ClaudeLoginRequest,
        ProviderAccountsRequest,
        ProviderAccountDisconnectRequest,
        ProviderAccountRetrySaveRequest,
        ProviderAccountRenameRequest,
        ProviderAccountRemoveRequest,
        ProviderUsageReadRequest,
    );

    export_types!(
        BridgeErrorCode,
        BridgeError,
        DesktopClientSnapshot,
        DesktopInitSummary,
        ManagedServerState,
        ManagedServerStatus,
        ManagedServerRootValidation,
        ManagedServerResetResult,
        PeerRemoveResponse,
        EnrollmentRequestView,
        NetworkStatusView,
        ToolServiceTestResult,
        TaskRunResult,
        ChatSendResult,
        ClientUpdateEvent,
        SessionHydrationView,
        SyncHealthView,
        PairingCollectionStatusView,
        DesktopOperationsSnapshot,
        SessionProvenanceView,
        InterruptRequestResult,
        BackendHealthView,
        MCPServiceHealthView,
        McpServiceProbeResult,
        DerivedCancelCauseView,
        DesktopSessionSnapshot,
        SessionLiveDeltaView,
        SessionRowsView,
        SessionRowsWindow,
        MessageView,
        ToolCallView,
        RequestResendResultView,
        WorkspaceListingView,
        DesktopObserverMetrics,
        InferenceProbeResult,
        InferenceDiscoveryFailure,
        InferenceDiscoveryResult,
        gents::inference_setup::InferenceSetupCatalog,
        gents::inference_setup::InferenceProviderOption,
        gents::inference_setup::InferenceAuthOption,
        gents::inference_setup::InferenceProviderId,
        gents::inference_setup::InferenceAuthMethod,
        gents::inference_setup::InferenceModelOption,
        gents::inference_setup::InferenceModelRecommendation,
        gents::inference_setup::RecommendedNumberControl,
        gents::inference_setup::RecommendedIntegerControl,
        gents::inference_setup::RecommendedReasoningControl,
        CodexLoginResult,
        CodexLoginUrl,
        GrokLoginResult,
        GrokLoginUrl,
        ClaudeLoginResult,
        ClaudeLoginUrl,
        ProviderAccountView,
        crate::tauri_commands::e2e::NativeE2eConfig,
        crate::tauri_commands::e2e::NativeE2eStatus,
        crate::tauri_commands::packs::PackInstallRequest,
        crate::tauri_commands::ux_plugins::PluginCallRequest,
        crate::tauri_commands::ux_plugins::UxDataCallRequest,
        crate::tauri_commands::ux_plugins::UxDataSurfacesRequest,
        crate::tauri_commands::packs::PluginBindRequest,
        crate::tauri_commands::packs::PackWireRequest,
        gents_server::packs::EditedDocuments,
        RequestTimelineView,
        ToolSurfaceExplanationView,
        BackendDisplayState,
        InstalledPacksView,
        PackSearchView,
        PackInfoView,
        PackInstallView,
        PackUpdateView,
        PackRemoveView,
        PackLoginView,
        PackLogoutView,
        PackAccountView,
        PackPluginSlotsView,
        PackPluginBindingView,
        crate::tauri_commands::allowed_dirs::AllowedFoldersView,
        crate::tauri_commands::allowed_dirs::PluginApprovalsView,
        SignInView,
        BackendUsageView,
        UsageWindowView,
        gents_protocol::job::JobProgress,
        gents_protocol::job::CatalogEntry,
        gents_protocol::job::JobView,
        gents_protocol::job::JobControlKind,
        gents_protocol::job::JobControlOutcome,
        gents_protocol::job::JobReadError,
        gents_protocol::job::JobState,
        gents_protocol::job::JobChildState,
        gents_protocol::job::JobPauseReason,
        gents_protocol::job::JobPhaseState,
        gents_protocol::job::JobOrigin,
        gents_protocol::job::JobPhase,
        gents_protocol::job::JobActiveChild,
        gents_protocol::job::JobBudget,
        gents_protocol::job::JobScope,
        gents_protocol::job::JobOutcome,
        gents_protocol::job::JobPinSummary,
        gents_protocol::job::JobChildRow,
        gents_protocol::job::JobUsage,
        gents_protocol::job::CatalogSource,
    );

    commands::export(dir)?;
    let protocol = format!(
        "// Generated from gents-desktop-bridge::host::protocol. Do not edit.\n\nexport const CLIENT_HOST_VERSION = {};\nexport const EDITOR_STORE_BOOTSTRAP_VERSION = {};\nexport const CLIENT_HOST_MAX_FRAME_BYTES = {};\nexport const CLIENT_HOST_MAX_PENDING_CALLS = {};\nexport const CLIENT_HOST_COMMANDS = {} as const;\n",
        crate::host::protocol::PROTOCOL_VERSION,
        crate::host::protocol::EDITOR_STORE_BOOTSTRAP_VERSION,
        crate::host::protocol::MAX_FRAME_BYTES,
        crate::host::protocol::MAX_PENDING_CALLS,
        serde_json::to_string(crate::host::protocol::ClientCommand::NAMES)
            .map_err(|error| error.to_string())?,
    );
    std::fs::write(dir.join("ClientHostProtocol.ts"), protocol)
        .map_err(|error| error.to_string())?;
    let media = format!(
        "// Generated from gents-protocol::client_media. Do not edit.\n\nexport const MEDIA_HOST_VERSION = {};\nexport const MEDIA_HOST_MAX_FRAME_BYTES = {};\nexport const MEDIA_HOST_MAX_PCM_BYTES = {};\nexport const MEDIA_HOST_MAX_SECONDS = {};\n",
        gents_protocol::client_media::VERSION,
        gents_protocol::client_media::MAX_FRAME_BYTES,
        gents_protocol::client_media::MAX_PCM_BYTES,
        gents_protocol::client_media::MAX_SECONDS,
    );
    std::fs::write(dir.join("MediaHostProtocol.ts"), media).map_err(|error| error.to_string())?;
    normalize_generated_types(dir)?;
    Ok(())
}

fn list_ts_files(dir: &Path) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    if !dir.exists() {
        return Ok(names);
    }
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            for nested in list_ts_files(&path)? {
                names.push(format!(
                    "{}/{nested}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        } else if path.extension().and_then(|s| s.to_str()) == Some("ts") {
            names.push(path.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

#[test]
fn ts_rs_exports_tagged_enum_and_camel_case_structs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    export_all(tmp.path()).expect("export");
    let goal = std::fs::read_to_string(tmp.path().join("GoalControlAction.ts"))
        .expect("GoalControlAction.ts");
    assert!(
        goal.contains("\"kind\": \"pause\"") && goal.contains("tokenBudget: string"),
        "goal controls must retain their tagged request shape and exact budget strings: {goal}"
    );

    let timeline = std::fs::read_to_string(tmp.path().join("RenderedTimelineItem.ts"))
        .expect("RenderedTimelineItem.ts");
    assert!(
        timeline.contains("kind") && timeline.contains("userMessage"),
        "tagged enum should surface the serde tag + camelCase variant; got:\n{timeline}"
    );
    assert!(
        timeline.contains("itemKey"),
        "per-variant rename_all should camelCase enum fields; got:\n{timeline}"
    );

    let client_message =
        std::fs::read_to_string(tmp.path().join("ClientMessage.ts")).expect("ClientMessage.ts");
    assert!(
        client_message.contains("\"kind\": \"hello\"")
            && client_message.contains("\"kind\": \"call\""),
        "{client_message}"
    );
    let client_command =
        std::fs::read_to_string(tmp.path().join("ClientCommand.ts")).expect("ClientCommand.ts");
    assert!(
        client_command.contains("\"name\": \"desktop_session_snapshot\"")
            && client_command.contains("\"args\":"),
        "{client_command}"
    );
    let session_args = std::fs::read_to_string(tmp.path().join("SessionSnapshotArgs.ts"))
        .expect("SessionSnapshotArgs.ts");
    assert!(
        session_args.contains("nodeDid:") && session_args.contains("sessionId:"),
        "{session_args}"
    );

    let bootstrap = std::fs::read_to_string(tmp.path().join("DesktopBootstrapSummary.ts"))
        .expect("DesktopBootstrapSummary.ts");
    assert!(
        bootstrap.contains("defaultNodeHome") && bootstrap.contains("clientStateExists"),
        "serde rename_all camelCase should be reflected; got:\n{bootstrap}"
    );
    assert!(
        bootstrap.contains("initConfigStatus?: InitConfigStatus | null"),
        "optional nullable bootstrap status should match the wire shape; got:\n{bootstrap}"
    );
    let runtime = std::fs::read_to_string(tmp.path().join("DesktopRuntimeSnapshot.ts"))
        .expect("DesktopRuntimeSnapshot.ts");
    assert!(
        runtime.contains("operatorConfigAuthoritativeNodeDids?: Array<string>"),
        "operator-config authority is optional and absent when unavailable; got:\n{runtime}"
    );
    let error = std::fs::read_to_string(tmp.path().join("BridgeError.ts")).expect("BridgeError.ts");
    assert!(
        error.contains("retryable"),
        "BridgeError fields; got:\n{error}"
    );

    assert!(
        !timeline.contains("bigint"),
        "serde IPC integers must be emitted as JavaScript numbers; got:\n{timeline}"
    );
}

#[test]
fn all_bridge_visible_contract_roots_are_generated() {
    let tmp = tempfile::tempdir().expect("tempdir");
    export_all(tmp.path()).expect("export");

    let files = list_ts_files(tmp.path())
        .expect("list generated")
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let mut wire_types = std::collections::BTreeSet::new();
    let mut typescript_types = std::collections::BTreeSet::new();
    for path in contract_source_paths() {
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        wire_types.extend(bridge_visible_derived_types(&source, "Serialize"));
        wire_types.extend(bridge_visible_derived_types(&source, "Deserialize"));
        typescript_types.extend(bridge_visible_derived_types(&source, "TS"));
    }
    assert!(
        wire_types.is_subset(&typescript_types),
        "bridge-visible serialized/deserialized types without TS derive: {:?}",
        wire_types.difference(&typescript_types).collect::<Vec<_>>()
    );
    for expected in typescript_types.iter().map(|name| format!("{name}.ts")) {
        assert!(
            files.contains(&expected),
            "missing generated bridge contract {expected}"
        );
    }

    for inference_wire_type in [
        "InferenceProbeRequest.ts",
        "InferenceProbeResult.ts",
        "CodexLoginRequest.ts",
        "CodexLoginResult.ts",
        "CodexLoginUrl.ts",
        "GrokLoginRequest.ts",
        "GrokLoginResult.ts",
        "GrokLoginUrl.ts",
        "ClaudeLoginRequest.ts",
        "ClaudeLoginResult.ts",
        "ClaudeLoginUrl.ts",
        "ProviderAccountRetrySaveRequest.ts",
    ] {
        assert!(
            files.contains(inference_wire_type),
            "inference onboarding wire type {inference_wire_type} is not generated"
        );
    }
}

#[test]
fn bigint_normalization_only_rewrites_typescript_identifier_tokens() {
    let source = "type Wire = bigint | Array<bigint>; type bigintCounter = string;\n";
    assert_eq!(
        replace_typescript_identifier(source, "bigint", "number"),
        "type Wire = number | Array<number>; type bigintCounter = string;\n"
    );
}

#[test]
fn committed_bindings_match_regeneration() {
    let tmp = tempfile::tempdir().expect("tempdir");
    export_all(tmp.path()).expect("export");

    let expected_files = list_ts_files(tmp.path()).expect("list generated");
    let committed = bindings_dir();
    let actual_files = list_ts_files(&committed).unwrap_or_default();
    assert_eq!(
        actual_files,
        expected_files,
        "bindings file set drifted under {}. Regenerate with:\n  cargo test -p gents-desktop-bridge write_bindings -- --ignored",
        committed.display()
    );

    for name in &expected_files {
        let expected = std::fs::read_to_string(tmp.path().join(name)).expect("read generated");
        let actual = std::fs::read_to_string(committed.join(name)).unwrap_or_else(|_| {
            panic!("missing committed binding {name}; regenerate with write_bindings")
        });
        assert_eq!(
            actual,
            expected,
            "binding {name} drifted under {}. Regenerate with:\n  cargo test -p gents-desktop-bridge write_bindings -- --ignored",
            committed.display()
        );
    }
}

#[test]
#[ignore = "run explicitly to regenerate packages/gents-desktop-client/src/generated/"]
fn write_bindings() {
    let temporary = tempfile::tempdir().expect("temporary bindings");
    export_all(temporary.path()).expect("export bindings");
    let expected = list_ts_files(temporary.path()).expect("generated bindings");
    let dir = bindings_dir();
    for obsolete in list_ts_files(&dir).expect("committed bindings") {
        if !expected.contains(&obsolete) {
            std::fs::remove_file(dir.join(obsolete)).expect("remove obsolete binding");
        }
    }
    for name in expected {
        let path = dir.join(&name);
        std::fs::create_dir_all(path.parent().unwrap()).expect("binding directory");
        let source = std::fs::read(temporary.path().join(name)).expect("generated binding");
        if std::fs::read(&path).ok().as_deref() != Some(source.as_slice()) {
            std::fs::write(path, source).expect("write binding");
        }
    }
    eprintln!("wrote bindings to {}", dir.display());
}

#[test]
fn media_commands_preserve_the_native_tag_in_generated_types() {
    use gents_protocol::client_media::{MediaCall, MediaOperation};
    let call: MediaCall = serde_json::from_value(serde_json::json!({
        "id":1,"operation":{"command":"hello","version":1},
    }))
    .unwrap();
    assert!(matches!(
        call.operation,
        MediaOperation::Hello { version: 1 }
    ));
    assert!(MediaOperation::decl().contains("\"command\": \"hello\""));
    assert!(MediaOperation::decl().contains("\"command\": \"capture\""));
}

#[test]
fn eval_capture_types_preserve_native_wire_tags() {
    use gents::document_config::EvalCapture;
    let declaration = EvalCapture::decl();
    for capture in [
        EvalCapture::Schema {
            name: "schema".into(),
            collections: vec!["Part".into()],
        },
        EvalCapture::Documents {
            name: "rows".into(),
            collection: "Part".into(),
            filter: serde_json::json!({}),
            fields: vec![],
        },
        EvalCapture::File {
            name: "files".into(),
            glob: "*.json".into(),
        },
    ] {
        let wire = serde_json::to_value(capture).unwrap();
        let tag = format!("\"kind\": \"{}\"", wire["kind"].as_str().unwrap());
        assert!(declaration.contains(&tag), "{declaration}");
    }
}

#[test]
fn canonical_config_requests_preserve_compact_authoring_and_auth_wire_tags() {
    let dir = tempfile::tempdir().expect("generated directory");
    gents::mailbox::MailboxNotificationPolicy::export_all_to(dir.path())
        .expect("notification export");
    let notification = std::fs::read_to_string(dir.path().join("NotificationIdentity.ts")).unwrap();
    assert!(
        notification.contains("\"mode\": \"event\""),
        "{notification}"
    );
    assert!(
        notification.contains("\"mode\": \"condition\""),
        "{notification}"
    );
    BackendSaveRequest::export_all_to(dir.path()).expect("backend export");
    ToolsSaveRequest::export_all_to(dir.path()).expect("tools export");
    ToolServiceSaveRequest::export_all_to(dir.path()).expect("service export");
    AgentSaveRequest::export_all_to(dir.path()).expect("agent export");
    InferenceProfileSaveRequest::export_all_to(dir.path()).expect("profile export");
    normalize_generated_types(dir.path()).expect("wire number normalization");
    let agent = std::fs::read_to_string(dir.path().join("Agent.ts")).unwrap();
    assert!(agent.contains("inference_profile_id: string"));
    assert!(!agent.contains("inference_profile_id?:"));
    let effort = std::fs::read_to_string(dir.path().join("ReasoningEffort.ts")).unwrap();
    assert!(effort.contains("high") && effort.contains("none"));
    let auth = std::fs::read_to_string(dir.path().join("BackendAuth.ts")).unwrap();
    for configured in [
        gents::document_config::BackendAuth::Unauthenticated,
        gents::document_config::BackendAuth::ApiKey {
            key: "secret".into(),
        },
        gents::document_config::BackendAuth::Environment {
            variable: "API_KEY".into(),
        },
        gents::document_config::BackendAuth::NodeOAuth { account_ref: None },
    ] {
        let wire = serde_json::to_value(configured).unwrap();
        assert!(auth.contains(wire["kind"].as_str().unwrap()));
    }
    assert!(auth.contains("kind"));
    let tools = std::fs::read_to_string(dir.path().join("Tools.ts")).unwrap();
    assert!(tools.contains("host?: HostTools | null"));
    assert!(tools.contains("tags?: Array<string> | null"));
    for compact in [
        serde_json::json!({"document":{"tools_id":"tools","node_did":"did:test:owner"}}),
        serde_json::json!({"document":{"tools_id":"tools","node_did":"did:test:owner","tags":null,"host":{"files":{"mode":null}}}}),
    ] {
        serde_json::from_value::<ToolsSaveRequest>(compact).unwrap();
    }
    assert!(
        serde_json::from_value::<ToolsSaveRequest>(serde_json::json!({
            "document":{"tools_id":"tools","node_did":"did:test:owner","enable_file_tools":true}
        }))
        .is_err()
    );
}

#[test]
fn retained_file_image_discriminants_match_native_serialization() {
    use gents_protocol::file_changes::{FileImage, FilePermissions, ImageFailure};
    let declaration = FileImage::decl();
    for image in [
        FileImage::Missing,
        FileImage::File {
            content_hash: String::new(),
            data: String::new(),
            permissions: FilePermissions {
                readonly: false,
                unix_mode: None,
            },
            links: None,
        },
        FileImage::Unavailable {
            reason: ImageFailure::MutationIncomplete,
        },
    ] {
        let value = serde_json::to_value(image).unwrap();
        let discriminator = format!(r#""kind": "{}""#, value["kind"].as_str().unwrap());
        assert!(declaration.contains(&discriminator), "{declaration}");
    }
}
