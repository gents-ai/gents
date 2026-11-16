import Proofs.Runtime.BackendHealth.Scheduling
import Proofs.Runtime.BackendHealth.Transition

/-! One probe cycle over the backends listed as enabled for this node: which
measurements it records, which it keeps, which change routing, and which
shared documents it promotes from `unknown` to `healthy`. -/

namespace Proofs.BackendHealth

/-- What a cycle can learn about a listed backend. A node-OAuth backend learns
nothing without a probe context: it is neither measured nor observed. -/
inductive ProbeTarget where
  | reachable
  | unreachable
  | nodeOauth
  deriving DecidableEq, Repr

def ProbeTarget.event : ProbeTarget → Option Event
  | .reachable => some .probeSuccess
  | .unreachable => some .probeFail
  | .nodeOauth => none

/-- A measurement remembers the probe inputs (`revision`) it was taken with;
`none` is a measurement recorded without them. -/
structure Entry where
  id : Nat
  model : Model
  revision : Option Nat
  deriving DecidableEq, Repr

def Entry.find? (entries : List Entry) (id : Nat) : Option Entry :=
  entries.find? (·.id == id)

/-- A measurement taken with other probe inputs (another endpoint, credential
or account) does not describe this configuration. -/
def stateFor (entry : Entry) (revision : Nat) : Option HealthState :=
  if entry.revision.all (· == revision) then some entry.model.state else none

/-- Routing availability or the account-recovery reason changed. -/
def flips (before after : HealthState) : Bool :=
  before.blocksRouting != after.blocksRouting || before.readinessReason != after.readinessReason

structure Recorded where
  model : Model
  flipped : Bool
  deriving DecidableEq, Repr

def recordEvent (prior : Option Entry) (revision : Nat) (event : Event) (K : Threshold) :
    Recorded :=
  let before := match prior with
    | none => Model.initial
    | some entry =>
        if entry.revision.any (· != revision) then step entry.model .configurationChanged K
        else entry.model
  let after := step before event K
  { model := after, flipped := flips ((prior.map (·.model.state)).getD .unknown) after.state }

structure CycleBackend where
  id : Nat
  revision : Nat
  target : ProbeTarget
  statusUnknown : Bool
  due : Bool
  deriving DecidableEq, Repr

structure Measured where
  entry : Option (Model × Option Nat)
  flipped : Bool
  promotable : Bool
  catalogued : Bool
  deriving DecidableEq, Repr

/-- A successful probe records a catalog and promotes a shared document whose
status is still `unknown`; a backend that is not due keeps its measurement. -/
def measure (entries : List Entry) (b : CycleBackend) (event : Event) (K : Threshold) :
    Measured :=
  let prior := Entry.find? entries b.id
  if b.due then
    let recorded := recordEvent prior b.revision event K
    { entry := some (recorded.model, some b.revision)
      flipped := recorded.flipped
      promotable := event == .probeSuccess && b.statusUnknown
      catalogued := event == .probeSuccess }
  else
    { entry := prior.map fun p => (p.model, p.revision)
      flipped := false, promotable := false, catalogued := false }

structure CycleResult where
  entries : List Entry
  flipped : List Nat
  promotable : List Nat
  catalogued : List Nat
  deriving DecidableEq, Repr

def measuredBackends (entries : List Entry) (backends : List CycleBackend) (K : Threshold) :
    List (Nat × Measured) :=
  backends.filterMap fun b => b.target.event.map fun event => (b.id, measure entries b event K)

/-- A cancelled cycle observes nothing and prunes nothing: shutdown must not read
as the disappearance of every backend. Otherwise a measurement survives exactly
while its backend is listed and observed. -/
def probeCycle (entries : List Entry) (backends : List CycleBackend) (cancelled : Bool)
    (K : Threshold) : CycleResult :=
  if cancelled then { entries, flipped := [], promotable := [], catalogued := [] } else
  let measured := measuredBackends entries backends K
  { entries := measured.filterMap fun (id, m) => m.entry.map fun (model, revision) =>
      { id, model, revision }
    flipped := (measured.filter (·.2.flipped)).map (·.1)
    promotable := (measured.filter (·.2.promotable)).map (·.1)
    catalogued := (measured.filter (·.2.catalogued)).map (·.1) }

theorem probe_shutdown_prevents_new_backend_probes_and_pruning (entries : List Entry)
    (backends : List CycleBackend) (K : Threshold) :
    probeCycle entries backends true K =
      { entries, flipped := [], promotable := [], catalogued := [] } := by
  simp [probeCycle]

theorem mem_measuredBackends (entries : List Entry) (backends : List CycleBackend)
    (K : Threshold) (id : Nat) (m : Measured)
    (hmem : (id, m) ∈ measuredBackends entries backends K) :
    ∃ b ∈ backends, b.id = id ∧ ∃ event, b.target.event = some event ∧
      m = measure entries b event K := by
  unfold measuredBackends at hmem
  rw [List.mem_filterMap] at hmem
  obtain ⟨b, hb, h⟩ := hmem
  cases hev : b.target.event with
  | none => simp [hev] at h
  | some event =>
      simp only [hev, Option.map_some', Option.some.injEq, Prod.mk.injEq] at h
      exact ⟨b, hb, h.1, event, hev, h.2.symm⟩

theorem cycle_drops_entries_for_backends_no_longer_enabled (entries : List Entry)
    (backends : List CycleBackend) (K : Threshold) (e : Entry)
    (kept : e ∈ (probeCycle entries backends false K).entries) :
    ∃ b ∈ backends, b.id = e.id ∧ b.target ≠ .nodeOauth := by
  simp only [probeCycle, Bool.false_eq_true, if_false] at kept
  rw [List.mem_filterMap] at kept
  obtain ⟨⟨id, m⟩, hmem, hentry⟩ := kept
  obtain ⟨b, hb, hid, event, hev, _⟩ := mem_measuredBackends entries backends K id m hmem
  obtain ⟨⟨model, revision⟩, _, he⟩ := Option.map_eq_some'.mp hentry
  refine ⟨b, hb, by rw [← he, hid], ?_⟩
  intro htarget
  simp [htarget, ProbeTarget.event] at hev

theorem oauth_kinds_are_skipped_without_a_probe_context (entries : List Entry)
    (backends : List CycleBackend) (K : Threshold) (id : Nat)
    (promoted : id ∈ (probeCycle entries backends false K).promotable) :
    ∃ b ∈ backends, b.id = id ∧ b.target = .reachable := by
  simp only [probeCycle, Bool.false_eq_true, if_false, List.mem_map, List.mem_filter] at promoted
  obtain ⟨⟨id', m⟩, ⟨hmem, hprom⟩, hid⟩ := promoted
  obtain ⟨b, hb, hbid, event, hev, hm⟩ := mem_measuredBackends entries backends K id' m hmem
  refine ⟨b, hb, by rw [hbid]; exact hid, ?_⟩
  subst hm
  cases ht : b.target with
  | reachable => rfl
  | unreachable =>
      simp [ht, ProbeTarget.event] at hev
      subst hev
      simp only [measure] at hprom
      split at hprom <;> simp at hprom
  | nodeOauth => simp [ht, ProbeTarget.event] at hev

theorem scheduled_cycles_preserve_health_across_observations_and_unrelated_edits
    (entries : List Entry) (b : CycleBackend) (event : Event) (K : Threshold)
    (notDue : b.due = false) :
    measure entries b event K =
      { entry := (Entry.find? entries b.id).map fun p => (p.model, p.revision)
        flipped := false, promotable := false, catalogued := false } := by
  simp [measure, notDue]

theorem subscription_probe_account_changes_retire_previous_measurement
    (entry : Entry) (revision : Nat) (event : Event) (K : Threshold)
    (other : entry.revision = some (revision + 1)) :
    stateFor entry revision = none ∧
      (recordEvent (some entry) revision event K).model = step Model.initial event K := by
  simp [stateFor, recordEvent, other, step]

/-- A scheduled step lists the node's backend documents; one missing from the
list was removed. Reachability is part of the connection a revision names. -/
structure TraceBackend where
  id : Nat
  revision : Nat
  enabled : Bool
  reachable : Bool
  deriving DecidableEq, Repr

structure TraceStep where
  backends : List TraceBackend
  periodic : Bool
  deriving DecidableEq, Repr

/-- Observation fields of a shared backend document: they survive configuration
rewrites and are lost with the document. -/
structure DocumentObservation where
  id : Nat
  healthy : Bool
  catalogued : Bool
  deriving DecidableEq, Repr

structure TraceState where
  entries : List Entry
  known : List ProbeConfiguration
  documents : List DocumentObservation
  deriving DecidableEq, Repr

structure StepObservation where
  result : CycleResult
  probed : List Nat
  known : List Nat
  documents : List DocumentObservation
  deriving DecidableEq, Repr

def TraceBackend.configuration (b : TraceBackend) : ProbeConfiguration :=
  { backendId := b.id, revision := b.revision, enabled := b.enabled }

def traceStep (state : TraceState) (step : TraceStep) (K : Threshold) :
    TraceState × StepObservation :=
  let current := step.backends.map (·.configuration)
  let due := scheduledProbes state.known current step.periodic
  let documents := step.backends.map fun b =>
    (state.documents.find? (·.id == b.id)).getD { id := b.id, healthy := false, catalogued := false }
  let listed := (step.backends.filter (·.enabled)).map fun b =>
    { id := b.id, revision := b.revision
      target := if b.reachable then .reachable else .unreachable
      statusUnknown := !((documents.find? (·.id == b.id)).any (·.healthy))
      due := due.contains b.id : CycleBackend }
  let result := probeCycle state.entries listed false K
  let documents := documents.map fun d =>
    { d with healthy := d.healthy || result.promotable.contains d.id
             catalogued := d.catalogued || result.catalogued.contains d.id }
  let known := observedProbeConfigurations current
  ({ entries := result.entries, known, documents },
   { result, probed := due, known := known.map (·.backendId), documents })

def runTrace (steps : List TraceStep) (K : Threshold) : List StepObservation :=
  (steps.foldl (fun (acc : TraceState × List StepObservation) step =>
    let (next, observation) := traceStep acc.1 step K
    (next, acc.2 ++ [observation])) ({ entries := [], known := [], documents := [] }, [])).2

/-- A disabled or removed backend leaves the schedule's known set, so the next
cycle that lists it enabled probes it as new (`new_enabled_probed`). -/
theorem disabled_and_removed_backends_are_probed_again_when_observed_enabled
    (current : List ProbeConfiguration) (c : ProbeConfiguration) (enabled : c.enabled = true)
    (absent : ∀ x ∈ current, x.backendId = c.backendId → x.enabled = false) :
    c ∉ observedProbeConfigurations current := by
  intro h
  unfold observedProbeConfigurations at h
  have hx := List.mem_filter.mp h
  have := absent c hx.1 rfl
  rw [enabled] at this
  exact absurd this (by decide)

end Proofs.BackendHealth
