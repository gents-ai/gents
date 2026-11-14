/-! How the access owner's classification of a failure travels through the
error owners that wrap it. -/

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
message of its own and returns the wrapped error from `source()`. A
`transparent` variant (`#[error(transparent)]`) forwards `source()` to the
wrapped error's own source, so the wrapped error is not on the chain. -/
inductive Holding where
  | sourced | transparent
  deriving DecidableEq, Repr

/-- An error as its receiver holds it. The two access owners wrap each other
and answer for each other by type, so together they are the one `access`
error. Application context (`anyhow`) holds what it wraps as a `sourced`
wrapper does. -/
inductive Error where
  | access (failure : Failure)
  | wrapper (holding : Holding) (inner : Error)
  deriving Repr

namespace Error

def failure : Error → Failure
  | .access failure => failure
  | .wrapper _ inner => inner.failure

/-- The classification a typed owner gives: each owner matches its own
variants and asks the error it holds. -/
def typed (error : Error) : Classification := error.failure.classification

/-- The errors `source()` reaches from `error`, nearest first. -/
def sources : Error → List Error
  | .access _ => []
  | .wrapper .sourced inner => inner :: inner.sources
  | .wrapper .transparent inner => inner.sources

def source (error : Error) : Option Error := error.sources.head?

theorem transparent_forwards_source (inner : Error) :
    (wrapper .transparent inner).source = inner.source := rfl

theorem sourced_returns_what_it_wraps (inner : Error) :
    (wrapper .sourced inner).source = some inner := rfl

def asAccess : Error → Option Failure
  | .access failure => some failure
  | .wrapper _ _ => none

/-- The access error a receiver of the erased error finds: the first on the
`source()` chain, starting with the error itself. The receiver cannot name the
owners above the store, so a wrapper is never asked; only the access owners
are recognised. -/
def found (error : Error) : Option Failure := (error :: error.sources).findSome? asAccess

/-- The classification of an erased (`anyhow` or boxed) error. An error with
no access error on its chain is not a denial and not a conflict. -/
def erased (error : Error) : Classification :=
  match error.found with
  | some failure => failure.classification
  | none => .neither

/-- Whether the variant that holds the access error is `sourced`. Decision
2026-11-13: every owner variant that holds an access error directly is
`sourced`, so the access error rides one level below any transparent variant
above it, and a transparent variant hides only the error it wraps (the
premise `Hooks.rerunSurvives` records for a rerun request). The access owner's
classifier therefore needs no list of the owners above it. A sourced variant
that kept the wrapped error's text as its own message shows that text once
for itself and once for its source wherever the whole chain is printed; the
alternative, a message of its own, would drop the access error's text
wherever only the outermost message is shown. -/
def exposed : Error → Bool
  | .access _ => true
  | .wrapper .sourced inner => inner.exposed
  | .wrapper .transparent (.access _) => false
  | .wrapper .transparent (.wrapper holding inner) => (wrapper holding inner).exposed

theorem found_of_wrapper (holding : Holding) (inner : Error) :
    (wrapper holding inner).found = (wrapper holding inner).sources.findSome? asAccess := by
  simp [found, asAccess]

theorem found_eq (error : Error) :
    error.found = if error.exposed then some error.failure else none := by
  induction error with
  | access failure => simp [found, sources, asAccess, exposed, Error.failure]
  | wrapper holding inner ih =>
    cases holding with
    | sourced =>
      have step : (wrapper .sourced inner).found = inner.found := by
        simp [found, sources, asAccess]
      rw [step, ih]
      simp [exposed, Error.failure]
    | transparent =>
      cases inner with
      | access failure => simp [found, sources, asAccess, exposed]
      | wrapper holding inner =>
        have step : (wrapper .transparent (wrapper holding inner)).found
            = (wrapper holding inner).found := by
          rw [found_of_wrapper, found_of_wrapper]
          rfl
        rw [step, ih]
        simp [exposed, Error.failure]

/-- Wrapping preserves the classification: an erased error whose access error
is held by a sourced variant classifies exactly as its typed owner does. -/
theorem exposed_classifies_as_typed (error : Error) (held : error.exposed = true) :
    error.erased = error.typed := by
  simp [erased, typed, found_eq, held]

/-- An access error held by a transparent variant is lost to every receiver of
the erased error: a refusal reads as an outage and a conflict is not replayed. -/
theorem hidden_is_unclassified (error : Error) (hidden : error.exposed = false) :
    error.erased = .neither := by
  simp [erased, found_eq, hidden]

theorem transparent_holder_loses_a_refusal :
    (wrapper .transparent (.access .forbidden)).erased = .neither
      ∧ (wrapper .transparent (.access .forbidden)).typed = .refused := ⟨rfl, rfl⟩

/-- `holdings` applied to `error`, outermost first. -/
def wrapAll (holdings : List Holding) (error : Error) : Error :=
  holdings.foldr wrapper error

theorem wrapAll_failure (holdings : List Holding) (error : Error) :
    (wrapAll holdings error).failure = error.failure := by
  induction holdings with
  | nil => rfl
  | cons holding rest ih => simpa [wrapAll, Error.failure] using ih

theorem sourced_holder_is_exposed_through_every_wrapping (holdings : List Holding)
    (failure : Failure) :
    (wrapAll holdings (wrapper .sourced (.access failure))).exposed = true := by
  induction holdings with
  | nil => rfl
  | cons holding rest ih =>
    cases holding with
    | sourced => simpa [wrapAll, exposed] using ih
    | transparent =>
      cases rest with
      | nil => rfl
      | cons next rest => simpa [wrapAll, exposed] using ih

/-- Totality: whatever owners wrap a sourced holder of an access error, and
however they hold what they wrap, the erased error classifies as the access
owner classified the failure. -/
theorem classification_survives_every_wrapping (holdings : List Holding) (failure : Failure) :
    (wrapAll holdings (wrapper .sourced (.access failure))).erased = failure.classification := by
  rw [exposed_classifies_as_typed _ (sourced_holder_is_exposed_through_every_wrapping _ _)]
  simp [typed, wrapAll_failure, Error.failure]

end Error

/-- How a classifier receives the error: by its owner's type, or erased into
an application error (`anyhow`) or a boxed `std::error::Error`. -/
inductive Erasure where
  | typed | anyhow | boxed
  deriving DecidableEq, Repr

def Error.classify (error : Error) : Erasure → Classification
  | .typed => error.typed
  | .anyhow | .boxed => error.erased

/-- Every variant of a store error owner that holds an access error, named
`<owner><variant>`. Native fence: the family's consumer reads the store's
sources and fails when a variant holding `ConfigClientError` or
`GraphqlResponseError` is missing here. -/
inductive StoreWrapper where
  | viewErrorExecute
  | packEvidenceErrorConfig
  | documentDecideErrorAuthorize
  | documentSignerErrorRead
  | documentSignerErrorDecode
  | deferredOperationErrorConfig
  | schemaInstallErrorConfigClient
  | agentSelfConfigPermissionErrorAccess
  | receiptWriteErrorWrite
  | runtimeHopCapErrorAccess
  | agentProvisionErrorAccess
  | agentAdmissionObservationErrorAccess
  | remoteAgentProvisionErrorAuthorization
  | runtimeMigrationErrorConfig
  | authoredDocumentCreateErrorPublication
  | authoredDocumentCreateErrorUnconfirmed
  | enrollmentApprovalErrorNodeRead
  | enrollmentGrantErrorRead
  | enrollmentGrantErrorObserve
  | enrollmentGrantErrorRecord
  | enrollmentGrantErrorIssue
  | enrollmentGrantErrorRevoke
  | enrollmentGrantErrorRetire
  | grantSetErrorGrant
  | grantSetErrorRollback
  | grantSetErrorRevoke
  | legacyBackendPreflightErrorQuery
  deriving DecidableEq, Repr

/-- No store variant holds an access error transparently (`Error.exposed`). -/
def StoreWrapper.holding (_ : StoreWrapper) : Holding := .sourced

def StoreWrapper.wrap (wrapper : StoreWrapper) (failure : Failure) : Error :=
  .wrapper wrapper.holding (.access failure)

/-- The variants of the view owners above the store that hold an access error
or a store view error. `inventoryErrorView` holds the store's `ViewError`
transparently, which is sound because `ViewError` holds the access error as a
sourced variant. -/
inductive ViewWrapper where
  | inventoryErrorStore
  | inventoryErrorView
  deriving DecidableEq, Repr

def ViewWrapper.wrap : ViewWrapper → Failure → Error
  | .inventoryErrorStore, failure => .wrapper .sourced (.access failure)
  | .inventoryErrorView, failure => .wrapper .transparent (StoreWrapper.viewErrorExecute.wrap failure)

/-- The classification of `failure` held by a store owner variant. -/
def storeWrapped (wrapper : StoreWrapper) (failure : Failure) (erasure : Erasure) :
    Classification :=
  (wrapper.wrap failure).classify erasure

/-- The classification of `failure` held by a view owner variant above the
store. -/
def viewWrapped (wrapper : ViewWrapper) (failure : Failure) (erasure : Erasure) :
    Classification :=
  (wrapper.wrap failure).classify erasure

theorem store_wrapping_preserves_classification (wrapper : StoreWrapper) (failure : Failure)
    (erasure : Erasure) : storeWrapped wrapper failure erasure = failure.classification := by
  cases erasure <;> cases failure <;> rfl

theorem view_wrapping_preserves_classification (wrapper : ViewWrapper) (failure : Failure)
    (erasure : Erasure) : viewWrapped wrapper failure erasure = failure.classification := by
  cases wrapper <;> cases erasure <;> cases failure <;> rfl

/-- A failure read through a typed view (`gents_store::views::ViewError`) and
erased by the caller, as every client snapshot read receives it. -/
def viewRead (failure : Failure) : Error :=
  .wrapper .sourced (StoreWrapper.viewErrorExecute.wrap failure)

theorem view_read_classifies_as_the_access_owner (failure : Failure) :
    (viewRead failure).erased = failure.classification := by
  cases failure <;> rfl

end AccessClassification
