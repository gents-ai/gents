import Proofs.Shared.Basic

namespace ToolExecution

inductive Health where
  | healthy
  | stale
  | unreachable
  deriving DecidableEq, Repr

namespace Health

def toDefraDB : Health → String
  | .healthy => "healthy"
  | .stale => "stale"
  | .unreachable => "unreachable"

def all : List Health :=
  [ .healthy, .stale, .unreachable ]

end Health

inductive SchemaStatus where
  | unchecked
  | valid
  | invalid
  deriving DecidableEq, Repr

namespace SchemaStatus

def toDefraDB : SchemaStatus → String
  | .unchecked => "unchecked"
  | .valid => "valid"
  | .invalid => "invalid"

def all : List SchemaStatus :=
  [ .unchecked, .valid, .invalid ]

end SchemaStatus

inductive ToolOperation where
  | mcpListTools
  | mcpCall
  | nativeCommand
  /-- `sessionMessage` (`agent_new`/`agent_message`) materializes or steers a
  request in another session through `lifecycle::materialize`; the row has no
  host process or deadline and its terminal is the caused request's terminal
  output, delivered as a completion notification. -/
  | sessionMessage
  /-- A background row with no host process; its terminal is the job driver's
  decision, never a child's terminal. -/
  | pluginJob
  deriving DecidableEq, Repr

namespace ToolOperation

def toDefraDB : ToolOperation → String
  | .mcpListTools => "mcpListTools"
  | .mcpCall => "mcpCall"
  | .nativeCommand => "nativeCommand"
  | .sessionMessage => "sessionMessage"
  | .pluginJob => "pluginJob"

def all : List ToolOperation :=
  [ .mcpListTools, .mcpCall, .nativeCommand, .sessionMessage, .pluginJob ]

@[simp] def isDetached : ToolOperation → Bool
  | .sessionMessage => true
  | .pluginJob => true
  | .mcpListTools => false
  | .mcpCall => false
  | .nativeCommand => false

theorem isDetached_iff (o : ToolOperation) :
    o.isDetached = true ↔ o = .sessionMessage ∨ o = .pluginJob := by
  cases o <;> simp [isDetached]

end ToolOperation

inductive FailureClass where
  | argumentInvalid
  | serviceUnavailable
  | transport
  | toolReturnedError
  | policyDenied
  | external
  deriving DecidableEq, Repr

namespace FailureClass

def toDefraDB : FailureClass → String
  | .argumentInvalid => "argumentInvalid"
  | .serviceUnavailable => "serviceUnavailable"
  | .transport => "transport"
  | .toolReturnedError => "toolReturnedError"
  | .policyDenied => "policyDenied"
  | .external => "external"

def all : List FailureClass :=
  [ .argumentInvalid
  , .serviceUnavailable
  , .transport
  , .toolReturnedError
  , .policyDenied
  , .external
  ]

end FailureClass

inductive PreflightDecision where
  | dispatch
  | block (failure : FailureClass)
  deriving DecidableEq, Repr

namespace PreflightDecision

def toContract : PreflightDecision → String
  | .dispatch => "dispatch"
  | .block _ => "block"

def failureClass : PreflightDecision → Option FailureClass
  | .dispatch => none
  | .block failure => some failure

end PreflightDecision

inductive RetryDisposition where
  | doNotRetry
  | retrySafeRead
  deriving DecidableEq, Repr

namespace RetryDisposition

def toDefraDB : RetryDisposition → String
  | .doNotRetry => "doNotRetry"
  | .retrySafeRead => "retrySafeRead"

def all : List RetryDisposition :=
  [ .doNotRetry, .retrySafeRead ]

theorem all_complete
    (disposition : RetryDisposition) :
    disposition ∈ all := by
  cases disposition <;> simp [all]

end RetryDisposition

def preflight
    (health : Health)
    (schema : SchemaStatus) : PreflightDecision :=
  match health with
  | .unreachable => .block .serviceUnavailable
  | .healthy | .stale =>
      match schema with
      | .invalid => .block .argumentInvalid
      | .unchecked | .valid => .dispatch

structure PreflightCase where
  name : String
  health : Health
  schema : SchemaStatus
  decision : PreflightDecision
  deriving Repr

structure RetryCase where
  name : String
  operation : ToolOperation
  disposition : RetryDisposition
  deriving Repr

/-- The MCP pool retries a failed `list_tools` once on a fresh connection,
whatever the failure, because listing is a safe read. It carries no idempotency
evidence for a tool call, so it never retries `call_tool`, and no other tool
operation is retried by the tool layer. -/
def retryDisposition : ToolOperation → RetryDisposition
  | .mcpListTools => .retrySafeRead
  | _ => .doNotRetry

def preflightCaseName
    (health : Health)
    (schema : SchemaStatus)
    (decision : PreflightDecision) : String :=
  let suffix :=
    match decision with
    | .dispatch => "dispatch"
      | .block failure => "blocks_" ++ failure.toDefraDB
  "preflight_" ++ health.toDefraDB ++ "_" ++ schema.toDefraDB ++ "_" ++ suffix

def retryCaseName (operation : ToolOperation) (disposition : RetryDisposition) : String :=
  "retry_" ++ operation.toDefraDB ++ "_" ++ disposition.toDefraDB

def preflightCases : List PreflightCase :=
  Health.all.flatMap fun health =>
    SchemaStatus.all.map fun schema =>
      let decision := preflight health schema
      { name := preflightCaseName health schema decision
      , health := health
      , schema := schema
      , decision := decision
      }

def retryCases : List RetryCase :=
  ToolOperation.all.map fun operation =>
    let disposition := retryDisposition operation
    { name := retryCaseName operation disposition
    , operation := operation
    , disposition := disposition
    }

theorem unreachable_blocks_dispatch
    (schema : SchemaStatus) :
    preflight .unreachable schema = .block .serviceUnavailable := by
  cases schema <;> rfl

theorem invalid_schema_blocks_healthy_dispatch :
    preflight .healthy .invalid = .block .argumentInvalid := rfl

theorem invalid_schema_blocks_stale_dispatch :
    preflight .stale .invalid = .block .argumentInvalid := rfl

theorem mcp_call_does_not_retry : retryDisposition .mcpCall = .doNotRetry := rfl

theorem native_command_not_retried_by_tool_model :
    retryDisposition .nativeCommand = .doNotRetry := rfl

theorem list_tools_retry_is_safe_read : retryDisposition .mcpListTools = .retrySafeRead := rfl

end ToolExecution
