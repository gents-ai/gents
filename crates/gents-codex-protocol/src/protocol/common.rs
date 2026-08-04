use std::path::PathBuf;

use crate::protocol::v1;
use crate::protocol::v2;
use crate::JSONRPCNotification;
use crate::JSONRPCRequest;
use crate::RequestId;
use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;

/// Authentication mode for OpenAI-backed providers.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Display)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// OpenAI API key provided by the caller and stored by Codex.
    ApiKey,
    /// ChatGPT OAuth managed by Codex (tokens persisted and refreshed by Codex).
    Chatgpt,
    /// [UNSTABLE] FOR OPENAI INTERNAL USE ONLY - DO NOT USE.
    ///
    /// ChatGPT auth tokens are supplied by an external host app and are only
    /// stored in memory. Token refresh must be handled by the external host app.
    #[serde(rename = "chatgptAuthTokens")]
    #[strum(serialize = "chatgptAuthTokens")]
    ChatgptAuthTokens,
    /// Programmatic Codex auth backed by a registered Agent Identity.
    #[serde(rename = "agentIdentity")]
    #[strum(serialize = "agentIdentity")]
    AgentIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientRequestSerializationScope {
    Global(&'static str),
    GlobalSharedRead(&'static str),
    Thread { thread_id: String },
    ThreadPath { path: PathBuf },
    CommandExecProcess { process_id: String },
    Process { process_handle: String },
    FuzzyFileSearchSession { session_id: String },
    FsWatch { watch_id: String },
    McpOauth { server_name: String },
}

macro_rules! serialization_scope_expr {
    ($actual_params:ident, None) => {
        None
    };
    ($actual_params:ident, global($key:literal)) => {
        Some(ClientRequestSerializationScope::Global($key))
    };
    ($actual_params:ident, global_shared_read($key:literal)) => {
        Some(ClientRequestSerializationScope::GlobalSharedRead($key))
    };
    ($actual_params:ident, thread_id($params:ident . $field:ident)) => {
        Some(ClientRequestSerializationScope::Thread {
            thread_id: $actual_params.$field.clone(),
        })
    };
    ($actual_params:ident, optional_thread_id($params:ident . $field:ident)) => {
        $actual_params
            .$field
            .clone()
            .map(|thread_id| ClientRequestSerializationScope::Thread { thread_id })
    };
    ($actual_params:ident, thread_or_path($params:ident . $thread_field:ident, $params2:ident . $path_field:ident)) => {
        if !$actual_params.$thread_field.is_empty() {
            Some(ClientRequestSerializationScope::Thread {
                thread_id: $actual_params.$thread_field.clone(),
            })
        } else if let Some(path) = $actual_params.$path_field.clone() {
            Some(ClientRequestSerializationScope::ThreadPath { path })
        } else {
            Some(ClientRequestSerializationScope::Thread {
                thread_id: $actual_params.$thread_field.clone(),
            })
        }
    };
    ($actual_params:ident, optional_command_process_id($params:ident . $field:ident)) => {
        $actual_params
            .$field
            .clone()
            .map(|process_id| ClientRequestSerializationScope::CommandExecProcess { process_id })
    };
    ($actual_params:ident, command_process_id($params:ident . $field:ident)) => {
        Some(ClientRequestSerializationScope::CommandExecProcess {
            process_id: $actual_params.$field.clone(),
        })
    };
    ($actual_params:ident, process_handle($params:ident . $field:ident)) => {
        Some(ClientRequestSerializationScope::Process {
            process_handle: $actual_params.$field.clone(),
        })
    };
    ($actual_params:ident, fuzzy_session_id($params:ident . $field:ident)) => {
        Some(ClientRequestSerializationScope::FuzzyFileSearchSession {
            session_id: $actual_params.$field.clone(),
        })
    };
    ($actual_params:ident, fs_watch_id($params:ident . $field:ident)) => {
        Some(ClientRequestSerializationScope::FsWatch {
            watch_id: $actual_params.$field.clone(),
        })
    };
    ($actual_params:ident, mcp_oauth_server($params:ident . $field:ident)) => {
        Some(ClientRequestSerializationScope::McpOauth {
            server_name: $actual_params.$field.clone(),
        })
    };
}

/// Generates an `enum ClientRequest` where each variant is a request that the
/// client can send to the server. Each variant has associated `params` and
/// `response` types. Also generates a `export_client_responses()` function to
/// export all response types to TypeScript.
macro_rules! client_request_definitions {
    (
        $(
            $(#[experimental($reason:expr)])?
            $(#[doc = $variant_doc:literal])*
            $variant:ident $(=> $wire:literal)? {
                params: $(#[$params_meta:meta])* $params:ty,
                $(inspect_params: $inspect_params:tt,)?
                serialization: $serialization:ident $( ( $($serialization_args:tt)* ) )?,
                $(manual_payload_conversion: $manual_payload_conversion:ident,)?
                response: $response:ty,
            }
        ),* $(,)?
    ) => {
        /// Request from the client to the server.
        #[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
        #[serde(tag = "method", rename_all = "camelCase")]
        pub enum ClientRequest {
            $(
                $(#[doc = $variant_doc])*
                $(#[serde(rename = $wire)])?
                $variant {
                    #[serde(rename = "id")]
                    request_id: RequestId,
                    $(#[$params_meta])*
                    params: $params,
                },
            )*
        }

        impl ClientRequest {
            pub fn id(&self) -> &RequestId {
                match self {
                    $(Self::$variant { request_id, .. } => request_id,)*
                }
            }

            pub fn method(&self) -> String {
                serde_json::to_value(self)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("method")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| "<unknown>".to_string())
            }

            pub fn serialization_scope(&self) -> Option<ClientRequestSerializationScope> {
                match self {
                    $(
                        Self::$variant { params, .. } => {
                            let _ = params;
                            serialization_scope_expr!(
                                params, $serialization $( ( $($serialization_args)* ) )?
                            )
                        }
                    )*
                }
            }
        }

        /// Typed response from the server to the client.
        #[derive(Serialize, Deserialize, Debug, Clone)]
        #[allow(clippy::large_enum_variant)]
        #[serde(tag = "method", rename_all = "camelCase")]
        pub enum ClientResponse {
            $(
                $(#[doc = $variant_doc])*
                $(#[serde(rename = $wire)])?
                $variant {
                    #[serde(rename = "id")]
                    request_id: RequestId,
                    response: $response,
                },
            )*
        }

        impl ClientResponse {
            pub fn id(&self) -> &RequestId {
                match self {
                    $(Self::$variant { request_id, .. } => request_id,)*
                }
            }

            pub fn method(&self) -> String {
                serde_json::to_value(self)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("method")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| "<unknown>".to_string())
            }

            pub fn into_jsonrpc_parts(
                self,
            ) -> std::result::Result<(RequestId, crate::Result), serde_json::Error> {
                match self {
                    $(
                        Self::$variant { request_id, response } => {
                            serde_json::to_value(response).map(|result| (request_id, result))
                        }
                    )*
                }
            }
        }

        #[derive(Debug, Clone)]
        #[allow(clippy::large_enum_variant)]
        pub enum ClientResponsePayload {
            $( $variant($response), )*
            InterruptConversation(v1::InterruptConversationResponse),
        }

        impl ClientResponsePayload {
            pub fn into_jsonrpc_parts_and_payload(
                self,
                request_id: RequestId,
            ) -> std::result::Result<
                (RequestId, crate::Result, Option<ClientResponsePayload>),
                serde_json::Error,
            > {
                match self {
                    $(
                        Self::$variant(response) => {
                            let result = serde_json::to_value(&response)?;
                            Ok((request_id, result, Some(Self::$variant(response))))
                        }
                    )*
                    Self::InterruptConversation(response) => {
                        serde_json::to_value(response).map(|result| (request_id, result, None))
                    }
                }
            }

            pub fn into_client_response(self, request_id: RequestId) -> Option<ClientResponse> {
                match self {
                    $(
                        Self::$variant(response) => {
                            Some(ClientResponse::$variant {
                                request_id,
                                response,
                            })
                        }
                    )*
                    Self::InterruptConversation(_) => None,
                }
            }

            pub fn into_jsonrpc_parts(
                self,
                request_id: RequestId,
            ) -> std::result::Result<(RequestId, crate::Result), serde_json::Error> {
                self.to_jsonrpc_parts(request_id)
            }

            pub fn to_jsonrpc_parts(
                &self,
                request_id: RequestId,
            ) -> std::result::Result<(RequestId, crate::Result), serde_json::Error> {
                match self {
                    $(
                        Self::$variant(response) => {
                            serde_json::to_value(response).map(|result| (request_id, result))
                        }
                    )*
                    Self::InterruptConversation(response) => {
                        serde_json::to_value(response).map(|result| (request_id, result))
                    }
                }
            }
        }

        impl From<v1::InterruptConversationResponse> for ClientResponsePayload {
            fn from(response: v1::InterruptConversationResponse) -> Self {
                Self::InterruptConversation(response)
            }
        }

        $(
            client_response_payload_from_impl!(
                $variant,
                $response
                $(, $manual_payload_conversion)?
            );
        )*

    };
}

macro_rules! client_response_payload_from_impl {
    ($variant:ident, $response:ty) => {
        impl From<$response> for ClientResponsePayload {
            fn from(response: $response) -> Self {
                Self::$variant(response)
            }
        }
    };
    ($variant:ident, $response:ty, manual) => {};
}

client_request_definitions! {
    Initialize {
        params: v1::InitializeParams,
        serialization: None,
        response: v1::InitializeResponse,
    },

    /// NEW APIs
    // Thread lifecycle
    // Uses `inspect_params` because only some fields are experimental.
    ThreadStart => "thread/start" {
        params: v2::ThreadStartParams,
        inspect_params: true,
        serialization: None,
        response: v2::ThreadStartResponse,
    },
    ThreadResume => "thread/resume" {
        params: v2::ThreadResumeParams,
        inspect_params: true,
        serialization: thread_or_path(params.thread_id, params.path),
        response: v2::ThreadResumeResponse,
    },
    ThreadFork => "thread/fork" {
        params: v2::ThreadForkParams,
        inspect_params: true,
        serialization: thread_or_path(params.thread_id, params.path),
        response: v2::ThreadForkResponse,
    },
    ThreadArchive => "thread/archive" {
        params: v2::ThreadArchiveParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadArchiveResponse,
    },
    ThreadUnsubscribe => "thread/unsubscribe" {
        params: v2::ThreadUnsubscribeParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadUnsubscribeResponse,
    },
    /// Increment the thread-local out-of-band elicitation counter.
    ///
    /// This is used by external helpers to pause timeout accounting while a user
    /// approval or other elicitation is pending outside the app-server request flow.
    ThreadIncrementElicitation => "thread/increment_elicitation" {
        params: v2::ThreadIncrementElicitationParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadIncrementElicitationResponse,
    },
    /// Decrement the thread-local out-of-band elicitation counter.
    ///
    /// When the count reaches zero, timeout accounting resumes for the thread.
    ThreadDecrementElicitation => "thread/decrement_elicitation" {
        params: v2::ThreadDecrementElicitationParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadDecrementElicitationResponse,
    },
    ThreadSetName => "thread/name/set" {
        params: v2::ThreadSetNameParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadSetNameResponse,
    },
    ThreadGoalSet => "thread/goal/set" {
        params: v2::ThreadGoalSetParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadGoalSetResponse,
    },
    ThreadGoalGet => "thread/goal/get" {
        params: v2::ThreadGoalGetParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadGoalGetResponse,
    },
    ThreadGoalClear => "thread/goal/clear" {
        params: v2::ThreadGoalClearParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadGoalClearResponse,
    },
    ThreadMetadataUpdate => "thread/metadata/update" {
        params: v2::ThreadMetadataUpdateParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadMetadataUpdateResponse,
    },
    ThreadSettingsUpdate => "thread/settings/update" {
        params: v2::ThreadSettingsUpdateParams,
        inspect_params: true,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadSettingsUpdateResponse,
    },
    ThreadMemoryModeSet => "thread/memoryMode/set" {
        params: v2::ThreadMemoryModeSetParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadMemoryModeSetResponse,
    },
    MemoryReset => "memory/reset" {
        params: #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global("memory"),
        response: v2::MemoryResetResponse,
    },
    ThreadUnarchive => "thread/unarchive" {
        params: v2::ThreadUnarchiveParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadUnarchiveResponse,
    },
    ThreadCompactStart => "thread/compact/start" {
        params: v2::ThreadCompactStartParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadCompactStartResponse,
    },
    ThreadShellCommand => "thread/shellCommand" {
        params: v2::ThreadShellCommandParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadShellCommandResponse,
    },
    ThreadApproveGuardianDeniedAction => "thread/approveGuardianDeniedAction" {
        params: v2::ThreadApproveGuardianDeniedActionParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadApproveGuardianDeniedActionResponse,
    },
    ThreadBackgroundTerminalsClean => "thread/backgroundTerminals/clean" {
        params: v2::ThreadBackgroundTerminalsCleanParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadBackgroundTerminalsCleanResponse,
    },
    ThreadRollback => "thread/rollback" {
        params: v2::ThreadRollbackParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadRollbackResponse,
    },
    ThreadList => "thread/list" {
        params: v2::ThreadListParams,
        serialization: None,
        response: v2::ThreadListResponse,
    },
    ThreadSearch => "thread/search" {
        params: v2::ThreadSearchParams,
        serialization: None,
        response: v2::ThreadSearchResponse,
    },
    ThreadLoadedList => "thread/loaded/list" {
        params: v2::ThreadLoadedListParams,
        serialization: None,
        response: v2::ThreadLoadedListResponse,
    },
    ThreadRead => "thread/read" {
        params: v2::ThreadReadParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadReadResponse,
    },
    ThreadTurnsList => "thread/turns/list" {
        params: v2::ThreadTurnsListParams,
        // Explicitly concurrent: this primarily reads append-only rollout storage.
        serialization: None,
        response: v2::ThreadTurnsListResponse,
    },
    ThreadTurnsItemsList => "thread/turns/items/list" {
        params: v2::ThreadTurnsItemsListParams,
        // Explicitly concurrent: this primarily reads append-only rollout storage.
        serialization: None,
        response: v2::ThreadTurnsItemsListResponse,
    },
    /// Append raw Responses API items to the thread history without starting a user turn.
    ThreadInjectItems => "thread/inject_items" {
        params: v2::ThreadInjectItemsParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadInjectItemsResponse,
    },
    SkillsList => "skills/list" {
        params: v2::SkillsListParams,
        serialization: global_shared_read("config"),
        response: v2::SkillsListResponse,
    },
    HooksList => "hooks/list" {
        params: v2::HooksListParams,
        serialization: global("config"),
        response: v2::HooksListResponse,
    },
    MarketplaceAdd => "marketplace/add" {
        params: v2::MarketplaceAddParams,
        serialization: global("config"),
        response: v2::MarketplaceAddResponse,
    },
    MarketplaceRemove => "marketplace/remove" {
        params: v2::MarketplaceRemoveParams,
        serialization: global("config"),
        response: v2::MarketplaceRemoveResponse,
    },
    MarketplaceUpgrade => "marketplace/upgrade" {
        params: v2::MarketplaceUpgradeParams,
        serialization: global("config"),
        response: v2::MarketplaceUpgradeResponse,
    },
    PluginList => "plugin/list" {
        params: v2::PluginListParams,
        serialization: None,
        response: v2::PluginListResponse,
    },
    PluginInstalled => "plugin/installed" {
        params: v2::PluginInstalledParams,
        serialization: None,
        response: v2::PluginInstalledResponse,
    },
    PluginRead => "plugin/read" {
        params: v2::PluginReadParams,
        serialization: None,
        response: v2::PluginReadResponse,
    },
    PluginSkillRead => "plugin/skill/read" {
        params: v2::PluginSkillReadParams,
        serialization: global("config"),
        response: v2::PluginSkillReadResponse,
    },
    PluginShareSave => "plugin/share/save" {
        params: v2::PluginShareSaveParams,
        serialization: global("config"),
        response: v2::PluginShareSaveResponse,
    },
    PluginShareUpdateTargets => "plugin/share/updateTargets" {
        params: v2::PluginShareUpdateTargetsParams,
        serialization: global("config"),
        response: v2::PluginShareUpdateTargetsResponse,
    },
    PluginShareList => "plugin/share/list" {
        params: v2::PluginShareListParams,
        serialization: global("config"),
        response: v2::PluginShareListResponse,
    },
    PluginShareCheckout => "plugin/share/checkout" {
        params: v2::PluginShareCheckoutParams,
        serialization: global("config"),
        response: v2::PluginShareCheckoutResponse,
    },
    PluginShareDelete => "plugin/share/delete" {
        params: v2::PluginShareDeleteParams,
        serialization: global("config"),
        response: v2::PluginShareDeleteResponse,
    },
    AppsList => "app/list" {
        params: v2::AppsListParams,
        serialization: None,
        response: v2::AppsListResponse,
    },
    // File system requests are intentionally concurrent. Desktop already treats local
    // file system operations as concurrent, and app-server remote fs mirrors that model.
    FsReadFile => "fs/readFile" {
        params: v2::FsReadFileParams,
        serialization: None,
        response: v2::FsReadFileResponse,
    },
    FsWriteFile => "fs/writeFile" {
        params: v2::FsWriteFileParams,
        serialization: None,
        response: v2::FsWriteFileResponse,
    },
    FsCreateDirectory => "fs/createDirectory" {
        params: v2::FsCreateDirectoryParams,
        serialization: None,
        response: v2::FsCreateDirectoryResponse,
    },
    FsGetMetadata => "fs/getMetadata" {
        params: v2::FsGetMetadataParams,
        serialization: None,
        response: v2::FsGetMetadataResponse,
    },
    FsReadDirectory => "fs/readDirectory" {
        params: v2::FsReadDirectoryParams,
        serialization: None,
        response: v2::FsReadDirectoryResponse,
    },
    FsRemove => "fs/remove" {
        params: v2::FsRemoveParams,
        serialization: None,
        response: v2::FsRemoveResponse,
    },
    FsCopy => "fs/copy" {
        params: v2::FsCopyParams,
        serialization: None,
        response: v2::FsCopyResponse,
    },
    FsWatch => "fs/watch" {
        params: v2::FsWatchParams,
        serialization: fs_watch_id(params.watch_id),
        response: v2::FsWatchResponse,
    },
    FsUnwatch => "fs/unwatch" {
        params: v2::FsUnwatchParams,
        serialization: fs_watch_id(params.watch_id),
        response: v2::FsUnwatchResponse,
    },
    SkillsConfigWrite => "skills/config/write" {
        params: v2::SkillsConfigWriteParams,
        serialization: global("config"),
        response: v2::SkillsConfigWriteResponse,
    },
    PluginInstall => "plugin/install" {
        params: v2::PluginInstallParams,
        serialization: global("config"),
        response: v2::PluginInstallResponse,
    },
    PluginUninstall => "plugin/uninstall" {
        params: v2::PluginUninstallParams,
        serialization: global("config"),
        response: v2::PluginUninstallResponse,
    },
    TurnStart => "turn/start" {
        params: v2::TurnStartParams,
        inspect_params: true,
        serialization: thread_id(params.thread_id),
        response: v2::TurnStartResponse,
    },
    TurnSteer => "turn/steer" {
        params: v2::TurnSteerParams,
        inspect_params: true,
        serialization: thread_id(params.thread_id),
        response: v2::TurnSteerResponse,
    },
    TurnInterrupt => "turn/interrupt" {
        params: v2::TurnInterruptParams,
        serialization: thread_id(params.thread_id),
        response: v2::TurnInterruptResponse,
    },
    ThreadRealtimeStart => "thread/realtime/start" {
        params: v2::ThreadRealtimeStartParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadRealtimeStartResponse,
    },
    ThreadRealtimeAppendAudio => "thread/realtime/appendAudio" {
        params: v2::ThreadRealtimeAppendAudioParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadRealtimeAppendAudioResponse,
    },
    ThreadRealtimeAppendText => "thread/realtime/appendText" {
        params: v2::ThreadRealtimeAppendTextParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadRealtimeAppendTextResponse,
    },
    ThreadRealtimeStop => "thread/realtime/stop" {
        params: v2::ThreadRealtimeStopParams,
        serialization: thread_id(params.thread_id),
        response: v2::ThreadRealtimeStopResponse,
    },
    ThreadRealtimeListVoices => "thread/realtime/listVoices" {
        params: v2::ThreadRealtimeListVoicesParams,
        serialization: None,
        response: v2::ThreadRealtimeListVoicesResponse,
    },
    ReviewStart => "review/start" {
        params: v2::ReviewStartParams,
        serialization: thread_id(params.thread_id),
        response: v2::ReviewStartResponse,
    },

    ModelList => "model/list" {
        params: v2::ModelListParams,
        serialization: None,
        response: v2::ModelListResponse,
    },
    ModelProviderCapabilitiesRead => "modelProvider/capabilities/read" {
        params: v2::ModelProviderCapabilitiesReadParams,
        serialization: None,
        response: v2::ModelProviderCapabilitiesReadResponse,
    },
    ExperimentalFeatureList => "experimentalFeature/list" {
        params: v2::ExperimentalFeatureListParams,
        serialization: global("config"),
        response: v2::ExperimentalFeatureListResponse,
    },
    PermissionProfileList => "permissionProfile/list" {
        params: v2::PermissionProfileListParams,
        serialization: global_shared_read("config"),
        response: v2::PermissionProfileListResponse,
    },
    ExperimentalFeatureEnablementSet => "experimentalFeature/enablement/set" {
        params: v2::ExperimentalFeatureEnablementSetParams,
        serialization: global("config"),
        response: v2::ExperimentalFeatureEnablementSetResponse,
    },
    RemoteControlEnable => "remoteControl/enable" {
        params: #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global("remote-control"),
        response: v2::RemoteControlEnableResponse,
    },
    RemoteControlDisable => "remoteControl/disable" {
        params: #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global("remote-control"),
        response: v2::RemoteControlDisableResponse,
    },
    RemoteControlStatusRead => "remoteControl/status/read" {
        params: #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global_shared_read("remote-control"),
        response: v2::RemoteControlStatusReadResponse,
    },
    /// Lists collaboration mode presets.
    CollaborationModeList => "collaborationMode/list" {
        params: v2::CollaborationModeListParams,
        serialization: None,
        response: v2::CollaborationModeListResponse,
    },
    /// Test-only method used to validate experimental gating.
    MockExperimentalMethod => "mock/experimentalMethod" {
        params: v2::MockExperimentalMethodParams,
        serialization: None,
        response: v2::MockExperimentalMethodResponse,
    },
    /// Adds or replaces a remote environment by id for later selection.
    EnvironmentAdd => "environment/add" {
        params: v2::EnvironmentAddParams,
        serialization: global("environment"),
        response: v2::EnvironmentAddResponse,
    },

    McpServerOauthLogin => "mcpServer/oauth/login" {
        params: v2::McpServerOauthLoginParams,
        serialization: mcp_oauth_server(params.name),
        response: v2::McpServerOauthLoginResponse,
    },

    McpServerRefresh => "config/mcpServer/reload" {
        params: #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global("mcp-registry"),
        response: v2::McpServerRefreshResponse,
    },

    McpServerStatusList => "mcpServerStatus/list" {
        params: v2::ListMcpServerStatusParams,
        serialization: global("mcp-registry"),
        response: v2::ListMcpServerStatusResponse,
    },

    McpResourceRead => "mcpServer/resource/read" {
        params: v2::McpResourceReadParams,
        serialization: optional_thread_id(params.thread_id),
        response: v2::McpResourceReadResponse,
    },

    McpServerToolCall => "mcpServer/tool/call" {
        params: v2::McpServerToolCallParams,
        serialization: thread_id(params.thread_id),
        response: v2::McpServerToolCallResponse,
    },

    WindowsSandboxSetupStart => "windowsSandbox/setupStart" {
        params: v2::WindowsSandboxSetupStartParams,
        serialization: global("windows-sandbox-setup"),
        response: v2::WindowsSandboxSetupStartResponse,
    },
    WindowsSandboxReadiness => "windowsSandbox/readiness" {
        params: #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global("config"),
        response: v2::WindowsSandboxReadinessResponse,
    },

    LoginAccount => "account/login/start" {
        params: v2::LoginAccountParams,
        inspect_params: true,
        serialization: global("account-auth"),
        response: v2::LoginAccountResponse,
    },

    CancelLoginAccount => "account/login/cancel" {
        params: v2::CancelLoginAccountParams,
        serialization: global("account-auth"),
        response: v2::CancelLoginAccountResponse,
    },

    LogoutAccount => "account/logout" {
        params: #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global("account-auth"),
        response: v2::LogoutAccountResponse,
    },

    GetAccountRateLimits => "account/rateLimits/read" {
        params: #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: None,
        response: v2::GetAccountRateLimitsResponse,
    },

    SendAddCreditsNudgeEmail => "account/sendAddCreditsNudgeEmail" {
        params: v2::SendAddCreditsNudgeEmailParams,
        serialization: global("account-auth"),
        response: v2::SendAddCreditsNudgeEmailResponse,
    },

    FeedbackUpload => "feedback/upload" {
        params: v2::FeedbackUploadParams,
        serialization: None,
        response: v2::FeedbackUploadResponse,
    },

    /// Execute a standalone command (argv vector) under the server's sandbox.
    OneOffCommandExec => "command/exec" {
        params: v2::CommandExecParams,
        inspect_params: true,
        serialization: optional_command_process_id(params.process_id),
        response: v2::CommandExecResponse,
    },
    /// Write stdin bytes to a running `command/exec` session or close stdin.
    CommandExecWrite => "command/exec/write" {
        params: v2::CommandExecWriteParams,
        serialization: command_process_id(params.process_id),
        response: v2::CommandExecWriteResponse,
    },
    /// Terminate a running `command/exec` session by client-supplied `processId`.
    CommandExecTerminate => "command/exec/terminate" {
        params: v2::CommandExecTerminateParams,
        serialization: command_process_id(params.process_id),
        response: v2::CommandExecTerminateResponse,
    },
    /// Resize a running PTY-backed `command/exec` session by client-supplied `processId`.
    CommandExecResize => "command/exec/resize" {
        params: v2::CommandExecResizeParams,
        serialization: command_process_id(params.process_id),
        response: v2::CommandExecResizeResponse,
    },
    /// Spawn a standalone process (argv vector) without a Codex sandbox.
    ProcessSpawn => "process/spawn" {
        params: v2::ProcessSpawnParams,
        serialization: process_handle(params.process_handle),
        response: v2::ProcessSpawnResponse,
    },
    /// Write stdin bytes to a running `process/spawn` session or close stdin.
    ProcessWriteStdin => "process/writeStdin" {
        params: v2::ProcessWriteStdinParams,
        serialization: process_handle(params.process_handle),
        response: v2::ProcessWriteStdinResponse,
    },
    /// Terminate a running `process/spawn` session by client-supplied `processHandle`.
    ProcessKill => "process/kill" {
        params: v2::ProcessKillParams,
        serialization: process_handle(params.process_handle),
        response: v2::ProcessKillResponse,
    },
    /// Resize a running PTY-backed `process/spawn` session by client-supplied `processHandle`.
    ProcessResizePty => "process/resizePty" {
        params: v2::ProcessResizePtyParams,
        serialization: process_handle(params.process_handle),
        response: v2::ProcessResizePtyResponse,
    },

    ConfigRead => "config/read" {
        params: v2::ConfigReadParams,
        serialization: global_shared_read("config"),
        response: v2::ConfigReadResponse,
    },
    ExternalAgentConfigDetect => "externalAgentConfig/detect" {
        params: v2::ExternalAgentConfigDetectParams,
        serialization: global("config"),
        response: v2::ExternalAgentConfigDetectResponse,
    },
    ExternalAgentConfigImport => "externalAgentConfig/import" {
        params: v2::ExternalAgentConfigImportParams,
        serialization: global("config"),
        response: v2::ExternalAgentConfigImportResponse,
    },
    ConfigValueWrite => "config/value/write" {
        params: v2::ConfigValueWriteParams,
        serialization: global("config"),
        manual_payload_conversion: manual,
        response: v2::ConfigWriteResponse,
    },
    ConfigBatchWrite => "config/batchWrite" {
        params: v2::ConfigBatchWriteParams,
        serialization: global("config"),
        manual_payload_conversion: manual,
        response: v2::ConfigWriteResponse,
    },

    ConfigRequirementsRead => "configRequirements/read" {
        params: #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global("config"),
        response: v2::ConfigRequirementsReadResponse,
    },

    GetAccount => "account/read" {
        params: v2::GetAccountParams,
        serialization: global("account-auth"),
        response: v2::GetAccountResponse,
    },

    /// DEPRECATED APIs below
    GetConversationSummary {
        params: v1::GetConversationSummaryParams,
        serialization: None,
        response: v1::GetConversationSummaryResponse,
    },
    GitDiffToRemote {
        params: v1::GitDiffToRemoteParams,
        serialization: None,
        response: v1::GitDiffToRemoteResponse,
    },
    /// DEPRECATED in favor of GetAccount
    GetAuthStatus {
        params: v1::GetAuthStatusParams,
        serialization: global("account-auth"),
        response: v1::GetAuthStatusResponse,
    },
    // Legacy fuzzy search cancellation is intentionally concurrent: clients reuse a
    // cancellation token so a newer request can cancel an older in-flight search.
    FuzzyFileSearch {
        params: FuzzyFileSearchParams,
        serialization: None,
        response: FuzzyFileSearchResponse,
    },
    FuzzyFileSearchSessionStart => "fuzzyFileSearch/sessionStart" {
        params: FuzzyFileSearchSessionStartParams,
        serialization: fuzzy_session_id(params.session_id),
        response: FuzzyFileSearchSessionStartResponse,
    },
    FuzzyFileSearchSessionUpdate => "fuzzyFileSearch/sessionUpdate" {
        params: FuzzyFileSearchSessionUpdateParams,
        serialization: fuzzy_session_id(params.session_id),
        response: FuzzyFileSearchSessionUpdateResponse,
    },
    FuzzyFileSearchSessionStop => "fuzzyFileSearch/sessionStop" {
        params: FuzzyFileSearchSessionStopParams,
        serialization: fuzzy_session_id(params.session_id),
        response: FuzzyFileSearchSessionStopResponse,
    },
}

/// Generates an `enum ServerRequest` where each variant is a request that the
/// server can send to the client along with the corresponding params and
/// response types. It also generates helper types used by the app/server
/// infrastructure (payload enum, request constructor, and export helpers).
macro_rules! server_request_definitions {
    (
        $(
            $(#[$variant_meta:meta])*
            $variant:ident $(=> $wire:literal)? {
                params: $params:ty,
                response: $response:ty,
            }
        ),* $(,)?
    ) => {
        /// Request initiated from the server and sent to the client.
        #[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
        #[allow(clippy::large_enum_variant)]
        #[serde(tag = "method", rename_all = "camelCase")]
        pub enum ServerRequest {
            $(
                $(#[$variant_meta])*
                $(#[serde(rename = $wire)])?
                $variant {
                    #[serde(rename = "id")]
                    request_id: RequestId,
                    params: $params,
                },
            )*
        }

        impl ServerRequest {
            pub fn id(&self) -> &RequestId {
                match self {
                    $(Self::$variant { request_id, .. } => request_id,)*
                }
            }

            pub fn response_from_result(
                &self,
                result: crate::Result,
            ) -> serde_json::Result<ServerResponse> {
                match self {
                    $(
                        Self::$variant { request_id, .. } => {
                            let response = serde_json::from_value::<$response>(result)?;
                            Ok(ServerResponse::$variant {
                                request_id: request_id.clone(),
                                response,
                            })
                        }
                    )*
                }
            }
        }

        /// Typed response from the client to the server.
        #[derive(Serialize, Deserialize, Debug, Clone)]
        #[serde(tag = "method", rename_all = "camelCase")]
        pub enum ServerResponse {
            $(
                $(#[$variant_meta])*
                $(#[serde(rename = $wire)])?
                $variant {
                    #[serde(rename = "id")]
                    request_id: RequestId,
                    response: $response,
                },
            )*
        }

        impl ServerResponse {
            pub fn id(&self) -> &RequestId {
                match self {
                    $(Self::$variant { request_id, .. } => request_id,)*
                }
            }

            pub fn method(&self) -> String {
                serde_json::to_value(self)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("method")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| "<unknown>".to_string())
            }
        }

        #[derive(Debug, Clone, PartialEq)]
        #[allow(clippy::large_enum_variant)]
        pub enum ServerRequestPayload {
            $( $variant($params), )*
        }

        impl ServerRequestPayload {
            pub fn request_with_id(self, request_id: RequestId) -> ServerRequest {
                match self {
                    $(Self::$variant(params) => ServerRequest::$variant { request_id, params },)*
                }
            }
        }

    };
}

/// Generates `ServerNotification` enum and helpers, including a JSON Schema
/// exporter for each notification.
macro_rules! server_notification_definitions {
    (
        $(
            $(#[$variant_meta:meta])*
            $variant:ident $(=> $wire:literal)? ( $payload:ty )
        ),* $(,)?
    ) => {
        /// Notification sent from the server to the client.
        #[derive(
            Serialize,
            Deserialize,
            Debug,
            Clone,
            Display,
                    )]
        #[allow(clippy::large_enum_variant)]
        #[serde(tag = "method", content = "params", rename_all = "camelCase")]
        #[strum(serialize_all = "camelCase")]
        pub enum ServerNotification {
            $(
                $(#[$variant_meta])*
                $(#[serde(rename = $wire)] #[strum(serialize = $wire)])?
                $variant($payload),
            )*
        }

        impl ServerNotification {
            pub fn to_params(self) -> Result<serde_json::Value, serde_json::Error> {
                match self {
                    $(Self::$variant(params) => serde_json::to_value(params),)*
                }
            }
        }

        impl TryFrom<JSONRPCNotification> for ServerNotification {
            type Error = serde_json::Error;

            fn try_from(value: JSONRPCNotification) -> Result<Self, serde_json::Error> {
                serde_json::from_value(serde_json::to_value(value)?)
            }
        }

    };
}
/// Notifications sent from the client to the server.
macro_rules! client_notification_definitions {
    (
        $(
            $(#[$variant_meta:meta])*
            $variant:ident $( ( $payload:ty ) )?
        ),* $(,)?
    ) => {
        #[derive(Serialize, Deserialize, Debug, Clone, Display)]
        #[serde(tag = "method", content = "params", rename_all = "camelCase")]
        #[strum(serialize_all = "camelCase")]
        pub enum ClientNotification {
            $(
                $(#[$variant_meta])*
                $variant $( ( $payload ) )?,
            )*
        }

    };
}

impl TryFrom<JSONRPCRequest> for ServerRequest {
    type Error = serde_json::Error;

    fn try_from(value: JSONRPCRequest) -> Result<Self, Self::Error> {
        serde_json::from_value(serde_json::to_value(value)?)
    }
}

server_request_definitions! {
    /// NEW APIs
    /// Sent when approval is requested for a specific command execution.
    /// This request is used for Turns started via turn/start.
    CommandExecutionRequestApproval => "item/commandExecution/requestApproval" {
        params: v2::CommandExecutionRequestApprovalParams,
        response: v2::CommandExecutionRequestApprovalResponse,
    },

    /// Sent when approval is requested for a specific file change.
    /// This request is used for Turns started via turn/start.
    FileChangeRequestApproval => "item/fileChange/requestApproval" {
        params: v2::FileChangeRequestApprovalParams,
        response: v2::FileChangeRequestApprovalResponse,
    },

    /// EXPERIMENTAL - Request input from the user for a tool call.
    ToolRequestUserInput => "item/tool/requestUserInput" {
        params: v2::ToolRequestUserInputParams,
        response: v2::ToolRequestUserInputResponse,
    },

    /// Request input for an MCP server elicitation.
    McpServerElicitationRequest => "mcpServer/elicitation/request" {
        params: v2::McpServerElicitationRequestParams,
        response: v2::McpServerElicitationRequestResponse,
    },

    /// Request approval for additional permissions from the user.
    PermissionsRequestApproval => "item/permissions/requestApproval" {
        params: v2::PermissionsRequestApprovalParams,
        response: v2::PermissionsRequestApprovalResponse,
    },

    /// Execute a dynamic tool call on the client.
    DynamicToolCall => "item/tool/call" {
        params: v2::DynamicToolCallParams,
        response: v2::DynamicToolCallResponse,
    },

    ChatgptAuthTokensRefresh => "account/chatgptAuthTokens/refresh" {
        params: v2::ChatgptAuthTokensRefreshParams,
        response: v2::ChatgptAuthTokensRefreshResponse,
    },

    /// Generate a fresh upstream attestation result on demand.
    AttestationGenerate => "attestation/generate" {
        params: v2::AttestationGenerateParams,
        response: v2::AttestationGenerateResponse,
    },

    /// DEPRECATED APIs below
    /// Request to approve a patch.
    /// This request is used for Turns started via the legacy APIs (i.e. SendUserTurn, SendUserMessage).
    ApplyPatchApproval {
        params: v1::ApplyPatchApprovalParams,
        response: v1::ApplyPatchApprovalResponse,
    },
    /// Request to exec a command.
    /// This request is used for Turns started via the legacy APIs (i.e. SendUserTurn, SendUserMessage).
    ExecCommandApproval {
        params: v1::ExecCommandApprovalParams,
        response: v1::ExecCommandApprovalResponse,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FuzzyFileSearchParams {
    pub query: String,
    pub roots: Vec<String>,
    // if provided, will cancel any previous request that used the same value
    pub cancellation_token: Option<String>,
}

/// Superset of [`codex_file_search::FileMatch`]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FuzzyFileSearchResult {
    pub root: String,
    pub path: String,
    pub match_type: FuzzyFileSearchMatchType,
    pub file_name: String,
    pub score: u32,
    pub indices: Option<Vec<u32>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FuzzyFileSearchMatchType {
    File,
    Directory,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FuzzyFileSearchResponse {
    pub files: Vec<FuzzyFileSearchResult>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FuzzyFileSearchSessionStartParams {
    pub session_id: String,
    pub roots: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct FuzzyFileSearchSessionStartResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FuzzyFileSearchSessionUpdateParams {
    pub session_id: String,
    pub query: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct FuzzyFileSearchSessionUpdateResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FuzzyFileSearchSessionStopParams {
    pub session_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct FuzzyFileSearchSessionStopResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FuzzyFileSearchSessionUpdatedNotification {
    pub session_id: String,
    pub query: String,
    pub files: Vec<FuzzyFileSearchResult>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FuzzyFileSearchSessionCompletedNotification {
    pub session_id: String,
}

server_notification_definitions! {
    /// NEW NOTIFICATIONS
    Error => "error" (v2::ErrorNotification),
    ThreadStarted => "thread/started" (v2::ThreadStartedNotification),
    ThreadStatusChanged => "thread/status/changed" (v2::ThreadStatusChangedNotification),
    ThreadArchived => "thread/archived" (v2::ThreadArchivedNotification),
    ThreadUnarchived => "thread/unarchived" (v2::ThreadUnarchivedNotification),
    ThreadClosed => "thread/closed" (v2::ThreadClosedNotification),
    SkillsChanged => "skills/changed" (v2::SkillsChangedNotification),
    ThreadNameUpdated => "thread/name/updated" (v2::ThreadNameUpdatedNotification),
    ThreadGoalUpdated => "thread/goal/updated" (v2::ThreadGoalUpdatedNotification),
    ThreadGoalCleared => "thread/goal/cleared" (v2::ThreadGoalClearedNotification),
    ThreadSettingsUpdated => "thread/settings/updated" (v2::ThreadSettingsUpdatedNotification),
    ThreadTokenUsageUpdated => "thread/tokenUsage/updated" (v2::ThreadTokenUsageUpdatedNotification),
    TurnStarted => "turn/started" (v2::TurnStartedNotification),
    HookStarted => "hook/started" (v2::HookStartedNotification),
    TurnCompleted => "turn/completed" (v2::TurnCompletedNotification),
    HookCompleted => "hook/completed" (v2::HookCompletedNotification),
    TurnDiffUpdated => "turn/diff/updated" (v2::TurnDiffUpdatedNotification),
    TurnPlanUpdated => "turn/plan/updated" (v2::TurnPlanUpdatedNotification),
    ItemStarted => "item/started" (v2::ItemStartedNotification),
    ItemGuardianApprovalReviewStarted => "item/autoApprovalReview/started" (v2::ItemGuardianApprovalReviewStartedNotification),
    ItemGuardianApprovalReviewCompleted => "item/autoApprovalReview/completed" (v2::ItemGuardianApprovalReviewCompletedNotification),
    ItemCompleted => "item/completed" (v2::ItemCompletedNotification),
    /// This event is internal-only. Used by Codex Cloud.
    RawResponseItemCompleted => "rawResponseItem/completed" (v2::RawResponseItemCompletedNotification),
    AgentMessageDelta => "item/agentMessage/delta" (v2::AgentMessageDeltaNotification),
    /// EXPERIMENTAL - proposed plan streaming deltas for plan items.
    PlanDelta => "item/plan/delta" (v2::PlanDeltaNotification),
    /// Stream base64-encoded stdout/stderr chunks for a running `command/exec` session.
    CommandExecOutputDelta => "command/exec/outputDelta" (v2::CommandExecOutputDeltaNotification),
    /// Stream base64-encoded stdout/stderr chunks for a running `process/spawn` session.
    ProcessOutputDelta => "process/outputDelta" (v2::ProcessOutputDeltaNotification),
    /// Final exit notification for a `process/spawn` session.
    ProcessExited => "process/exited" (v2::ProcessExitedNotification),
    CommandExecutionOutputDelta => "item/commandExecution/outputDelta" (v2::CommandExecutionOutputDeltaNotification),
    TerminalInteraction => "item/commandExecution/terminalInteraction" (v2::TerminalInteractionNotification),
    /// Deprecated legacy apply_patch output stream notification.
    FileChangeOutputDelta => "item/fileChange/outputDelta" (v2::FileChangeOutputDeltaNotification),
    FileChangePatchUpdated => "item/fileChange/patchUpdated" (v2::FileChangePatchUpdatedNotification),
    ServerRequestResolved => "serverRequest/resolved" (v2::ServerRequestResolvedNotification),
    McpToolCallProgress => "item/mcpToolCall/progress" (v2::McpToolCallProgressNotification),
    McpServerOauthLoginCompleted => "mcpServer/oauthLogin/completed" (v2::McpServerOauthLoginCompletedNotification),
    McpServerStatusUpdated => "mcpServer/startupStatus/updated" (v2::McpServerStatusUpdatedNotification),
    AccountUpdated => "account/updated" (v2::AccountUpdatedNotification),
    AccountRateLimitsUpdated => "account/rateLimits/updated" (v2::AccountRateLimitsUpdatedNotification),
    AppListUpdated => "app/list/updated" (v2::AppListUpdatedNotification),
    RemoteControlStatusChanged => "remoteControl/status/changed" (v2::RemoteControlStatusChangedNotification),
    ExternalAgentConfigImportCompleted => "externalAgentConfig/import/completed" (v2::ExternalAgentConfigImportCompletedNotification),
    FsChanged => "fs/changed" (v2::FsChangedNotification),
    ReasoningSummaryTextDelta => "item/reasoning/summaryTextDelta" (v2::ReasoningSummaryTextDeltaNotification),
    ReasoningSummaryPartAdded => "item/reasoning/summaryPartAdded" (v2::ReasoningSummaryPartAddedNotification),
    ReasoningTextDelta => "item/reasoning/textDelta" (v2::ReasoningTextDeltaNotification),
    /// Deprecated: Use `ContextCompaction` item type instead.
    ContextCompacted => "thread/compacted" (v2::ContextCompactedNotification),
    ModelRerouted => "model/rerouted" (v2::ModelReroutedNotification),
    ModelVerification => "model/verification" (v2::ModelVerificationNotification),
    Warning => "warning" (v2::WarningNotification),
    GuardianWarning => "guardianWarning" (v2::GuardianWarningNotification),
    DeprecationNotice => "deprecationNotice" (v2::DeprecationNoticeNotification),
    ConfigWarning => "configWarning" (v2::ConfigWarningNotification),
    FuzzyFileSearchSessionUpdated => "fuzzyFileSearch/sessionUpdated" (FuzzyFileSearchSessionUpdatedNotification),
    FuzzyFileSearchSessionCompleted => "fuzzyFileSearch/sessionCompleted" (FuzzyFileSearchSessionCompletedNotification),
    ThreadRealtimeStarted => "thread/realtime/started" (v2::ThreadRealtimeStartedNotification),
    ThreadRealtimeItemAdded => "thread/realtime/itemAdded" (v2::ThreadRealtimeItemAddedNotification),
    ThreadRealtimeTranscriptDelta => "thread/realtime/transcript/delta" (v2::ThreadRealtimeTranscriptDeltaNotification),
    ThreadRealtimeTranscriptDone => "thread/realtime/transcript/done" (v2::ThreadRealtimeTranscriptDoneNotification),
    ThreadRealtimeOutputAudioDelta => "thread/realtime/outputAudio/delta" (v2::ThreadRealtimeOutputAudioDeltaNotification),
    ThreadRealtimeSdp => "thread/realtime/sdp" (v2::ThreadRealtimeSdpNotification),
    ThreadRealtimeError => "thread/realtime/error" (v2::ThreadRealtimeErrorNotification),
    ThreadRealtimeClosed => "thread/realtime/closed" (v2::ThreadRealtimeClosedNotification),

    /// Notifies the user of world-writable directories on Windows, which cannot be protected by the sandbox.
    WindowsWorldWritableWarning => "windows/worldWritableWarning" (v2::WindowsWorldWritableWarningNotification),
    WindowsSandboxSetupCompleted => "windowsSandbox/setupCompleted" (v2::WindowsSandboxSetupCompletedNotification),

    #[serde(rename = "account/login/completed")]
    #[strum(serialize = "account/login/completed")]
    AccountLoginCompleted(v2::AccountLoginCompletedNotification),

}

client_notification_definitions! {
    Initialized,
}
