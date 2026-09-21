import Proofs.PromptAssembly.Budget

/-!
# Shared reduction decision and outcome

The compactor owns one scope-independent decision/outcome engine. It either
leaves the provider view unchanged, reports that no legal reduction can fit, or
returns one checkpoint over an exact prefix together with the exact suffix.
Session compaction and request-local provider-context reduction deliberately
commit that shared outcome through separate persistence functions.
-/

namespace Compaction.ReductionEngine

inductive Decision where
  | notNeeded
  | cannotFit
  | reduce (prefixLength : Nat) (checkpoint : Nat)
  deriving DecidableEq, Repr

abbrev ThresholdDecision := PromptAssembly.Budget.ThresholdDecision

/-- The one input-driven threshold gate. Equality is admitted; only one token
over the effective input budget makes reduction eligible. `cannotFit` remains
an execution outcome after an eligible reduction cannot produce a legal split. -/
def decideThreshold (inputTokens effectiveInputBudget : Nat) : ThresholdDecision :=
  PromptAssembly.Budget.decideThreshold inputTokens effectiveInputBudget

theorem threshold_equality_is_not_needed (effectiveInputBudget : Nat) :
    decideThreshold effectiveInputBudget effectiveInputBudget = .notNeeded := by
  simp [decideThreshold, PromptAssembly.Budget.decideThreshold]

theorem threshold_one_over_is_reduce_eligible (effectiveInputBudget : Nat) :
    decideThreshold (effectiveInputBudget + 1) effectiveInputBudget = .reduceEligible := by
  simp [decideThreshold, PromptAssembly.Budget.decideThreshold]

inductive Outcome where
  | notNeeded (messages : List Nat)
  | cannotFit
  | reduced (compactedPrefix retainedSuffix : List Nat) (checkpoint : Nat)
  deriving DecidableEq, Repr

/-- Apply one reduction decision. A zero-length or overlong requested prefix is
not silently discarded; it becomes the typed `cannotFit` outcome. -/
def applyDecision (source : List Nat) : Decision → Outcome
  | .notNeeded => .notNeeded source
  | .cannotFit => .cannotFit
  | .reduce prefixLength checkpoint =>
      if 0 < prefixLength ∧ prefixLength ≤ source.length then
        .reduced (source.take prefixLength) (source.drop prefixLength) checkpoint
      else
        .cannotFit

/-- Combine the threshold gate with the execution result under one shared
decision owner. Scope-specific callers provide whether a legal bounded split
and checkpoint were produced; they do not reinterpret threshold equality. -/
def reductionDecision
    (inputTokens effectiveInputBudget : Nat) (canFit : Bool)
    (prefixLength checkpoint : Nat) : Decision :=
  match decideThreshold inputTokens effectiveInputBudget with
  | .notNeeded => .notNeeded
  | .reduceEligible =>
      if canFit then .reduce prefixLength checkpoint else .cannotFit

def reduce
    (source : List Nat) (inputTokens effectiveInputBudget : Nat) (canFit : Bool)
    (prefixLength checkpoint : Nat) : Outcome :=
  applyDecision source
    (reductionDecision inputTokens effectiveInputBudget canFit prefixLength checkpoint)

/-- The shared engine's semantic postcondition. A no-op preserves the complete
source, while a reduction returns an exact prefix/suffix partition. -/
def ExactOutcome (source : List Nat) : Outcome → Prop
  | .notNeeded messages => messages = source
  | .cannotFit => True
  | .reduced compactedPrefix retainedSuffix _ =>
      compactedPrefix ++ retainedSuffix = source

instance (source : List Nat) (outcome : Outcome) :
    Decidable (ExactOutcome source outcome) := by
  cases outcome <;> simp [ExactOutcome] <;> infer_instance

theorem apply_decision_is_exact (source : List Nat) (decision : Decision) :
    ExactOutcome source (applyDecision source decision) := by
  cases decision with
  | notNeeded => rfl
  | cannotFit => trivial
  | reduce prefixLength checkpoint =>
      simp only [applyDecision]
      split
      · exact List.take_append_drop prefixLength source
      · trivial

theorem reduce_is_exact
    (source : List Nat) (inputTokens effectiveInputBudget : Nat) (canFit : Bool)
    (prefixLength checkpoint : Nat) :
    ExactOutcome source
      (reduce source inputTokens effectiveInputBudget canFit prefixLength checkpoint) := by
  exact apply_decision_is_exact source _

theorem reduced_outcome_uses_requested_checkpoint
    (source compactedPrefix retainedSuffix : List Nat)
    (inputTokens effectiveInputBudget : Nat) (canFit : Bool)
    (prefixLength checkpoint outcomeCheckpoint : Nat)
    (h : reduce source inputTokens effectiveInputBudget canFit prefixLength checkpoint =
      .reduced compactedPrefix retainedSuffix outcomeCheckpoint) :
    outcomeCheckpoint = checkpoint := by
  cases hthreshold : decideThreshold inputTokens effectiveInputBudget with
  | notNeeded => simp [reduce, reductionDecision, hthreshold, applyDecision] at h
  | reduceEligible =>
      cases canFit
      · simp [reduce, reductionDecision, hthreshold, applyDecision] at h
      · simp [reduce, reductionDecision, hthreshold, applyDecision] at h
        split at h <;> simp_all

theorem valid_reduce_has_requested_prefix_length
    (source : List Nat) (prefixLength checkpoint : Nat)
    (valid : 0 < prefixLength ∧ prefixLength ≤ source.length) :
    applyDecision source (.reduce prefixLength checkpoint) =
      .reduced (source.take prefixLength) (source.drop prefixLength) checkpoint ∧
    (source.take prefixLength).length = prefixLength := by
  constructor
  · simp [applyDecision, valid]
  · simp [List.length_take, Nat.min_eq_left valid.2]

theorem invalid_reduce_cannot_fit
    (source : List Nat) (prefixLength checkpoint : Nat)
    (invalid : ¬ (0 < prefixLength ∧ prefixLength ≤ source.length)) :
    applyDecision source (.reduce prefixLength checkpoint) = .cannotFit := by
  simp [applyDecision, invalid]

/-! ## Projection-bounded reduction and rebuilt dispatch

Projection and estimation are deliberately external functions.  In production
the projection closure may capture the fixed request context and performs the
native reconstruction/materialization step; this model neither substitutes a
tokenizer nor assigns serialization costs.  The composition below does own two
important bindings: the initial estimate is taken from the projection of the
actual source, and a reduced request is rebuilt only from that same outcome's
checkpoint and retained suffix.
-/

/-- Evidence retained at a projection boundary.  Consumers should obtain this
through `projectRequest`; the composition never accepts one as an input. -/
structure ProjectedRequest (Request : Type) where
  source : List Nat
  request : Request
  inputTokens : Nat

/-- Fallibly reconstruct/materialize exactly `source`, then estimate that
materialized request. -/
def projectRequest {Request Error : Type}
    (project : List Nat → Except Error Request) (estimate : Request → Except Error Nat)
    (source : List Nat) : Except Error (ProjectedRequest Request) :=
  match project source with
  | .error error => .error error
  | .ok request =>
      match estimate request with
      | .error error => .error error
      | .ok inputTokens => .ok { source, request, inputTokens }

theorem project_request_binds_source_and_estimate
    {Request Error : Type} (project : List Nat → Except Error Request)
    (estimate : Request → Except Error Nat) (source : List Nat)
    (projected : ProjectedRequest Request)
    (h : projectRequest project estimate source = .ok projected) :
    projected.source = source ∧
      project projected.source = .ok projected.request ∧
      estimate projected.request = .ok projected.inputTokens := by
  cases hproject : project source with
  | error error => simp [projectRequest, hproject] at h
  | ok request =>
      cases hestimate : estimate request with
      | error error => simp [projectRequest, hproject, hestimate] at h
      | ok inputTokens =>
          simp [projectRequest, hproject, hestimate] at h
          subst projected
          simp [hproject, hestimate]

/-- Admission for any freshly materialized provider view, including a
view repaired without creating a checkpoint.  It always projects and estimates
the supplied source again before applying the same threshold and output guards. -/
inductive FreshAdmission (Request : Type) where
  | overThreshold (projected : ProjectedRequest Request)
  | noOutputCapacity (projected : ProjectedRequest Request)
  | dispatch (projected : ProjectedRequest Request) (outputTokens : Nat)

def projectAndAuthorize {Request Error : Type}
    (project : List Nat → Except Error Request) (estimate : Request → Except Error Nat)
    (source : List Nat) (effectiveInputBudget contextWindow configuredMaxOutputTokens : Nat) :
    Except Error (FreshAdmission Request) :=
  match projectRequest project estimate source with
  | .error error => .error error
  | .ok projected =>
      if PromptAssembly.Budget.CanDispatch projected.inputTokens contextWindow
          configuredMaxOutputTokens then
        if decideThreshold projected.inputTokens effectiveInputBudget = .reduceEligible then
          .ok (.overThreshold projected)
        else .ok (.dispatch projected
          (PromptAssembly.Budget.effectiveOutputBudget projected.inputTokens contextWindow
            configuredMaxOutputTokens))
      else .ok (.noOutputCapacity projected)

theorem fresh_dispatch_is_authorized
    {Request Error : Type} (project : List Nat → Except Error Request)
    (estimate : Request → Except Error Nat) (source : List Nat)
    (effectiveInputBudget contextWindow configuredMaxOutputTokens outputTokens : Nat)
    (projected : ProjectedRequest Request)
    (h : projectAndAuthorize project estimate source effectiveInputBudget contextWindow
      configuredMaxOutputTokens = .ok (.dispatch projected outputTokens)) :
    projected.source = source ∧
      decideThreshold projected.inputTokens effectiveInputBudget = .notNeeded ∧
      PromptAssembly.Budget.CanDispatch projected.inputTokens contextWindow
        configuredMaxOutputTokens ∧ 0 < outputTokens ∧
      outputTokens = PromptAssembly.Budget.effectiveOutputBudget projected.inputTokens
        contextWindow configuredMaxOutputTokens ∧
      projected.inputTokens + outputTokens ≤ contextWindow := by
  cases hproject : projectRequest project estimate source with
  | error error => simp [projectAndAuthorize, hproject] at h
  | ok actual =>
      by_cases hcapacity : PromptAssembly.Budget.CanDispatch actual.inputTokens contextWindow
          configuredMaxOutputTokens
      · by_cases hover : decideThreshold actual.inputTokens effectiveInputBudget = .reduceEligible
        · simp [projectAndAuthorize, hproject, hcapacity, hover] at h
        · simp [projectAndAuthorize, hproject, hcapacity, hover] at h
          obtain ⟨rfl, rfl⟩ := h
          have hsource := (project_request_binds_source_and_estimate project estimate source
            actual hproject).1
          refine ⟨hsource, ?_, hcapacity, hcapacity, rfl,
            PromptAssembly.Budget.can_dispatch_is_provider_safe hcapacity⟩
          cases hd : decideThreshold actual.inputTokens effectiveInputBudget
          · rfl
          · exact (hover hd).elim
      · simp [projectAndAuthorize, hproject, hcapacity] at h

structure RebuiltRequest (Request : Type) where
  checkpoint : Nat
  retainedSuffix : List Nat
  request : Request
  inputTokens : Nat

def projectRebuilt {Request Error : Type}
    (rebuild : Nat → List Nat → Except Error Request)
    (estimate : Request → Except Error Nat) (checkpoint : Nat) (retainedSuffix : List Nat) :
    Except Error (RebuiltRequest Request) :=
  match rebuild checkpoint retainedSuffix with
  | .error error => .error error
  | .ok request =>
      match estimate request with
      | .error error => .error error
      | .ok inputTokens => .ok { checkpoint, retainedSuffix, request, inputTokens }

inductive RebuiltDispatch (Error Request : Type) where
  | notReduced (outcome : Outcome)
  | projectionFailed (error : Error)
  | stillOverThreshold (projected : RebuiltRequest Request)
  | noOutputCapacity (projected : RebuiltRequest Request)
  | dispatch (projected : RebuiltRequest Request) (outputTokens : Nat)

/-- Rebuild a reduced outcome, project and estimate that rebuilt request, then
re-run both the input threshold and positive-output-capacity guards. -/
def rebuildAndAuthorize {Request Error : Type}
    (rebuild : Nat → List Nat → Except Error Request)
    (estimate : Request → Except Error Nat)
    (outcome : Outcome) (effectiveInputBudget contextWindow
      configuredMaxOutputTokens : Nat) : RebuiltDispatch Error Request :=
  match outcome with
  | .reduced _ retainedSuffix checkpoint =>
      match projectRebuilt rebuild estimate checkpoint retainedSuffix with
      | .error error => .projectionFailed error
      | .ok projected =>
          if PromptAssembly.Budget.CanDispatch projected.inputTokens contextWindow
              configuredMaxOutputTokens then
            if decideThreshold projected.inputTokens effectiveInputBudget = .reduceEligible then
              .stillOverThreshold projected
            else .dispatch projected
              (PromptAssembly.Budget.effectiveOutputBudget projected.inputTokens contextWindow
                configuredMaxOutputTokens)
          else
            .noOutputCapacity projected
  | outcome => .notReduced outcome

theorem rebuilt_dispatch_is_authorized
    {Request Error : Type} (rebuild : Nat → List Nat → Except Error Request)
    (estimate : Request → Except Error Nat) (outcome : Outcome)
    (effectiveInputBudget contextWindow configuredMaxOutputTokens outputTokens : Nat)
    (projected : RebuiltRequest Request)
    (h : rebuildAndAuthorize rebuild estimate outcome effectiveInputBudget contextWindow
      configuredMaxOutputTokens = .dispatch projected outputTokens) :
    decideThreshold projected.inputTokens effectiveInputBudget = .notNeeded ∧
      PromptAssembly.Budget.CanDispatch projected.inputTokens contextWindow
        configuredMaxOutputTokens ∧
      outputTokens = PromptAssembly.Budget.effectiveOutputBudget projected.inputTokens
        contextWindow configuredMaxOutputTokens := by
  cases outcome with
  | notNeeded messages => simp [rebuildAndAuthorize] at h
  | cannotFit => simp [rebuildAndAuthorize] at h
  | reduced compacted suffix checkpoint =>
      cases hproject : projectRebuilt rebuild estimate checkpoint suffix with
      | error error => simp [rebuildAndAuthorize, hproject] at h
      | ok actual =>
          by_cases hcapacity : PromptAssembly.Budget.CanDispatch actual.inputTokens
              contextWindow configuredMaxOutputTokens
          · by_cases hover : decideThreshold actual.inputTokens effectiveInputBudget = .reduceEligible
            · simp [rebuildAndAuthorize, hproject, hcapacity, hover] at h
            · simp [rebuildAndAuthorize, hproject, hcapacity, hover] at h
              obtain ⟨rfl, rfl⟩ := h
              refine ⟨?_, hcapacity, rfl⟩
              cases hd : decideThreshold actual.inputTokens effectiveInputBudget
              · rfl
              · exact (hover hd).elim
          · simp [rebuildAndAuthorize, hproject, hcapacity] at h

theorem rebuilt_dispatch_has_positive_output_capacity
    {Request Error : Type} (rebuild : Nat → List Nat → Except Error Request)
    (estimate : Request → Except Error Nat) (outcome : Outcome)
    (effectiveInputBudget contextWindow configuredMaxOutputTokens outputTokens : Nat)
    (projected : RebuiltRequest Request)
    (h : rebuildAndAuthorize rebuild estimate outcome effectiveInputBudget contextWindow
      configuredMaxOutputTokens = .dispatch projected outputTokens) :
    0 < outputTokens := by
  obtain ⟨_, hcapacity, hout⟩ := rebuilt_dispatch_is_authorized rebuild estimate outcome
    effectiveInputBudget contextWindow configuredMaxOutputTokens outputTokens projected h
  rw [hout]
  exact hcapacity

theorem rebuilt_dispatch_binds_exact_reduction
    {Request Error : Type} (rebuild : Nat → List Nat → Except Error Request)
    (estimate : Request → Except Error Nat) (source compactedPrefix retainedSuffix : List Nat)
    (checkpoint effectiveInputBudget contextWindow configuredMaxOutputTokens outputTokens : Nat)
    (projected : RebuiltRequest Request)
    (hexact : ExactOutcome source (.reduced compactedPrefix retainedSuffix checkpoint))
    (h : rebuildAndAuthorize rebuild estimate
      (.reduced compactedPrefix retainedSuffix checkpoint) effectiveInputBudget contextWindow
      configuredMaxOutputTokens = .dispatch projected outputTokens) :
    compactedPrefix ++ retainedSuffix = source ∧
      projected.checkpoint = checkpoint ∧ projected.retainedSuffix = retainedSuffix ∧
      rebuild checkpoint retainedSuffix = .ok projected.request ∧
      estimate projected.request = .ok projected.inputTokens := by
  have hpartition : compactedPrefix ++ retainedSuffix = source := hexact
  cases hproject : projectRebuilt rebuild estimate checkpoint retainedSuffix with
  | error error => simp [rebuildAndAuthorize, hproject] at h
  | ok actual =>
      by_cases hcapacity : PromptAssembly.Budget.CanDispatch actual.inputTokens contextWindow
          configuredMaxOutputTokens
      · by_cases hover : decideThreshold actual.inputTokens effectiveInputBudget = .reduceEligible
        · simp [rebuildAndAuthorize, hproject, hcapacity, hover] at h
        · simp [rebuildAndAuthorize, hproject, hcapacity, hover] at h
          obtain ⟨rfl, rfl⟩ := h
          cases hrebuilt : rebuild checkpoint retainedSuffix with
          | error error => simp [projectRebuilt, hrebuilt] at hproject
          | ok request =>
              cases hestimate : estimate request with
              | error error => simp [projectRebuilt, hrebuilt, hestimate] at hproject
              | ok tokens =>
                  simp [projectRebuilt, hrebuilt, hestimate] at hproject
                  subst actual
                  simp [hpartition, hrebuilt, hestimate]
      · simp [rebuildAndAuthorize, hproject, hcapacity] at h

/-- The single reduction-to-dispatch entry.  Both threshold checks use the same
Budget-owned effective threshold.  The initial callback consumes canonical
source identities; the distinct rebuild callback consumes the generated
checkpoint and exact retained identity suffix, so the checkpoint is never
misinterpreted as a durable message identity. -/
def reduceRebuildAndAuthorize {Request Error : Type}
    (project : List Nat → Except Error Request)
    (rebuild : Nat → List Nat → Except Error Request)
    (estimate : Request → Except Error Nat) (source : List Nat)
    (contextWindow thresholdBasisPoints configuredMaxOutputTokens : Nat)
    (canFit : Bool) (prefixLength checkpoint : Nat) :
    Except Error (RebuiltDispatch Error Request) :=
  let effectiveInputBudget := PromptAssembly.Budget.effectiveInputBudget
    (PromptAssembly.Budget.configuredThresholdBudget contextWindow thresholdBasisPoints)
    contextWindow
  match projectRequest project estimate source with
  | .error error => .error error
  | .ok projected =>
      .ok (rebuildAndAuthorize rebuild estimate
        (reduce source projected.inputTokens effectiveInputBudget canFit prefixLength checkpoint)
        effectiveInputBudget contextWindow configuredMaxOutputTokens)

theorem composed_dispatch_is_authorized
    {Request Error : Type} (project : List Nat → Except Error Request)
    (rebuild : Nat → List Nat → Except Error Request)
    (estimate : Request → Except Error Nat) (source : List Nat)
    (contextWindow thresholdBasisPoints configuredMaxOutputTokens : Nat)
    (canFit : Bool) (prefixLength checkpoint outputTokens : Nat)
    (projected : RebuiltRequest Request)
    (h : reduceRebuildAndAuthorize project rebuild estimate source contextWindow
      thresholdBasisPoints configuredMaxOutputTokens canFit prefixLength checkpoint =
      .ok (.dispatch projected outputTokens)) :
    let budget := PromptAssembly.Budget.effectiveInputBudget
      (PromptAssembly.Budget.configuredThresholdBudget contextWindow thresholdBasisPoints)
      contextWindow
    decideThreshold projected.inputTokens budget = .notNeeded ∧
      PromptAssembly.Budget.CanDispatch projected.inputTokens contextWindow
        configuredMaxOutputTokens ∧
      0 < outputTokens ∧
      outputTokens = PromptAssembly.Budget.effectiveOutputBudget projected.inputTokens
        contextWindow configuredMaxOutputTokens ∧
      projected.inputTokens + outputTokens ≤ contextWindow := by
  dsimp
  cases hinitial : projectRequest project estimate source with
  | error error => simp [reduceRebuildAndAuthorize, hinitial] at h
  | ok initial =>
      simp [reduceRebuildAndAuthorize, hinitial] at h
      obtain ⟨hthreshold, hcapacity, hout⟩ := rebuilt_dispatch_is_authorized rebuild estimate
        (reduce source initial.inputTokens
          (PromptAssembly.Budget.effectiveInputBudget
            (PromptAssembly.Budget.configuredThresholdBudget contextWindow thresholdBasisPoints)
            contextWindow)
          canFit prefixLength checkpoint)
        (PromptAssembly.Budget.effectiveInputBudget
          (PromptAssembly.Budget.configuredThresholdBudget contextWindow thresholdBasisPoints)
          contextWindow)
        contextWindow configuredMaxOutputTokens outputTokens projected h
      refine ⟨hthreshold, hcapacity, hout ▸ hcapacity, hout, ?_⟩
      rw [hout]
      exact PromptAssembly.Budget.can_dispatch_is_provider_safe hcapacity

theorem composed_dispatch_binds_exact_source
    {Request Error : Type} (project : List Nat → Except Error Request)
    (rebuild : Nat → List Nat → Except Error Request)
    (estimate : Request → Except Error Nat) (source : List Nat)
    (contextWindow thresholdBasisPoints configuredMaxOutputTokens : Nat)
    (canFit : Bool) (prefixLength checkpoint outputTokens : Nat)
    (projected : RebuiltRequest Request)
    (h : reduceRebuildAndAuthorize project rebuild estimate source contextWindow
      thresholdBasisPoints configuredMaxOutputTokens canFit prefixLength checkpoint =
      .ok (.dispatch projected outputTokens)) :
    ∃ (initial : ProjectedRequest Request) (compactedPrefix retainedSuffix : List Nat),
      initial.source = source ∧
      project source = .ok initial.request ∧
      estimate initial.request = .ok initial.inputTokens ∧
      compactedPrefix ++ retainedSuffix = source ∧
      projected.checkpoint = checkpoint ∧ projected.retainedSuffix = retainedSuffix ∧
      rebuild checkpoint retainedSuffix = .ok projected.request ∧
      estimate projected.request = .ok projected.inputTokens := by
  cases hinitial : projectRequest project estimate source with
  | error error => simp [reduceRebuildAndAuthorize, hinitial] at h
  | ok initial =>
      let budget := PromptAssembly.Budget.effectiveInputBudget
        (PromptAssembly.Budget.configuredThresholdBudget contextWindow thresholdBasisPoints)
        contextWindow
      have hrebuilt : rebuildAndAuthorize rebuild estimate
          (reduce source initial.inputTokens budget canFit prefixLength checkpoint)
          budget contextWindow configuredMaxOutputTokens = .dispatch projected outputTokens := by
        simpa [reduceRebuildAndAuthorize, hinitial, budget] using h
      cases houtcome : reduce source initial.inputTokens budget canFit prefixLength checkpoint with
      | notNeeded messages => simp [houtcome, rebuildAndAuthorize] at hrebuilt
      | cannotFit => simp [houtcome, rebuildAndAuthorize] at hrebuilt
      | reduced compactedPrefix retainedSuffix outcomeCheckpoint =>
          have hexact : ExactOutcome source
              (.reduced compactedPrefix retainedSuffix outcomeCheckpoint) := by
            rw [← houtcome]
            exact reduce_is_exact source initial.inputTokens budget canFit prefixLength checkpoint
          have hcheckpoint : outcomeCheckpoint = checkpoint :=
            reduced_outcome_uses_requested_checkpoint source compactedPrefix retainedSuffix
              initial.inputTokens budget canFit prefixLength checkpoint outcomeCheckpoint houtcome
          rw [houtcome] at hrebuilt
          subst outcomeCheckpoint
          obtain ⟨hsource, hproject, hestimate⟩ :=
            project_request_binds_source_and_estimate project estimate source initial hinitial
          exact ⟨initial, compactedPrefix, retainedSuffix, hsource, hsource ▸ hproject, hestimate,
            rebuilt_dispatch_binds_exact_reduction rebuild estimate source compactedPrefix
              retainedSuffix checkpoint budget contextWindow configuredMaxOutputTokens outputTokens
              projected hexact hrebuilt⟩

/-! ## Scope-specific persistence

These states are intentionally different types. The shared engine decides and
constructs the exact reduction; each caller then commits it according to its
own crash-cut and identity contract.
-/

structure SessionState where
  cursor : Nat
  checkpoint : Option Nat
  deriving DecidableEq, Repr

def commitSession (before : SessionState) : Outcome → SessionState
  | .reduced compactedPrefix _ checkpoint =>
      { cursor := before.cursor + compactedPrefix.length
      , checkpoint := some checkpoint }
  | _ => before

structure RequestLocalState where
  reductions : List (Nat × Nat)
  deriving DecidableEq, Repr

def commitRequestLocal (before : RequestLocalState) : Outcome → RequestLocalState
  | .reduced compactedPrefix _ checkpoint =>
      { reductions := before.reductions ++ [(compactedPrefix.length, checkpoint)] }
  | _ => before

theorem session_noop_does_not_commit
    (before : SessionState) (messages : List Nat) :
    commitSession before (.notNeeded messages) = before ∧
    commitSession before .cannotFit = before := by
  constructor <;> rfl

theorem request_local_noop_does_not_commit
    (before : RequestLocalState) (messages : List Nat) :
    commitRequestLocal before (.notNeeded messages) = before ∧
    commitRequestLocal before .cannotFit = before := by
  constructor <;> rfl

theorem session_commit_advances_exact_prefix
    (before : SessionState) (compactedPrefix retainedSuffix : List Nat)
    (checkpoint : Nat) :
    (commitSession before (.reduced compactedPrefix retainedSuffix checkpoint)).cursor =
      before.cursor + compactedPrefix.length := by
  rfl

theorem request_local_commit_appends_exact_prefix
    (before : RequestLocalState) (compactedPrefix retainedSuffix : List Nat)
    (checkpoint : Nat) :
    (commitRequestLocal before
      (.reduced compactedPrefix retainedSuffix checkpoint)).reductions =
      before.reductions ++ [(compactedPrefix.length, checkpoint)] := by
  rfl

end Compaction.ReductionEngine
