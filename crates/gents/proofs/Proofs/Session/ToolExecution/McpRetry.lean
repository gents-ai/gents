import Proofs.Session.ToolExecution.Policy

/-!
Idempotent retry policy for MCP `call_tool` invocations.

Retrying a failed `call_tool` is only sound when:
1. The tool's declared metadata certifies that repeating the identical call
   (`service_id`, `tool_name`, `arguments`) has no duplicate side effects
   (`readOnly` or `idempotent`).
2. The failure is a transient transport drop (`transportDropped`), not a
   handshake/connect failure, authorization refusal, protocol/tool error,
   timeout, or cancellation.
3. The per-call retry budget has not been exhausted and stays within the global
   retry cap (`maxAllowedRetries`).

Until a tool exposes retry-safe metadata, its metadata is `.absent` and every
failure remains final (`failFinal`), preserving the default no-retry behavior
of `ToolExecution.retryDisposition .mcpCall = .doNotRetry`.
-/

namespace ToolExecution
namespace McpRetry

/-- Three-valued encoding of an optional boolean hint from MCP
`ToolAnnotations` (`readOnlyHint`, `idempotentHint`). -/
inductive HintValue where
  | absent
  | isFalse
  | isTrue
  deriving DecidableEq, Repr

namespace HintValue

def all : List HintValue :=
  [ .absent, .isFalse, .isTrue ]

end HintValue

/-- Retry-safety metadata classified for an MCP tool.

External replay premises (held by the remote MCP server, never verified by
Gents):
- `readOnly`: the tool does not modify external state, so repeating the call
  with identical arguments produces the same environment state as one call.
- `idempotent`: the tool may modify external state, but repeating the call
  with identical arguments produces the same cumulative effect as executing it
  once (no duplicate side effects).
- `absent` / `nonIdempotent`: no idempotency guarantee is held; repeating an
  attempt whose effect may have been applied can duplicate side effects, so
  the pool must not retry. -/
inductive RetryMetadata where
  /-- Tool exposed no retry-safety annotations (the default). -/
  | absent
  /-- Tool explicitly disclaimed idempotency (`idempotentHint = false`). -/
  | nonIdempotent
  /-- Tool declared `readOnlyHint = true` without disclaiming idempotency. -/
  | readOnly
  /-- Tool declared `idempotentHint = true`. -/
  | idempotent
  deriving DecidableEq, Repr

namespace RetryMetadata

def toContract : RetryMetadata → String
  | .absent => "absent"
  | .nonIdempotent => "nonIdempotent"
  | .readOnly => "readOnly"
  | .idempotent => "idempotent"

def all : List RetryMetadata :=
  [ .absent, .nonIdempotent, .readOnly, .idempotent ]

/-- Whether the tool's declared metadata certifies that replaying an identical
invocation is safe even when the earlier attempt's effect may already have been
applied on the server. -/
def allowsReplay : RetryMetadata → Bool
  | .absent => false
  | .nonIdempotent => false
  | .readOnly => true
  | .idempotent => true

theorem all_complete (metadata : RetryMetadata) : metadata ∈ all := by
  cases metadata <;> simp [all]

end RetryMetadata

/-- Classify MCP `ToolAnnotations` (`readOnlyHint`, `idempotentHint`) into
`RetryMetadata`.

Fail-closed rules:
- An explicit `idempotentHint = false` always produces `.nonIdempotent`, even
  if `readOnlyHint = true` (contradictory metadata fails closed).
- `readOnlyHint = true` with `idempotentHint ≠ false` produces `.readOnly`.
- `idempotentHint = true` produces `.idempotent`.
- Absence of both positive hints (including `readOnlyHint = false` alone)
  produces `.absent`, keeping failures permanent by default. -/
def classifyAnnotations (readOnlyHint idempotentHint : HintValue) : RetryMetadata :=
  match readOnlyHint, idempotentHint with
  | _, .isFalse => .nonIdempotent
  | .isTrue, .absent => .readOnly
  | .isTrue, .isTrue => .readOnly
  | .absent, .isTrue => .idempotent
  | .isFalse, .isTrue => .idempotent
  | .absent, .absent => .absent
  | .isFalse, .absent => .absent

/-- Failure classes observed by `McpPool` during a `call_tool` invocation. -/
inductive CallFailure where
  /-- Transport dropped, reset, or closed during dispatch
  (`ServiceError::TransportSend` or `ServiceError::TransportClosed`). -/
  | transportDropped
  /-- Server refused handshake for authorization (HTTP 401/403). -/
  | authRefused
  /-- Connection failed before dispatch (connect error, connect timeout, or
  endpoint parked). -/
  | connectFailed
  /-- Server answered with an MCP/JSON-RPC error (`ServiceError::McpError` or
  `ServiceError::UnexpectedResponse`) or a tool-level error result. -/
  | protocolOrToolError
  /-- Tool call timed out (`ServiceError::Timeout`). -/
  | timedOut
  /-- Tool call was cancelled (`ServiceError::Cancelled`). -/
  | cancelled
  deriving DecidableEq, Repr

namespace CallFailure

def toContract : CallFailure → String
  | .transportDropped => "transportDropped"
  | .authRefused => "authRefused"
  | .connectFailed => "connectFailed"
  | .protocolOrToolError => "protocolOrToolError"
  | .timedOut => "timedOut"
  | .cancelled => "cancelled"

def all : List CallFailure :=
  [ .transportDropped
  , .authRefused
  , .connectFailed
  , .protocolOrToolError
  , .timedOut
  , .cancelled
  ]

theorem all_complete (failure : CallFailure) : failure ∈ all := by
  cases failure <;> simp [all]

/-- Whether the failure is a transient transport failure recoverable by
evicting the broken pooled connection and dialing a fresh MCP session. -/
def isTransientTransport : CallFailure → Bool
  | .transportDropped => true
  | .authRefused => false
  | .connectFailed => false
  | .protocolOrToolError => false
  | .timedOut => false
  | .cancelled => false

end CallFailure

/-- Whether the remote tool's effect may have been applied on the server before
the failure was observed. -/
inductive EffectStatus where
  /-- Failure happened before the tool call was sent on an established
  connection, so no tool effect was applied. -/
  | notApplied
  /-- The call was dispatched toward the server; whether its effect was applied
  is unknown or already happened. -/
  | mayHaveApplied
  deriving DecidableEq, Repr

/-- Effect status associated with each `CallFailure`.

External premise: `authRefused` and `connectFailed` occur during
`get_or_connect` before `call_tool` is transmitted; every failure during
`call_tool` dispatch leaves server-side execution uncertain (`mayHaveApplied`). -/
@[simp] def CallFailure.effectStatus : CallFailure → EffectStatus
  | .authRefused => .notApplied
  | .connectFailed => .notApplied
  | .transportDropped => .mayHaveApplied
  | .protocolOrToolError => .mayHaveApplied
  | .timedOut => .mayHaveApplied
  | .cancelled => .mayHaveApplied

/-- Default retry budget for retry-safe MCP `call_tool` invocations: at most
one retry on a fresh connection (two total dispatch attempts), matching
`list_tools` and the pool's single retry credit. -/
def defaultMaxRetries : Nat := 1

/-- Hard upper cap on retries per `call_tool` invocation so no caller or
configuration can cause unbounded retry loops. -/
def maxAllowedRetries : Nat := 3

/-- Effective retry budget for a call, bounded above by `maxAllowedRetries`. -/
def effectiveBudget (configuredMaxRetries : Nat) : Nat :=
  min configuredMaxRetries maxAllowedRetries

/-- Action taken by `McpPool` after a failed `call_tool` attempt. -/
inductive McpCallRetryDecision where
  /-- Propagate the failure as final without evicting or reconnecting. -/
  | failFinal
  /-- Evict the failed connection, dial a fresh connection, and replay the
  identical tool name and arguments. -/
  | evictAndRetry
  deriving DecidableEq, Repr

/-- Pure retry decision for a failed MCP `call_tool` attempt.

Replay premises:
1. Same-argument equivalence: when `metadata.allowsReplay = true`, repeating
   the call with the identical `service_id`, `tool_name`, and `arguments` has
   the same external effect as one call.
2. Fresh-session recovery: evicting the broken pooled connection and dialing a
   new MCP session recovers from a dropped connection or restarted server.
3. Route stability: retries target the exact resolved endpoint and bound
   `node_did` chosen before dispatch; if reconnection fails or is parked
   without retry credit, the retry stops with `connectFailed`/`authRefused`.
4. Bounded attempts: each retry increments `retriesUsed` by one and stops once
   `retriesUsed ≥ effectiveBudget maxRetries`. -/
def decideMcpCallRetry
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (retriesUsed : Nat)
    (maxRetries : Nat) : McpCallRetryDecision :=
  if metadata.allowsReplay && failure.isTransientTransport &&
      decide (retriesUsed < effectiveBudget maxRetries) then
    .evictAndRetry
  else
    .failFinal

private def runRetriesAux
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (maxRetries : Nat)
    (retriesUsed : Nat) : Nat → Nat
  | 0 => retriesUsed
  | fuel + 1 =>
      match decideMcpCallRetry metadata failure retriesUsed maxRetries with
      | .evictAndRetry => runRetriesAux metadata failure maxRetries (retriesUsed + 1) fuel
      | .failFinal => retriesUsed

/-- Total number of retries executed when every attempt fails with `failure`.
Fuel `maxAllowedRetries + 1` suffices because no retry occurs once
`retriesUsed ≥ maxAllowedRetries`. -/
def runRetries (metadata : RetryMetadata) (failure : CallFailure) (maxRetries : Nat) : Nat :=
  runRetriesAux metadata failure maxRetries 0 (maxAllowedRetries + 1)

/-- Whether the pool evicts the cached connection at least once for this
failure sequence. -/
def evictsConnection
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (maxRetries : Nat) : Bool :=
  decideMcpCallRetry metadata failure 0 maxRetries == .evictAndRetry

-- Annotation classification theorems

theorem default_annotations_not_retry_safe :
    (classifyAnnotations .absent .absent).allowsReplay = false := rfl

theorem explicit_non_idempotent_never_allows_replay (ro : HintValue) :
    (classifyAnnotations ro .isFalse).allowsReplay = false := by
  cases ro <;> rfl

theorem explicit_non_idempotent_overrides_read_only :
    classifyAnnotations .isTrue .isFalse = .nonIdempotent := rfl

theorem read_only_false_without_idempotent_not_retry_safe :
    (classifyAnnotations .isFalse .absent).allowsReplay = false := rfl

theorem idempotent_hint_allows_replay (ro : HintValue) :
    (classifyAnnotations ro .isTrue).allowsReplay = true := by
  cases ro <;> rfl

theorem read_only_without_false_idempotent_allows_replay :
    (classifyAnnotations .isTrue .absent).allowsReplay = true := rfl

-- Safety theorems on `decideMcpCallRetry`

theorem no_retry_without_safe_metadata
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (retriesUsed maxRetries : Nat)
    (h_unsafe : metadata.allowsReplay = false) :
    decideMcpCallRetry metadata failure retriesUsed maxRetries = .failFinal := by
  simp [decideMcpCallRetry, h_unsafe]

theorem absent_metadata_never_retries
    (failure : CallFailure)
    (retriesUsed maxRetries : Nat) :
    decideMcpCallRetry .absent failure retriesUsed maxRetries = .failFinal := by
  exact no_retry_without_safe_metadata .absent failure retriesUsed maxRetries rfl

theorem non_idempotent_never_retries
    (failure : CallFailure)
    (retriesUsed maxRetries : Nat) :
    decideMcpCallRetry .nonIdempotent failure retriesUsed maxRetries = .failFinal := by
  exact no_retry_without_safe_metadata .nonIdempotent failure retriesUsed maxRetries rfl

theorem absent_metadata_matches_operation_default
    (failure : CallFailure)
    (retriesUsed maxRetries : Nat) :
    decideMcpCallRetry .absent failure retriesUsed maxRetries = .failFinal ∧
    ToolExecution.retryDisposition .mcpCall = .doNotRetry := by
  exact ⟨absent_metadata_never_retries failure retriesUsed maxRetries, rfl⟩

theorem retry_implies_safe_transient_and_within_budget
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (retriesUsed maxRetries : Nat)
    (h_retry : decideMcpCallRetry metadata failure retriesUsed maxRetries = .evictAndRetry) :
    metadata.allowsReplay = true ∧
    failure.isTransientTransport = true ∧
    retriesUsed < effectiveBudget maxRetries := by
  unfold decideMcpCallRetry at h_retry
  split at h_retry
  · rename_i h_cond
    simp only [Bool.and_eq_true, decide_eq_true_eq] at h_cond
    exact ⟨h_cond.1.1, h_cond.1.2, h_cond.2⟩
  · cases h_retry

theorem uncertain_effect_retries_only_when_replay_safe
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (retriesUsed maxRetries : Nat)
    (_h_eff : failure.effectStatus = .mayHaveApplied)
    (h_retry : decideMcpCallRetry metadata failure retriesUsed maxRetries = .evictAndRetry) :
    metadata.allowsReplay = true :=
  (retry_implies_safe_transient_and_within_budget metadata failure retriesUsed maxRetries h_retry).1

theorem exhausted_budget_never_retries
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (retriesUsed maxRetries : Nat)
    (h_exhausted : retriesUsed ≥ effectiveBudget maxRetries) :
    decideMcpCallRetry metadata failure retriesUsed maxRetries = .failFinal := by
  unfold decideMcpCallRetry
  have h_not_lt : ¬ (retriesUsed < effectiveBudget maxRetries) := Nat.not_lt.mpr h_exhausted
  simp [h_not_lt]

theorem zero_budget_never_retries
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (retriesUsed : Nat) :
    decideMcpCallRetry metadata failure retriesUsed 0 = .failFinal := by
  apply exhausted_budget_never_retries
  simp [effectiveBudget]

theorem retry_never_exceeds_global_cap
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (retriesUsed maxRetries : Nat)
    (h_cap : retriesUsed ≥ maxAllowedRetries) :
    decideMcpCallRetry metadata failure retriesUsed maxRetries = .failFinal := by
  apply exhausted_budget_never_retries
  exact Nat.le_trans (Nat.min_le_right maxRetries maxAllowedRetries) h_cap

theorem auth_refusal_never_retries
    (metadata : RetryMetadata)
    (retriesUsed maxRetries : Nat) :
    decideMcpCallRetry metadata .authRefused retriesUsed maxRetries = .failFinal := by
  simp [decideMcpCallRetry, CallFailure.isTransientTransport]

theorem connect_failure_never_retries
    (metadata : RetryMetadata)
    (retriesUsed maxRetries : Nat) :
    decideMcpCallRetry metadata .connectFailed retriesUsed maxRetries = .failFinal := by
  simp [decideMcpCallRetry, CallFailure.isTransientTransport]

theorem protocol_or_tool_error_never_retries
    (metadata : RetryMetadata)
    (retriesUsed maxRetries : Nat) :
    decideMcpCallRetry metadata .protocolOrToolError retriesUsed maxRetries = .failFinal := by
  simp [decideMcpCallRetry, CallFailure.isTransientTransport]

theorem timeout_never_retries
    (metadata : RetryMetadata)
    (retriesUsed maxRetries : Nat) :
    decideMcpCallRetry metadata .timedOut retriesUsed maxRetries = .failFinal := by
  simp [decideMcpCallRetry, CallFailure.isTransientTransport]

theorem cancelled_never_retries
    (metadata : RetryMetadata)
    (retriesUsed maxRetries : Nat) :
    decideMcpCallRetry metadata .cancelled retriesUsed maxRetries = .failFinal := by
  simp [decideMcpCallRetry, CallFailure.isTransientTransport]

-- Boundedness theorems on multi-attempt `runRetries`

private theorem runRetriesAux_bounded
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (maxRetries : Nat) :
    ∀ (fuel retriesUsed : Nat),
      retriesUsed ≤ effectiveBudget maxRetries →
      runRetriesAux metadata failure maxRetries retriesUsed fuel ≤ effectiveBudget maxRetries
  | 0, retriesUsed, h_le => h_le
  | fuel + 1, retriesUsed, h_le => by
      simp only [runRetriesAux]
      cases h_dec : decideMcpCallRetry metadata failure retriesUsed maxRetries with
      | failFinal => exact h_le
      | evictAndRetry =>
          have h_lt : retriesUsed < effectiveBudget maxRetries :=
            (retry_implies_safe_transient_and_within_budget
              metadata failure retriesUsed maxRetries h_dec).2.2
          exact runRetriesAux_bounded metadata failure maxRetries fuel (retriesUsed + 1)
            (Nat.succ_le_of_lt h_lt)

theorem runRetries_bounded_by_effective_budget
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (maxRetries : Nat) :
    runRetries metadata failure maxRetries ≤ effectiveBudget maxRetries :=
  runRetriesAux_bounded metadata failure maxRetries (maxAllowedRetries + 1) 0 (Nat.zero_le _)

theorem runRetries_bounded_by_max_allowed
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (maxRetries : Nat) :
    runRetries metadata failure maxRetries ≤ maxAllowedRetries :=
  Nat.le_trans
    (runRetries_bounded_by_effective_budget metadata failure maxRetries)
    (Nat.min_le_right maxRetries maxAllowedRetries)

theorem runRetries_zero_when_unsafe
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (maxRetries : Nat)
    (h_unsafe : metadata.allowsReplay = false) :
    runRetries metadata failure maxRetries = 0 := by
  unfold runRetries
  simp [runRetriesAux, decideMcpCallRetry, h_unsafe, maxAllowedRetries]

theorem runRetries_zero_when_non_transient
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (maxRetries : Nat)
    (h_non_transient : failure.isTransientTransport = false) :
    runRetries metadata failure maxRetries = 0 := by
  unfold runRetries
  simp [runRetriesAux, decideMcpCallRetry, h_non_transient, maxAllowedRetries]

private theorem runRetriesAux_exact_safe_transport
    (metadata : RetryMetadata)
    (maxRetries : Nat)
    (h_safe : metadata.allowsReplay = true) :
    ∀ (fuel retriesUsed : Nat),
      retriesUsed ≤ effectiveBudget maxRetries →
      effectiveBudget maxRetries - retriesUsed ≤ fuel →
      runRetriesAux metadata .transportDropped maxRetries retriesUsed fuel =
        effectiveBudget maxRetries
  | 0, retriesUsed, h_le, h_fuel => by
      simp only [runRetriesAux]
      omega
  | fuel + 1, retriesUsed, h_le, h_fuel => by
      simp only [runRetriesAux]
      by_cases h_lt : retriesUsed < effectiveBudget maxRetries
      · have h_dec : decideMcpCallRetry metadata .transportDropped retriesUsed maxRetries =
            .evictAndRetry := by
          simp [decideMcpCallRetry, CallFailure.isTransientTransport, h_safe, h_lt]
        rw [h_dec]
        apply runRetriesAux_exact_safe_transport metadata maxRetries h_safe fuel (retriesUsed + 1)
        · omega
        · omega
      · have h_dec : decideMcpCallRetry metadata .transportDropped retriesUsed maxRetries =
            .failFinal := by
          simp [decideMcpCallRetry, h_lt]
        rw [h_dec]
        dsimp only
        omega

theorem runRetries_exact_for_safe_transport
    (metadata : RetryMetadata)
    (maxRetries : Nat)
    (h_safe : metadata.allowsReplay = true) :
    runRetries metadata .transportDropped maxRetries = effectiveBudget maxRetries := by
  apply runRetriesAux_exact_safe_transport metadata maxRetries h_safe (maxAllowedRetries + 1) 0
  · exact Nat.zero_le _
  · have h_cap : effectiveBudget maxRetries ≤ maxAllowedRetries :=
      Nat.min_le_right maxRetries maxAllowedRetries
    omega

theorem no_eviction_without_safe_metadata
    (metadata : RetryMetadata)
    (failure : CallFailure)
    (maxRetries : Nat)
    (h_unsafe : metadata.allowsReplay = false) :
    evictsConnection metadata failure maxRetries = false := by
  simp [evictsConnection, no_retry_without_safe_metadata metadata failure 0 maxRetries h_unsafe]

end McpRetry
end ToolExecution
