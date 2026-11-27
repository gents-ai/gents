import Proofs.Shared.OperationRecovery
import Proofs.Conformance.Family

namespace OperationRecovery

deriving instance Lean.ToJson for
  NextCallKind,
  NextCall,
  AgentCreateOperation,
  AgentError,
  DocumentResource,
  DocumentError,
  DocumentDeleteError,
  GoalError,
  InferenceDocument,
  InferenceVerb,
  InferenceError,
  InstallRepairBlocker,
  MailboxError,
  McpServiceError,
  RegistryError,
  PackOperation,
  PackError,
  AgentPermissionError,
  RequestError,
  SchemaInstallDecision,
  SessionState,
  SessionStopStatus,
  SessionError,
  SkillError,
  TaskError,
  ToolsError,
  WorkspaceRootVerb,
  WorkspaceRootError

attribute [conformance_function]
  agentRecovery
  agentEditRecovery
  documentRecovery
  documentDeleteRecovery
  goalRecovery
  inferenceRecovery
  installRepairBlockerNextCall
  mailboxRecovery
  mcpServiceRecovery
  registryRecovery
  packRecovery
  agentPermissionRecovery
  requestErrorCode
  requestNextCall
  schemaInstallRecovery
  sessionState
  sessionStopStatus
  sessionErrorCode
  sessionNextCall
  skillRecovery
  taskRecovery
  toolsRecovery
  workspaceRootRecovery

end OperationRecovery
