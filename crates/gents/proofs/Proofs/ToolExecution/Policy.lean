import Proofs.Basic

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

inductive IdempotencyEvidence where
  | unknown
  | idempotent
  | nonIdempotent
  deriving DecidableEq, Repr

namespace IdempotencyEvidence

def toDefraDB : IdempotencyEvidence → String
  | .unknown => "unknown"
  | .idempotent => "idempotent"
  | .nonIdempotent => "nonIdempotent"

def all : List IdempotencyEvidence :=
  [ .unknown, .idempotent, .nonIdempotent ]

end IdempotencyEvidence

inductive ToolOperation where
  | mcpListTools
  | mcpCall
  | nativeCommand
  deriving DecidableEq, Repr

namespace ToolOperation

def toDefraDB : ToolOperation → String
  | .mcpListTools => "mcpListTools"
  | .mcpCall => "mcpCall"
  | .nativeCommand => "nativeCommand"

def all : List ToolOperation :=
  [ .mcpListTools, .mcpCall, .nativeCommand ]

end ToolOperation

inductive FailureClass where
  | approvalDenied
  | argumentInvalid
  | serviceUnavailable
  | transport
  | toolReturnedError
  | policyDenied
  | external
  deriving DecidableEq, Repr

namespace FailureClass

def toDefraDB : FailureClass → String
  | .approvalDenied => "approvalDenied"
  | .argumentInvalid => "argumentInvalid"
  | .serviceUnavailable => "serviceUnavailable"
  | .transport => "transport"
  | .toolReturnedError => "toolReturnedError"
  | .policyDenied => "policyDenied"
  | .external => "external"

def all : List FailureClass :=
  [ .approvalDenied
  , .argumentInvalid
  , .serviceUnavailable
  , .transport
  , .toolReturnedError
  , .policyDenied
  , .external
  ]

end FailureClass

inductive PreflightDecision where
  | dispatch
  | hold
  | block (failure : FailureClass)
  deriving DecidableEq, Repr

namespace PreflightDecision

def toContract : PreflightDecision → String
  | .dispatch => "dispatch"
  | .hold => "hold"
  | .block _ => "block"

def failureClass : PreflightDecision → Option FailureClass
  | .dispatch => none
  | .hold => none
  | .block failure => some failure

end PreflightDecision

inductive RetryDisposition where
  | doNotRetry
  | retrySafeRead
  | retryIdempotentToolCall
  deriving DecidableEq, Repr

namespace RetryDisposition

def toDefraDB : RetryDisposition → String
  | .doNotRetry => "doNotRetry"
  | .retrySafeRead => "retrySafeRead"
  | .retryIdempotentToolCall => "retryIdempotentToolCall"

def all : List RetryDisposition :=
  [ .doNotRetry, .retrySafeRead, .retryIdempotentToolCall ]

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
  idempotency : IdempotencyEvidence
  failure : FailureClass
  disposition : RetryDisposition
  deriving Repr

def retryDisposition
    (operation : ToolOperation)
    (idempotency : IdempotencyEvidence)
    (failure : FailureClass) : RetryDisposition :=
  match operation, idempotency, failure with
  | .mcpListTools, _, .transport => .retrySafeRead
  | .mcpCall, .idempotent, .transport => .retryIdempotentToolCall
  | _, _, _ => .doNotRetry

def preflightCaseName
    (health : Health)
    (schema : SchemaStatus)
    (decision : PreflightDecision) : String :=
  let suffix :=
    match decision with
    | .dispatch => "dispatch"
    | .hold => "hold"
    | .block failure => "blocks_" ++ failure.toDefraDB
  "preflight_" ++ health.toDefraDB ++ "_" ++ schema.toDefraDB ++ "_" ++ suffix

def retryCaseName
    (operation : ToolOperation)
    (idempotency : IdempotencyEvidence)
    (failure : FailureClass)
    (disposition : RetryDisposition) : String :=
  "retry_"
    ++ operation.toDefraDB ++ "_"
    ++ idempotency.toDefraDB ++ "_"
    ++ failure.toDefraDB ++ "_"
    ++ disposition.toDefraDB

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
  ToolOperation.all.flatMap fun operation =>
    IdempotencyEvidence.all.flatMap fun idempotency =>
      FailureClass.all.map fun failure =>
        let disposition := retryDisposition operation idempotency failure
        { name := retryCaseName operation idempotency failure disposition
        , operation := operation
        , idempotency := idempotency
        , failure := failure
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

theorem mcp_call_without_idempotency_metadata_does_not_retry
    (failure : FailureClass) :
    retryDisposition .mcpCall .unknown failure = .doNotRetry := by
  cases failure <;> rfl

theorem mcp_call_transport_retry_requires_idempotency
    (idempotency : IdempotencyEvidence)
    (h_retry :
      retryDisposition .mcpCall idempotency .transport =
        .retryIdempotentToolCall) :
    idempotency = .idempotent := by
  cases idempotency <;> simp [retryDisposition] at h_retry ⊢

theorem native_command_not_retried_by_tool_model
    (idempotency : IdempotencyEvidence)
    (failure : FailureClass) :
    retryDisposition .nativeCommand idempotency failure = .doNotRetry := by
  cases idempotency <;> cases failure <;> rfl

theorem list_tools_transport_retry_is_safe_read
    (idempotency : IdempotencyEvidence) :
    retryDisposition .mcpListTools idempotency .transport = .retrySafeRead := by
  cases idempotency <;> rfl

end ToolExecution
