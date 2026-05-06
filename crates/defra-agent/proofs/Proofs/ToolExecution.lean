import Proofs.Basic

/-!
# Tool Execution Policy

Initial model for MCP/native tool dispatch boundaries. It deliberately models
only the service-local facts Rust can enforce today: health/schema preflight
and retry eligibility. Tool side effects, remote service behavior, and schema
soundness beyond the checked subset remain external assumptions.
-/

namespace ToolExecution

/-- Coarse service health visible before dispatch. -/
inductive Health where
  | healthy
  | stale
  | unreachable
  deriving DecidableEq, Repr

/-- Result of argument/schema checks available before dispatch. -/
inductive SchemaStatus where
  | unchecked
  | valid
  | invalid
  deriving DecidableEq, Repr

/-- Evidence needed before a tool call can be retried after dispatch. -/
inductive IdempotencyEvidence where
  | unknown
  | idempotent
  | nonIdempotent
  deriving DecidableEq, Repr

/-- Tool operations that have different retry semantics. -/
inductive ToolOperation where
  | mcpListTools
  | mcpCall
  | nativeCommand
  deriving DecidableEq, Repr

/-- Failure classes at the tool boundary. -/
inductive FailureClass where
  | argumentInvalid
  | serviceUnavailable
  | transport
  | toolReturnedError
  | external
  deriving DecidableEq, Repr

/-- Pre-dispatch decision. -/
inductive PreflightDecision where
  | dispatch
  | block (failure : FailureClass)
  deriving DecidableEq, Repr

/-- Retry class emitted by the model for Rust conformance docs/tests. -/
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

/-- Exhaustive constructor list used by the Rust conformance vocabulary. -/
def all : List RetryDisposition :=
  [ .doNotRetry, .retrySafeRead, .retryIdempotentToolCall ]

/-- Adding a retry disposition constructor must update `all`. -/
theorem all_complete
    (disposition : RetryDisposition) :
    disposition ∈ all := by
  cases disposition <;> simp [all]

end RetryDisposition

/-- Health/schema preflight. Stale services are allowed through with a longer
timeout; unreachable services and invalid arguments are blocked locally. -/
def preflight
    (health : Health)
    (schema : SchemaStatus) : PreflightDecision :=
  match health with
  | .unreachable => .block .serviceUnavailable
  | .healthy | .stale =>
      match schema with
      | .invalid => .block .argumentInvalid
      | .unchecked | .valid => .dispatch

/-- Retry eligibility after a failed operation. Listing tools is a safe read.
Calling tools requires explicit idempotency evidence before a transport retry.
Native command retries are outside this model. -/
def retryDisposition
    (operation : ToolOperation)
    (idempotency : IdempotencyEvidence)
    (failure : FailureClass) : RetryDisposition :=
  match operation, idempotency, failure with
  | .mcpListTools, _, .transport => .retrySafeRead
  | .mcpCall, .idempotent, .transport => .retryIdempotentToolCall
  | _, _, _ => .doNotRetry

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
