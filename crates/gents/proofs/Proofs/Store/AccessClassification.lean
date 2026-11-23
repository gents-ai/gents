/-! How the answer of an error owner travels through the error owners that wrap
it, for every classifier that finds an owner on an erased error's `source()`
chain: the access owner's classification (refused, transaction conflict), the
GraphQL owners' retryability, and the store's insecure key, lock contention and
incompatible store. -/

namespace AccessClassification

/-- What the access owner (`ConfigClientError`, and `GraphqlResponseError`
forwarding one) says about a failure. `refused` is a permission denial;
`conflict` is a transaction conflict the transaction owner may replay. Every
caller that reconnects, retries or reports a denial decides by this and by
nothing else. -/
inductive Classification where
  | refused | conflict | neither
  deriving DecidableEq, Repr

/-- The access failures the classification distinguishes: HTTP 401, HTTP 403,
a `PERMISSION_DENIED` GraphQL error code, a transaction conflict, and any
other failure. -/
inductive Failure where
  | unauthorized | forbidden | permissionDeniedCode | transactionConflict | other
  deriving DecidableEq, Repr

def Failure.classification : Failure → Classification
  | .unauthorized | .forbidden | .permissionDeniedCode => .refused
  | .transactionConflict => .conflict
  | .other => .neither

/-- How an owner variant holds the error it wraps. A `sourced` variant has a
message of its own and returns the wrapped error from `source()`. A `boxed`
variant is sourced through a `Box` of the wrapped error: `source()` returns the
box, which is a type of its own on the chain and answers `source()` as the
error in it does. A `transparent` variant (`#[error(transparent)]`) forwards
`source()` to the wrapped error's own source, so the wrapped error is not on
the chain; an `anyhow::Error` or a boxed `dyn Error` held transparently loses
the error at its top the same way. -/
inductive Holding where
  | sourced | boxed | transparent
  deriving DecidableEq, Repr

/-- An error as its receiver holds it, around the owner a classifier looks
for; `α` is what that owner answers when asked. Application context (`anyhow`)
holds what it wraps as a `sourced` wrapper does. -/
inductive Chain (α : Type) where
  | owner (answer : α)
  | wrapper (holding : Holding) (inner : Chain α)
  deriving Repr

/-- How a classifier recognises the owner on the chain. `exact` downcasts to
the owner's type; `ownerOrBox` (`gents_protocol::error_chain::as_owner`) also
downcasts to a box of it. Decision 2026-11-13: every classifier that searches
a chain for an owner asks through `as_owner` (and `find_source`, which uses
it), so a boxed `#[source]` is found without the classifier naming the box. -/
inductive Search where
  | exact | ownerOrBox
  deriving DecidableEq, Repr

namespace Chain

variable {α : Type}

def answer : Chain α → α
  | .owner answer => answer
  | .wrapper _ inner => inner.answer

/-- The errors `source()` reaches from `error`, nearest first, each marked
with whether it is on the chain as the box that holds it. -/
def sources : Chain α → List (Chain α × Bool)
  | .owner _ => []
  | .wrapper .sourced inner => (inner, false) :: inner.sources
  | .wrapper .boxed inner => (inner, true) :: inner.sources
  | .wrapper .transparent inner => inner.sources

/-- The owner's answer, when the chain element is the owner and the search
recognises it as it sits there. A wrapper is never asked: the receiver cannot
name the owners above the one it classifies by. -/
def asOwner (search : Search) : Chain α × Bool → Option α
  | (.owner answer, false) => some answer
  | (.owner answer, true) => if search = .ownerOrBox then some answer else none
  | (.wrapper _ _, _) => none

/-- What a receiver of the erased error finds: the first owner on the
`source()` chain, starting with the error itself. -/
def found (search : Search) (error : Chain α) : Option α :=
  ((error, false) :: error.sources).findSome? (asOwner search)

/-- Whether the variant that holds the owner is sourced, directly or through a
box. Decision 2026-11-13: every owner variant that holds an access error, a
GraphQL failure or an erased error (`anyhow`, a boxed `dyn Error`) directly is
sourced, so the owner rides one level below any transparent variant above it,
and a transparent variant hides only the error it wraps (the premise
`Hooks.rerunSurvives` records for a rerun request). The classifiers therefore
need no list of the owners above the one they ask. A sourced variant that keeps
the wrapped error's text as its own message shows that text once for itself
and once for its source wherever the whole chain is printed;
`gents_protocol::error_chain::ErrorChain` prints such an adjacent repeat once.
The alternative, a message of its own, would drop the wrapped error's text
wherever only the outermost message is shown. -/
def exposed : Chain α → Bool
  | .owner _ => true
  | .wrapper .sourced inner => inner.exposed
  | .wrapper .boxed inner => inner.exposed
  | .wrapper .transparent (.owner _) => false
  | .wrapper .transparent (.wrapper holding inner) => (wrapper holding inner).exposed

theorem asOwner_box (element : Chain α) :
    asOwner .ownerOrBox (element, true) = asOwner .ownerOrBox (element, false) := by
  cases element <;> rfl

theorem found_of_wrapper (search : Search) (holding : Holding) (inner : Chain α) :
    (wrapper holding inner).found search = (wrapper holding inner).sources.findSome? (asOwner search) := by
  simp [found, asOwner]

theorem found_eq (error : Chain α) :
    error.found .ownerOrBox = if error.exposed then some error.answer else none := by
  induction error with
  | owner answer => simp [found, sources, asOwner, exposed, Chain.answer]
  | wrapper holding inner ih =>
    cases holding with
    | sourced =>
      have step : (wrapper .sourced inner).found .ownerOrBox = inner.found .ownerOrBox := by
        simp [found, sources, asOwner]
      rw [step, ih]
      simp [exposed, Chain.answer]
    | boxed =>
      have step : (wrapper .boxed inner).found .ownerOrBox = inner.found .ownerOrBox := by
        cases inner <;> rfl
      rw [step, ih]
      simp [exposed, Chain.answer]
    | transparent =>
      cases inner with
      | owner answer => simp [found, sources, asOwner, exposed]
      | wrapper holding inner =>
        have step : (wrapper .transparent (wrapper holding inner)).found .ownerOrBox
            = (wrapper holding inner).found .ownerOrBox := by
          rw [found_of_wrapper, found_of_wrapper]
          rfl
        rw [step, ih]
        simp [exposed, Chain.answer]

/-- An owner held by a transparent variant is lost to every receiver of the
erased error. -/
theorem hidden_is_not_found (error : Chain α) (hidden : error.exposed = false) :
    error.found .ownerOrBox = none := by
  simp [found_eq, hidden]

/-- A box is a type of its own on the chain: a classifier that downcasts to
the owner's type alone does not find an owner held through a box. -/
theorem boxed_owner_is_lost_to_an_exact_search (answer : α) :
    (wrapper .boxed (owner answer)).found .exact = none := rfl

/-- `holdings` applied to `error`, outermost first. -/
def wrapAll (holdings : List Holding) (error : Chain α) : Chain α :=
  holdings.foldr wrapper error

theorem wrapAll_answer (holdings : List Holding) (error : Chain α) :
    (wrapAll holdings error).answer = error.answer := by
  induction holdings with
  | nil => rfl
  | cons holding rest ih => simpa [wrapAll, Chain.answer] using ih

theorem held_owner_is_exposed_through_every_wrapping (holdings : List Holding)
    (holding : Holding) (held : holding ≠ .transparent) (answer : α) :
    (wrapAll holdings (wrapper holding (owner answer))).exposed = true := by
  induction holdings with
  | nil => cases holding <;> simp_all [wrapAll, exposed]
  | cons outer rest ih =>
    cases outer with
    | sourced => simpa [wrapAll, exposed] using ih
    | boxed => simpa [wrapAll, exposed] using ih
    | transparent =>
      cases rest with
      | nil => cases holding <;> simp_all [wrapAll, exposed]
      | cons next rest => simpa [wrapAll, exposed] using ih

/-- Totality: whatever owners wrap a sourced or boxed holder of an owner, and
however they hold what they wrap, the receiver of the erased error finds the
owner's answer. -/
theorem found_survives_every_wrapping (holdings : List Holding) (holding : Holding)
    (held : holding ≠ .transparent) (answer : α) :
    (wrapAll holdings (wrapper holding (owner answer))).found .ownerOrBox = some answer := by
  rw [found_eq, held_owner_is_exposed_through_every_wrapping _ _ held]
  simp [wrapAll_answer, Chain.answer]

end Chain

/-- How a classifier receives the error: by its owner's type, or erased into
an application error (`anyhow`) or a boxed `std::error::Error`. -/
inductive Erasure where
  | typed | anyhow | boxed
  deriving DecidableEq, Repr

/-! ## The access owner's classification -/

/-- An error around the access error. The two access owners wrap each other
and answer for each other by type, so together they are the one owner. -/
abbrev Error := Chain Failure

namespace Error

/-- The classification a typed owner gives: each owner matches its own
variants and asks the error it holds. -/
def typed (error : Error) : Classification := (Chain.answer error).classification

/-- The classification of an erased (`anyhow` or boxed) error: the access
owner's classification of the access error found on the chain
(`retry::access_error_in`). An error with no access error on its chain is not a
denial and not a conflict. -/
def erased (error : Error) : Classification :=
  match Chain.found .ownerOrBox error with
  | some failure => failure.classification
  | none => .neither

def classify (error : Error) : Erasure → Classification
  | .typed => typed error
  | .anyhow | .boxed => erased error

/-- Wrapping preserves the classification: an erased error whose access error
is held by a sourced variant classifies exactly as its typed owner does. -/
theorem exposed_classifies_as_typed (error : Error) (held : Chain.exposed error = true) :
    erased error = typed error := by
  simp [erased, typed, Chain.found_eq, held]

/-- An access error held by a transparent variant is lost to every receiver of
the erased error: a refusal reads as an outage and a conflict is not replayed. -/
theorem hidden_is_unclassified (error : Error) (hidden : Chain.exposed error = false) :
    erased error = .neither := by
  simp [erased, Chain.found_eq, hidden]

theorem transparent_holder_loses_a_refusal :
    erased (.wrapper .transparent (.owner .forbidden)) = .neither
      ∧ typed (.wrapper .transparent (.owner .forbidden)) = .refused := ⟨rfl, rfl⟩

theorem held_classification_survives_every_wrapping (holdings : List Holding)
    (holding : Holding) (held : holding ≠ .transparent) (failure : Failure) :
    erased (Chain.wrapAll holdings (.wrapper holding (.owner failure))) = failure.classification := by
  simp [erased, Chain.found_survives_every_wrapping _ _ held]

/-- Totality: whatever owners wrap a sourced holder of an access error, and
however they hold what they wrap, the erased error classifies as the access
owner classified the failure. -/
theorem classification_survives_every_wrapping (holdings : List Holding) (failure : Failure) :
    erased (Chain.wrapAll holdings (.wrapper .sourced (.owner failure))) = failure.classification :=
  held_classification_survives_every_wrapping holdings .sourced (by decide) failure

/-- The classification of `failure` held directly by a variant holding it as
`holding`, received as `erasure`. -/
def held (holding : Holding) (failure : Failure) (erasure : Erasure) : Classification :=
  classify (.wrapper holding (.owner failure)) erasure

theorem held_preserves_classification (holding : Holding) (sourced : holding ≠ .transparent)
    (failure : Failure) (erasure : Erasure) : held holding failure erasure = failure.classification := by
  cases erasure
  · rfl
  all_goals exact held_classification_survives_every_wrapping [] holding sourced failure

end Error

/-! ## Every owner variant that holds an access error, by crate

Each crate's family enumerates the variants of that crate's error owners that
hold `ConfigClientError`, `GraphqlResponseError` or the store's `ViewError`, named
`<owner><variant>`. Native fence: `write_owner_structure`'s
`access_error_holders_are_sourced` reads every crate's sources and fails when a
variant holding an access error, a GraphQL failure or an erased error is
transparent, or when a holder of an access error is missing from its crate's
family. Each family has one consumer, in its crate. -/

/-- `gents-store`. -/
inductive StoreWrapper where
  | viewErrorExecute
  | packEvidenceErrorConfig
  | documentDecideErrorAuthorize
  | deferredOperationErrorConfig
  | schemaInstallErrorConfigClient
  | agentSelfConfigPermissionErrorAccess
  | receiptWriteErrorWrite
  | runtimeHopCapErrorAccess
  | agentProvisionErrorAccess
  | agentAdmissionObservationErrorAccess
  | remoteAgentProvisionErrorAuthorization
  | runtimeSchemaErrorConfig
  | authoredDocumentCreateErrorPublication
  | authoredDocumentCreateErrorUnconfirmed
  | enrollmentApprovalErrorNodeRead
  | enrollmentGrantErrorRead
  | enrollmentGrantErrorObserve
  | enrollmentGrantErrorRecord
  | enrollmentGrantErrorIssue
  | enrollmentGrantErrorIssueNotAbandoned
  | enrollmentGrantErrorRevoke
  | enrollmentGrantErrorRetire
  | grantSetErrorGrant
  | grantSetErrorRollback
  | grantSetErrorRevoke
  | legacyBackendPreflightErrorQuery
  | journalSchemaErrorAccess
  | outboxErrorAccess
  | outboxErrorResponse
  | receiverErrorStore
  | receiverErrorResponse
  | clientAccessErrorAccess
  | runtimeAccessErrorConfiguration
  | ownGrantErrorUnanswered
  deriving DecidableEq, Repr

def StoreWrapper.holding : StoreWrapper → Holding
  | .packEvidenceErrorConfig | .grantSetErrorGrant | .grantSetErrorRollback
  | .grantSetErrorRevoke => .boxed
  | _ => .sourced

def StoreWrapper.wrap (wrapper : StoreWrapper) (failure : Failure) : Error :=
  .wrapper wrapper.holding (.owner failure)

/-- `gents-inference`. `inventoryErrorView` and `usageRefusalErrorRead` hold
the store's `ViewError` transparently, which is sound because `ViewError`
holds the access error as a sourced variant. -/
inductive InferenceWrapper where
  | inventoryErrorStore
  | inventoryErrorView
  | runtimeAccountMetadataErrorAccess
  | configurationVisibilityErrorVersion
  | configurationVisibilityErrorAccess
  | usageRefusalErrorRead
  deriving DecidableEq, Repr

def InferenceWrapper.wrap : InferenceWrapper → Failure → Error
  | .inventoryErrorView, failure | .usageRefusalErrorRead, failure =>
      .wrapper .transparent (StoreWrapper.viewErrorExecute.wrap failure)
  | _, failure => .wrapper .sourced (.owner failure)

/-- `gents-session`. -/
inductive SessionWrapper where
  | commitErrorAccess
  | sessionControlErrorAccess
  | redriveErrorStore
  | hookErrorStore
  | userMessageInputErrorRead
  | watcherScanErrorQuery
  | positionErrorAccess
  | renewalErrorAccess
  deriving DecidableEq, Repr

def SessionWrapper.holding (_ : SessionWrapper) : Holding := .sourced

/-- `gents-p2p`. -/
inductive P2pWrapper where
  | endpointErrorAccess
  | pairingReadErrorAccess
  | appCollectionAdmissionErrorUnreadable
  deriving DecidableEq, Repr

def P2pWrapper.holding (_ : P2pWrapper) : Holding := .sourced

/-- `gents-service`. -/
inductive ServiceWrapper where
  | controlFailureAuthorization
  deriving DecidableEq, Repr

def ServiceWrapper.holding : ServiceWrapper → Holding
  | .controlFailureAuthorization => .boxed

/-- `gents-tools`. `registryReadErrorView` holds the store's `ViewError`
transparently, as `InferenceWrapper.inventoryErrorView` does. -/
inductive ToolsWrapper where
  | schemaIdentityErrorConfigClient
  | registryReadErrorView
  | queryCommandErrorAccess
  | titleUsageErrorRead
  deriving DecidableEq, Repr

def ToolsWrapper.wrap : ToolsWrapper → Failure → Error
  | .registryReadErrorView, failure => .wrapper .transparent (StoreWrapper.viewErrorExecute.wrap failure)
  | .schemaIdentityErrorConfigClient, failure => .wrapper .sourced (.owner failure)
  | .queryCommandErrorAccess, failure => .wrapper .sourced (.owner failure)
  | .titleUsageErrorRead, failure => .wrapper .sourced (.owner failure)

/-- `gents-automation`. -/
inductive AutomationWrapper where
  | goalApplicationErrorAccess
  | goalApplicationErrorView
  deriving DecidableEq, Repr

def AutomationWrapper.holding (_ : AutomationWrapper) : Holding := .sourced

/-- `gents-desktop-bridge`. -/
inductive DesktopBridgeWrapper where
  | localHomeAccessErrorSigning
  deriving DecidableEq, Repr

def DesktopBridgeWrapper.holding (_ : DesktopBridgeWrapper) : Holding := .sourced

/-- `gents-desktop-core`. -/
inductive DesktopCoreWrapper where
  | journalErrorAccess
  deriving DecidableEq, Repr

def DesktopCoreWrapper.holding (_ : DesktopCoreWrapper) : Holding := .sourced

/-- `gents-eval`. -/
inductive EvalWrapper where
  | promoteTxnErrorAccess
  deriving DecidableEq, Repr

def EvalWrapper.holding (_ : EvalWrapper) : Holding := .sourced

/-- `gents-toolcalls`. -/
inductive ToolcallsWrapper where
  | terminalWriteErrorAccess
  | writeFailureAccess
  deriving DecidableEq, Repr

def ToolcallsWrapper.holding (_ : ToolcallsWrapper) : Holding := .sourced

/-- The classification of `failure` held by a store owner variant. -/
def storeWrapped (wrapper : StoreWrapper) (failure : Failure) (erasure : Erasure) :
    Classification :=
  (wrapper.wrap failure).classify erasure

/-- The classification of `failure` held by an inference owner variant. -/
def inferenceWrapped (wrapper : InferenceWrapper) (failure : Failure) (erasure : Erasure) :
    Classification :=
  (wrapper.wrap failure).classify erasure

def sessionWrapped (wrapper : SessionWrapper) (failure : Failure) (erasure : Erasure) :
    Classification :=
  Error.held wrapper.holding failure erasure

def p2pWrapped (wrapper : P2pWrapper) (failure : Failure) (erasure : Erasure) : Classification :=
  Error.held wrapper.holding failure erasure

def serviceWrapped (wrapper : ServiceWrapper) (failure : Failure) (erasure : Erasure) :
    Classification :=
  Error.held wrapper.holding failure erasure

def toolsWrapped (wrapper : ToolsWrapper) (failure : Failure) (erasure : Erasure) :
    Classification :=
  (wrapper.wrap failure).classify erasure

def automationWrapped (wrapper : AutomationWrapper) (failure : Failure) (erasure : Erasure) :
    Classification :=
  Error.held wrapper.holding failure erasure

def desktopBridgeWrapped (wrapper : DesktopBridgeWrapper) (failure : Failure) (erasure : Erasure) :
    Classification :=
  Error.held wrapper.holding failure erasure

def desktopCoreWrapped (wrapper : DesktopCoreWrapper) (failure : Failure) (erasure : Erasure) :
    Classification :=
  Error.held wrapper.holding failure erasure

def evalWrapped (wrapper : EvalWrapper) (failure : Failure) (erasure : Erasure) :
    Classification :=
  Error.held wrapper.holding failure erasure

def toolcallsWrapped (wrapper : ToolcallsWrapper) (failure : Failure) (erasure : Erasure) :
    Classification :=
  Error.held wrapper.holding failure erasure

theorem eval_wrapping_preserves_classification (wrapper : EvalWrapper) (failure : Failure)
    (erasure : Erasure) : evalWrapped wrapper failure erasure = failure.classification :=
  Error.held_preserves_classification wrapper.holding (by cases wrapper <;> decide) failure erasure

theorem toolcalls_wrapping_preserves_classification (wrapper : ToolcallsWrapper)
    (failure : Failure) (erasure : Erasure) :
    toolcallsWrapped wrapper failure erasure = failure.classification :=
  Error.held_preserves_classification wrapper.holding (by cases wrapper <;> decide) failure erasure

theorem store_wrapping_preserves_classification (wrapper : StoreWrapper) (failure : Failure)
    (erasure : Erasure) : storeWrapped wrapper failure erasure = failure.classification :=
  Error.held_preserves_classification wrapper.holding (by cases wrapper <;> decide) failure erasure

theorem inference_wrapping_preserves_classification (wrapper : InferenceWrapper)
    (failure : Failure) (erasure : Erasure) :
    inferenceWrapped wrapper failure erasure = failure.classification := by
  cases wrapper <;> cases erasure <;> cases failure <;> rfl

theorem session_wrapping_preserves_classification (wrapper : SessionWrapper) (failure : Failure)
    (erasure : Erasure) : sessionWrapped wrapper failure erasure = failure.classification :=
  Error.held_preserves_classification wrapper.holding (by cases wrapper <;> decide) failure erasure

theorem p2p_wrapping_preserves_classification (wrapper : P2pWrapper) (failure : Failure)
    (erasure : Erasure) : p2pWrapped wrapper failure erasure = failure.classification :=
  Error.held_preserves_classification wrapper.holding (by cases wrapper <;> decide) failure erasure

theorem service_wrapping_preserves_classification (wrapper : ServiceWrapper) (failure : Failure)
    (erasure : Erasure) : serviceWrapped wrapper failure erasure = failure.classification :=
  Error.held_preserves_classification wrapper.holding (by cases wrapper <;> decide) failure erasure

theorem tools_wrapping_preserves_classification (wrapper : ToolsWrapper) (failure : Failure)
    (erasure : Erasure) : toolsWrapped wrapper failure erasure = failure.classification := by
  cases wrapper <;> cases erasure <;> cases failure <;> rfl

theorem automation_wrapping_preserves_classification (wrapper : AutomationWrapper)
    (failure : Failure) (erasure : Erasure) :
    automationWrapped wrapper failure erasure = failure.classification :=
  Error.held_preserves_classification wrapper.holding (by cases wrapper <;> decide) failure erasure

theorem desktop_bridge_wrapping_preserves_classification (wrapper : DesktopBridgeWrapper)
    (failure : Failure) (erasure : Erasure) :
    desktopBridgeWrapped wrapper failure erasure = failure.classification :=
  Error.held_preserves_classification wrapper.holding (by cases wrapper <;> decide) failure erasure

theorem desktop_core_wrapping_preserves_classification (wrapper : DesktopCoreWrapper)
    (failure : Failure) (erasure : Erasure) :
    desktopCoreWrapped wrapper failure erasure = failure.classification :=
  Error.held_preserves_classification wrapper.holding (by cases wrapper <;> decide) failure erasure

/-- A failure read through a typed view (`gents_store::views::ViewError`) and
erased by the caller, as every client snapshot read receives it. -/
def viewRead (failure : Failure) : Error :=
  .wrapper .sourced (StoreWrapper.viewErrorExecute.wrap failure)

theorem view_read_classifies_as_the_access_owner (failure : Failure) :
    (viewRead failure).erased = failure.classification := by
  cases failure <;> rfl

/-! ## Retryability

`gents_protocol::graphql::graphql_error_is_retryable` finds a database failure
DefraDB coded retryable as a `GraphqlFailure` on the chain, or asks a
`GraphqlError` that holds one. It is the same search as the access owner's,
over the GraphQL owners: retryability is found wherever a sourced variant
holds the failure. -/

/-- Whether a receiver of `error` finds a retryable database failure. -/
def retryableFound (error : Chain Bool) : Bool := (error.found .ownerOrBox).getD false

/-- `graphql_error_is_retryable` searches the chain from the error it is
given, so the typed error is searched as the erased one is. -/
def retryableAs (error : Chain Bool) : Erasure → Bool
  | .typed | .anyhow | .boxed => retryableFound error

theorem retryability_survives_every_wrapping (holdings : List Holding) (holding : Holding)
    (held : holding ≠ .transparent) (retryable : Bool) :
    retryableFound (Chain.wrapAll holdings (.wrapper holding (.owner retryable))) = retryable := by
  simp [retryableFound, Chain.found_survives_every_wrapping _ _ held]

theorem transparent_holder_loses_retryability :
    retryableFound (.wrapper .transparent (.owner true)) = false := rfl

/-- The variants of `gents-store` owners other than the GraphQL owners that
hold a `GraphqlError` or a `GraphqlFailure`. -/
inductive StoreRetryWrapper where
  | configClientErrorProtocol
  | configClientErrorDatabase
  | graphqlResponseErrorDatabase
  | deferredOperationErrorGraphql
  | enrollmentGrantErrorRecordUnconfirmed
  deriving DecidableEq, Repr

/-- The same, in `gents-protocol`. -/
inductive ProtocolRetryWrapper where
  | canonicalRowErrorGraphql
  deriving DecidableEq, Repr

def StoreRetryWrapper.holding (_ : StoreRetryWrapper) : Holding := .sourced

def ProtocolRetryWrapper.holding (_ : ProtocolRetryWrapper) : Holding := .sourced

def storeRetryWrapped (wrapper : StoreRetryWrapper) (retryable : Bool) (erasure : Erasure) : Bool :=
  retryableAs (.wrapper wrapper.holding (.owner retryable)) erasure

def protocolRetryWrapped (wrapper : ProtocolRetryWrapper) (retryable : Bool) (erasure : Erasure) :
    Bool :=
  retryableAs (.wrapper wrapper.holding (.owner retryable)) erasure

theorem store_retry_wrapping_preserves_retryability (wrapper : StoreRetryWrapper)
    (retryable : Bool) (erasure : Erasure) : storeRetryWrapped wrapper retryable erasure = retryable := by
  cases erasure <;> exact retryability_survives_every_wrapping [] .sourced (by decide) retryable

theorem protocol_retry_wrapping_preserves_retryability (wrapper : ProtocolRetryWrapper)
    (retryable : Bool) (erasure : Erasure) :
    protocolRetryWrapped wrapper retryable erasure = retryable := by
  cases erasure <;> exact retryability_survives_every_wrapping [] .sourced (by decide) retryable

/-! ## The store's local owners

`find_insecure_key` asks `IdentityError`, `store_lock_held` asks `HomeError`
and `StoreKeyError`, and `find_incompatible` asks `StoreKeyError`; each owner
answers from the variant that holds the leaf. Those owners are also held
through a `Box` (`IdentityError`'s and `StoreKeyError`'s keychain variants,
`HomeError`'s archive and delete variants, `PackEvidenceError::Identity`), and
`StoreCompatibilityError` holds `HomeError` and `StoreKeyError`. -/

/-- What a store classifier answers: an insecure key file, a held store lock,
an incompatible store. -/
inductive OwnerProbe where
  | insecureKey | lockHeld | incompatible
  deriving DecidableEq, Repr

inductive StoreOwnerWrapper where
  | identityErrorLoadSigningIdentity
  | identityErrorStoreKeychainIdentity
  | identityErrorSecureEnclavePublicKey
  | identityErrorSecureEnclaveDid
  | storeKeyErrorKeychainStore
  | storeKeyErrorKeychainDelete
  | packEvidenceErrorIdentity
  | homeErrorArchiveRolledBack
  | homeErrorArchiveUnrestored
  | homeErrorDeleteStopped
  | storeCompatibilityErrorHome
  | storeCompatibilityErrorKey
  deriving DecidableEq, Repr

def StoreOwnerWrapper.holding : StoreOwnerWrapper → Holding
  | .storeCompatibilityErrorHome | .storeCompatibilityErrorKey => .sourced
  | _ => .boxed

/-- The leaf the native case puts under the owner the variant holds. -/
def StoreOwnerWrapper.probe : StoreOwnerWrapper → OwnerProbe
  | .homeErrorArchiveRolledBack | .homeErrorArchiveUnrestored | .homeErrorDeleteStopped
  | .storeCompatibilityErrorHome => .lockHeld
  | .storeCompatibilityErrorKey => .incompatible
  | _ => .insecureKey

/-- The store classifier that answers for the error the variant holds; each
classifier searches the chain from the error it is given. -/
def storeOwnerFound (wrapper : StoreOwnerWrapper) (erasure : Erasure) : Option OwnerProbe :=
  match erasure with
  | .typed | .anyhow | .boxed => (Chain.wrapper wrapper.holding (.owner wrapper.probe)).found .ownerOrBox

theorem store_owner_is_found (wrapper : StoreOwnerWrapper) (erasure : Erasure) :
    storeOwnerFound wrapper erasure = some wrapper.probe := by
  cases wrapper <;> cases erasure <;> rfl

theorem boxed_store_owner_is_lost_to_an_exact_search :
    (Chain.wrapper StoreOwnerWrapper.identityErrorLoadSigningIdentity.holding
      (.owner OwnerProbe.insecureKey)).found .exact = none := rfl

/-! ## A passthrough: classified through its source, printed once

An operation owner's `Passthrough` variant (`gents_protocol::operations`) passes
another owner's failure through unchanged. It holds
`gents_protocol::error_chain::PassedThrough` transparently, and `PassedThrough`
returns the passed-through error from `source()`, so the variant is a
transparent holder of a sourced holder (`passthrough`): the passed-through error
is on the chain and every classifier finds it. An application error converted
into a box instead shows its top error's message and forwards `source()` past
it (`hiddenPassthrough`), so that top error was never found.

Both holders keep the passed-through error's text as their own message, so a
printer of the whole chain meets that text twice in a row. The printers of the
whole chain (`error_chain::ErrorChain`, and `error_chain::ErrorReport` for the
CLI's `Error:` and `Caused by:` report) print a message that repeats the one
just printed once (`collapse`). Decision 2026-11-16: the printer removes the
repeat; a holder with a message of its own would drop the passed-through text
wherever only the outermost message is shown. -/

namespace Chain

variable {α : Type}

/-- An operation owner's `Passthrough` variant around the error it passes. -/
def passthrough (inner : Chain α) : Chain α := .wrapper .transparent (.wrapper .sourced inner)

/-- The same variant holding a box of an application error, before
`PassedThrough`. -/
def hiddenPassthrough (inner : Chain α) : Chain α :=
  .wrapper .transparent (.wrapper .transparent inner)

theorem wrapAll_append (outer inner : List Holding) (error : Chain α) :
    wrapAll (outer ++ inner) error = wrapAll outer (wrapAll inner error) := by
  simp [wrapAll, List.foldr_append]

/-- Whatever owners wrap a passthrough, the error it passes is found. -/
theorem passthrough_is_found (holdings : List Holding) (answer : α) :
    (wrapAll holdings (passthrough (owner answer))).found .ownerOrBox = some answer := by
  have shape : wrapAll holdings (passthrough (owner answer))
      = wrapAll (holdings ++ [.transparent]) (wrapper .sourced (owner answer)) := by
    rw [wrapAll_append]
    rfl
  rw [shape]
  exact found_survives_every_wrapping _ .sourced (by decide) answer

theorem hidden_passthrough_is_not_found (answer : α) :
    (hiddenPassthrough (owner answer)).found .ownerOrBox = none := rfl

end Chain

/-- The messages a printer of the whole chain prints, from the messages it
meets, outermost first: a message that repeats the one just printed is printed
once. Messages are named by number; a message repeats another when it equals
it or is its first line, as for a report that adds lines below the error it
reports (`Next: ...`), so both carry one number here. -/
def collapse : List Nat → List Nat
  | [] => []
  | first :: rest => first :: collapseAfter first rest
where
  collapseAfter (previous : Nat) : List Nat → List Nat
    | [] => []
    | next :: rest =>
      if next = previous then collapseAfter previous rest else next :: collapseAfter next rest

/-- The messages a printer meets on a holder that keeps the held error's text
as its own message (`#[error(transparent)]`, `#[error(fmt = error_chain::held)]`,
`PassedThrough`), given those it meets on the held error. A transparent holder
displays as the held error and forwards `source()` past it, so it takes the
held error's place; a sourced or boxed holder is met above the held error. -/
def Holding.print : Holding → List Nat → List Nat
  | .transparent, messages => messages
  | _, [] => []
  | _, message :: rest => message :: message :: rest

theorem collapse_print (holding : Holding) (messages : List Nat) :
    collapse (holding.print messages) = collapse messages := by
  cases holding <;> cases messages <;>
    simp [Holding.print, collapse, collapse.collapseAfter]

/-- Holders that keep the held error's text add nothing to what a printer of
the whole chain prints. -/
theorem kept_text_is_printed_once (holdings : List Holding) (messages : List Nat) :
    collapse (holdings.foldr Holding.print messages) = collapse messages := by
  induction holdings with
  | nil => rfl
  | cons holding rest ih => simp [List.foldr, collapse_print, ih]

/-- How a passthrough is printed: through a printer of the whole chain, which
prints a repeat once, or by joining every message, as anyhow's `{:#}` and
`{:?}` do. -/
inductive Printer where
  | errorChain | errorReport | joined
  deriving DecidableEq, Repr

/-- The messages `printer` prints for a passthrough of an error with message
`0` caused by an error with message `1`. -/
def Printer.printPassthrough (printer : Printer) : List Nat :=
  let met := Holding.transparent.print (Holding.sourced.print [0, 1])
  match printer with
  | .errorChain | .errorReport => collapse met
  | .joined => met

theorem passthrough_is_printed_once :
    Printer.errorChain.printPassthrough = [0, 1]
      ∧ Printer.errorReport.printPassthrough = [0, 1] := ⟨rfl, rfl⟩

/-- Joining every message prints the passed-through text twice. -/
theorem joined_passthrough_repeats : Printer.joined.printPassthrough = [0, 0, 1] := rfl

/-- Every operation owner with a `Passthrough` variant, named
`<owner>Passthrough`. Native fence: `write_owner_structure`'s
`passthrough_variants_keep_their_source` fails when a `Passthrough` variant
holds a boxed error, or when the owners holding `PassedThrough` differ from
this. -/
inductive PassthroughWrapper where
  | agentOperationErrorPassthrough
  | documentOperationErrorPassthrough
  | documentDeleteErrorPassthrough
  | goalOperationErrorPassthrough
  | inferenceOperationErrorPassthrough
  | mailboxOperationErrorPassthrough
  | registryErrorPassthrough
  | applicationPairingErrorPassthrough
  | packOperationErrorPassthrough
  | agentPermissionErrorPassthrough
  | requestOperationErrorPassthrough
  | sessionOperationErrorPassthrough
  | taskOperationErrorPassthrough
  | workspaceRootErrorPassthrough
  deriving DecidableEq, Repr

/-- The classification of an access failure an operation owner passes through,
received as `erasure`. -/
def passthroughWrapped (_ : PassthroughWrapper) (failure : Failure) (erasure : Erasure) :
    Classification :=
  Error.classify (Chain.passthrough (.owner failure)) erasure

/-- How many times `printer` prints the text an operation owner passes
through. -/
def passthroughPrinted (_ : PassthroughWrapper) (printer : Printer) : Nat :=
  printer.printPassthrough.count 0

theorem passthrough_wrapping_preserves_classification (wrapper : PassthroughWrapper)
    (failure : Failure) (erasure : Erasure) :
    passthroughWrapped wrapper failure erasure = failure.classification := by
  cases erasure <;> cases failure <;> rfl

theorem passthrough_text_is_printed_once (wrapper : PassthroughWrapper) :
    passthroughPrinted wrapper .errorChain = 1 ∧ passthroughPrinted wrapper .errorReport = 1 :=
  ⟨rfl, rfl⟩

end AccessClassification
