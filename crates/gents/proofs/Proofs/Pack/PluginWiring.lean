import Mathlib.Data.List.Basic
import Mathlib.Data.List.Dedup

/-!
# Plugin wiring

Wiring connects an installed plugin to one Agent. It writes three things on
that Agent: the plugin entry among its Tools, the Agent targets the Agent may
start, and the skills in its Context. Unwiring takes them back.

A plugin entry may carry a job block: the ceiling within which the plugin runs
job steps. A block comes from the block the installer reviewed, or from the
defaults over the packs whose targets are wired in. An existing block is never
widened by a later wiring: bounds only fall, the dynamic mode only falls, and
the targets dynamic scripts may reach only shrink. Targets and trusted packs
are the additions a wiring is for.

Nothing records which wiring added a target or a skill, so unwiring decides
from what is left: a target goes when no remaining job block names it, and the
skills go with the last plugin entry.
-/

namespace Pack.PluginWiring

inductive Dynamic | off | check | run
  deriving DecidableEq, Repr

def Dynamic.rank : Dynamic → Nat
  | .off => 0
  | .check => 1
  | .run => 2

structure Policy where
  dynamic : Dynamic
  dynamicTargets : List String
  trustedPacks : List String
  deriving DecidableEq, Repr

structure Job where
  agentTargets : List String
  maxLive : Nat
  maxSpawns : Nat
  maxMessagesPerSlot : Nat
  resultBytes : Nat
  maxWallSecs : Nat
  policy : Policy
  deriving DecidableEq, Repr

/-- The rules a job block must meet to be written. Trusted packs must also be
pack coordinates without a version; that grammar belongs to the pack spec and
is not restated here, so the model's inputs use well-formed coordinates. A
target of only whitespace is refused like an empty one. -/
def Job.valid (job : Job) : Bool :=
  !job.agentTargets.isEmpty
    && job.agentTargets.all (· ≠ "")
    && decide job.agentTargets.Nodup
    && decide (1 ≤ job.maxLive ∧ job.maxLive ≤ 64)
    && decide (1 ≤ job.maxSpawns ∧ job.maxSpawns ≤ 1024)
    && decide (job.maxMessagesPerSlot ≤ 16)
    && decide (1024 ≤ job.resultBytes ∧ job.resultBytes ≤ 1048576)
    && decide (60 ≤ job.maxWallSecs ∧ job.maxWallSecs ≤ 86400)
    && (job.policy.dynamic == .off || job.policy.dynamicTargets.all (· ∈ job.agentTargets))

structure Entry where
  plugin : String
  digest : String
  toolCalls : Bool
  job : Option Job
  deriving DecidableEq, Repr

/-- What wiring reads and writes on one Agent. -/
structure Surface where
  plugins : List Entry
  targets : List String
  enabled : Option Bool
  skills : List String
  deriving DecidableEq, Repr

/-- The installed plugin being wired. -/
structure Plugin where
  coordinate : String
  digest : String
  runsJobs : Bool
  ownerPack : Option String
  deriving DecidableEq, Repr

/-- A pack whose Agent targets are wired into the plugin's job block. -/
structure Source where
  pack : String
  targets : List String
  operatorInstalled : Bool
  deriving DecidableEq, Repr

inductive Warning
  /-- A wired pack the operator did not install; its scripts classify as dynamic. -/
  | trustedPackNotOperator
  /-- The block is at `check`: scripts are checked and their effects refused. -/
  | dynamicCheck
  /-- The Agent may start Agents directly, so the new targets widen that too. -/
  | agentNewWidened
  | notWired
  deriving DecidableEq, Repr

structure Outcome where
  surface : Surface
  /-- The plugin's job block after the change. -/
  job : Option Job
  warnings : List Warning
  /-- The plugin can run jobs and has no block yet; the report names the call. -/
  nextCall : Bool
  deriving DecidableEq, Repr

def addMissing (list items : List String) : List String :=
  items.foldl (fun list item => if item ∈ list then list else list ++ [item]) list

def merge (existing incoming : Job) : Job :=
  { agentTargets := addMissing existing.agentTargets incoming.agentTargets
    maxLive := min existing.maxLive incoming.maxLive
    maxSpawns := min existing.maxSpawns incoming.maxSpawns
    maxMessagesPerSlot := min existing.maxMessagesPerSlot incoming.maxMessagesPerSlot
    resultBytes := min existing.resultBytes incoming.resultBytes
    maxWallSecs := min existing.maxWallSecs incoming.maxWallSecs
    policy :=
      { dynamic :=
          if incoming.policy.dynamic.rank < existing.policy.dynamic.rank
          then incoming.policy.dynamic else existing.policy.dynamic
        dynamicTargets :=
          existing.policy.dynamicTargets.filter (· ∈ incoming.policy.dynamicTargets)
        trustedPacks := addMissing existing.policy.trustedPacks incoming.policy.trustedPacks } }

/-- The block written when packs are wired in and nobody reviewed one: the
default bounds, dynamic scripts checked, and the plugin's own pack trusted. -/
def defaultJob (plugin : Plugin) : Job :=
  { agentTargets := []
    maxLive := 8
    maxSpawns := 128
    maxMessagesPerSlot := 4
    resultBytes := 65536
    maxWallSecs := 7200
    policy := { dynamic := .check, dynamicTargets := [], trustedPacks := plugin.ownerPack.toList } }

/-- Wiring one pack trusts it, adds its targets to the job's and the job's to
the Agent's. -/
def wireSource (state : Job × List String) (source : Source) : Job × List String :=
  let targets := state.1.agentTargets ∪ source.targets
  ({ state.1 with
      agentTargets := targets
      policy := { state.1.policy with trustedPacks := state.1.policy.trustedPacks.insert source.pack } },
    state.2 ∪ targets)

def pinEntry (plugin : Plugin) (job : Option Job) : List Entry → List Entry
  | [] => [{ plugin := plugin.coordinate, digest := plugin.digest, toolCalls := false, job }]
  | entry :: rest =>
    if entry.plugin = plugin.coordinate
    then { entry with digest := plugin.digest, job } :: rest
    else entry :: pinEntry plugin job rest

def priorJob (surface : Surface) (plugin : Plugin) : Option Job :=
  (surface.plugins.find? (·.plugin = plugin.coordinate)).bind (·.job)

def incomingJob (plugin : Plugin) (reviewed : Option Job) (sources : List Source) : Option Job :=
  if plugin.runsJobs then
    match reviewed with
    | some job => some job
    | none => if sources.isEmpty then none else some (defaultJob plugin)
  else none

/-- The job block and Agent targets a wiring leaves, or `none` when the block
it would write is not valid. -/
def wiredJob (surface : Surface) (plugin : Plugin) (reviewed : Option Job)
    (sources : List Source) : Option (Option Job × List String) :=
  match incomingJob plugin reviewed sources with
  | none => some (priorJob surface plugin, surface.targets)
  | some incoming =>
    let base := match priorJob surface plugin with
      | some existing => merge existing incoming
      | none => incoming
    let wired := sources.foldl wireSource (base, surface.targets)
    if wired.1.valid then some (some wired.1, addMissing wired.2 wired.1.agentTargets) else none

def wire (surface : Surface) (plugin : Plugin) (reviewed : Option Job) (skills : List String)
    (sources : List Source) : Option Outcome :=
  (wiredJob surface plugin reviewed sources).map fun wired =>
    { surface :=
        { surface with
          plugins := pinEntry plugin wired.1 surface.plugins
          targets := wired.2
          skills := surface.skills ++ skills.filter (· ∉ surface.skills) }
      job := wired.1
      warnings :=
        (if (incomingJob plugin reviewed sources).isSome
          then (sources.filter (!·.operatorInstalled)).map fun _ => .trustedPackNotOperator else [])
        ++ (if wired.1.any (·.policy.dynamic = .check) then [.dynamicCheck] else [])
        ++ (if surface.enabled = some true ∧ ¬ (wired.2.filter (· ∉ surface.targets)).isEmpty
          then [.agentNewWidened] else [])
      nextCall := plugin.runsJobs && wired.1.isNone }

def setJob (coordinate : String) (job : Option Job) : List Entry → List Entry
  | [] => []
  | entry :: rest =>
    if entry.plugin = coordinate then { entry with job } :: rest
    else entry :: setJob coordinate job rest

def namedBy (entries : List Entry) (target : String) : Bool :=
  entries.any fun entry =>
    entry.job.any fun job => target ∈ job.agentTargets || target ∈ job.policy.dynamicTargets

/-- Unwiring one pack takes its targets out of the job and stops trusting it,
unless it is the plugin's own pack. The second component collects the targets
taken out. -/
def unwireSource (ownerPack : Option String) (state : Job × List String) (source : Source) :
    Job × List String :=
  ({ state.1 with
      agentTargets := state.1.agentTargets.filter (· ∉ source.targets)
      policy :=
        { state.1.policy with
          trustedPacks :=
            if ownerPack = some source.pack then state.1.policy.trustedPacks
            else state.1.policy.trustedPacks.filter (· ≠ source.pack) } },
    state.2 ++ state.1.agentTargets.filter (· ∈ source.targets))

/-- The Agent targets left when `dropped` leave a job block: a dropped target
stays only while another plugin's block still names it. -/
def releaseTargets (others : List Entry) (dropped targets : List String) : List String :=
  targets.filter fun target => !(target ∈ dropped && !namedBy others target)

def quiet (surface : Surface) : Outcome :=
  { surface, job := none, warnings := [], nextCall := false }

/-- With no sources the plugin entry goes, and with it the targets of its job
block and, when it was the last entry, the skills. With sources the entry
stays and only those packs' targets and trust go; a block left without targets
is dropped. `none` when the narrowed block is not valid. -/
def unwire (surface : Surface) (plugin : Plugin) (skills : List String) (sources : List Source) :
    Option Outcome :=
  match surface.plugins.find? (·.plugin = plugin.coordinate) with
  | none => some { quiet surface with warnings := [.notWired] }
  | some entry =>
    let others := surface.plugins.eraseP (·.plugin = plugin.coordinate)
    if sources.isEmpty then
      some (quiet
        { surface with
          plugins := others
          targets :=
            releaseTargets others ((entry.job.map (·.agentTargets)).getD []) surface.targets
          skills := if others.isEmpty then surface.skills.filter (· ∉ skills) else surface.skills })
    else
      match entry.job with
      | none => some (quiet surface)
      | some block =>
        let narrowed := sources.foldl (unwireSource plugin.ownerPack) (block, [])
        let job : Job :=
          { narrowed.1 with
            policy :=
              { narrowed.1.policy with
                dynamicTargets :=
                  narrowed.1.policy.dynamicTargets.filter (· ∈ narrowed.1.agentTargets) } }
        let targets := releaseTargets others narrowed.2 surface.targets
        if job.agentTargets.isEmpty then
          some { quiet
            { surface with plugins := setJob plugin.coordinate none surface.plugins, targets } with
            nextCall := true }
        else if job.valid then
          some { quiet
            { surface with plugins := setJob plugin.coordinate (some job) surface.plugins, targets } with
            job := some job }
        else none

/-- The node owner's consent to dynamic scripts raises the plugin's block from
`check` to `run` and nothing else: `off` stays off. -/
def consentDynamic (coordinate : String) (outcome : Outcome) : Outcome :=
  match (outcome.surface.plugins.find? (·.plugin = coordinate)).bind (·.job) with
  | some job =>
    if job.policy.dynamic = .check then
      let raised : Job := { job with policy := { job.policy with dynamic := .run } }
      { outcome with
        surface :=
          { outcome.surface with
            plugins := setJob coordinate (some raised) outcome.surface.plugins }
        job := some raised
        warnings := outcome.warnings.filter (· ≠ .dynamicCheck) }
    else outcome
  | none => outcome

def grantToolCalls (coordinate : String) : List Entry → List Entry
  | [] => []
  | entry :: rest =>
    if entry.plugin = coordinate && !entry.toolCalls then { entry with toolCalls := true } :: rest
    else entry :: grantToolCalls coordinate rest

/-- The node owner's consent to the plugin calling the Agent's own tools. A
pack cannot ship that grant for an Agent it does not own. -/
def consentToolCalls (coordinate : String) (outcome : Outcome) : Outcome :=
  { outcome with
    surface :=
      { outcome.surface with plugins := grantToolCalls coordinate outcome.surface.plugins } }

/-! ## What wiring cannot do -/

theorem wire_keeps_enabled (surface : Surface) (plugin : Plugin) (reviewed : Option Job)
    (skills : List String) (sources : List Source) (outcome : Outcome)
    (h : wire surface plugin reviewed skills sources = some outcome) :
    outcome.surface.enabled = surface.enabled := by
  simp only [wire, Option.map_eq_some'] at h
  obtain ⟨_, _, rfl⟩ := h
  rfl

theorem unwire_keeps_enabled (surface : Surface) (plugin : Plugin) (skills : List String)
    (sources : List Source) (outcome : Outcome)
    (h : unwire surface plugin skills sources = some outcome) :
    outcome.surface.enabled = surface.enabled := by
  unfold unwire at h
  split at h
  · simp only [Option.some.injEq] at h; subst h; rfl
  · split at h
    · simp only [Option.some.injEq] at h; subst h; rfl
    · split at h
      · simp only [Option.some.injEq] at h; subst h; rfl
      · dsimp only at h
        split at h
        · simp only [Option.some.injEq] at h; subst h; rfl
        · split at h
          · simp only [Option.some.injEq] at h; subst h; rfl
          · exact absurd h (by simp)

/-- A later wiring never widens the block an earlier one left: every bound
falls or stays, the dynamic mode falls or stays, and dynamic scripts reach no
target they could not reach before. -/
theorem merge_never_widens (existing incoming : Job) :
    (merge existing incoming).maxLive ≤ existing.maxLive
      ∧ (merge existing incoming).maxSpawns ≤ existing.maxSpawns
      ∧ (merge existing incoming).maxMessagesPerSlot ≤ existing.maxMessagesPerSlot
      ∧ (merge existing incoming).resultBytes ≤ existing.resultBytes
      ∧ (merge existing incoming).maxWallSecs ≤ existing.maxWallSecs
      ∧ (merge existing incoming).policy.dynamic.rank ≤ existing.policy.dynamic.rank
      ∧ (merge existing incoming).policy.dynamicTargets ⊆ existing.policy.dynamicTargets := by
  refine ⟨Nat.min_le_left _ _, Nat.min_le_left _ _, Nat.min_le_left _ _, Nat.min_le_left _ _,
    Nat.min_le_left _ _, ?_, (List.filter_sublist _).subset⟩
  simp only [merge]
  split
  · omega
  · exact Nat.le_refl _

theorem merge_keeps_off (existing incoming : Job) (h : existing.policy.dynamic = .off) :
    (merge existing incoming).policy.dynamic = .off := by
  simp [merge, h, Dynamic.rank]

/-- Consent never enables dynamic scripts on a block that is `off`. -/
theorem consent_keeps_off (coordinate : String) (outcome : Outcome) (job : Job)
    (h : (outcome.surface.plugins.find? (·.plugin = coordinate)).bind (·.job) = some job)
    (hoff : job.policy.dynamic = .off) :
    consentDynamic coordinate outcome = outcome := by
  simp [consentDynamic, h, hoff]

/-! ## What unwiring takes back -/

theorem releaseTargets_subset (others : List Entry) (dropped targets : List String) :
    releaseTargets others dropped targets ⊆ targets :=
  (List.filter_sublist _).subset

/-- A target another plugin's job block still names survives. -/
theorem releaseTargets_keeps_named (others : List Entry) (dropped targets : List String)
    (target : String) (hmem : target ∈ targets) (hnamed : namedBy others target = true) :
    target ∈ releaseTargets others dropped targets := by
  simp [releaseTargets, List.mem_filter, hmem, hnamed]

/-- A target no wiring dropped survives. -/
theorem releaseTargets_keeps_others (others : List Entry) (dropped targets : List String)
    (target : String) (hmem : target ∈ targets) (hkept : target ∉ dropped) :
    target ∈ releaseTargets others dropped targets := by
  simp [releaseTargets, List.mem_filter, hmem, hkept]

/-- Unwiring never gives an Agent a target. -/
theorem unwire_never_adds_targets (surface : Surface) (plugin : Plugin) (skills : List String)
    (sources : List Source) (outcome : Outcome)
    (h : unwire surface plugin skills sources = some outcome) :
    outcome.surface.targets ⊆ surface.targets := by
  unfold unwire at h
  split at h
  · simp only [Option.some.injEq] at h; subst h; exact List.Subset.refl _
  · split at h
    · simp only [Option.some.injEq] at h; subst h; exact releaseTargets_subset _ _ _
    · split at h
      · simp only [Option.some.injEq] at h; subst h; exact List.Subset.refl _
      · dsimp only at h
        split at h
        · simp only [Option.some.injEq] at h; subst h; exact releaseTargets_subset _ _ _
        · split at h
          · simp only [Option.some.injEq] at h; subst h; exact releaseTargets_subset _ _ _
          · exact absurd h (by simp)

theorem unwireSource_keeps_dynamic (ownerPack : Option String) (sources : List Source)
    (state : Job × List String) :
    (sources.foldl (unwireSource ownerPack) state).1.policy.dynamic = state.1.policy.dynamic := by
  induction sources generalizing state with
  | nil => rfl
  | cons source rest ih => simp only [List.foldl_cons]; rw [ih]; rfl

theorem unwireSource_narrows (ownerPack : Option String) (sources : List Source)
    (state : Job × List String) :
    (sources.foldl (unwireSource ownerPack) state).1.agentTargets ⊆ state.1.agentTargets := by
  induction sources generalizing state with
  | nil => exact List.Subset.refl _
  | cons source rest ih =>
    simp only [List.foldl_cons]
    exact List.Subset.trans (ih _) (List.filter_sublist _).subset

/-! ## Wiring and unwiring one plugin on an Agent with no other -/

theorem addMissing_filter (keep : String → Bool) (items list : List String)
    (h : ∀ item ∈ items, keep item = false) :
    (addMissing list items).filter keep = list.filter keep := by
  induction items generalizing list with
  | nil => rfl
  | cons item rest ih =>
    have hrest : ∀ item ∈ rest, keep item = false := fun i hi => h i (List.mem_cons_of_mem _ hi)
    show (addMissing (if item ∈ list then list else list ++ [item]) rest).filter keep = _
    rw [ih _ hrest]
    split
    · rfl
    · simp [h item (List.mem_cons_self _ _)]

/-- On an Agent with no plugin, wiring a reviewed block and unwiring it again
gives back the Agent's plugins, targets and skills, provided the block's
targets and the skills were not already the Agent's.

The proviso is the limit of the rule: unwiring does not know which targets
the Agent held before the wiring, so a target it held and the block also
named leaves with the block. -/
theorem unwire_wire_restores (surface : Surface) (plugin : Plugin) (job : Job)
    (skills : List String) (outcome : Outcome)
    (hplugins : surface.plugins = []) (hjobs : plugin.runsJobs = true)
    (htargets : ∀ target ∈ job.agentTargets, target ∉ surface.targets)
    (hskills : ∀ skill ∈ skills, skill ∉ surface.skills)
    (h : wire surface plugin (some job) skills [] = some outcome) :
    (unwire outcome.surface plugin skills []).map (·.surface) = some surface := by
  have hw : wiredJob surface plugin (some job) []
      = if job.valid then some (some job, addMissing surface.targets job.agentTargets) else none := by
    simp [wiredJob, incomingJob, hjobs, priorJob, hplugins]
  unfold wire at h
  rw [hw] at h
  by_cases hvalid : job.valid = true
  · simp only [hvalid, if_true, Option.map_some', Option.some.injEq] at h
    subst h
    have hfilter : skills.filter (· ∉ surface.skills) = skills := by
      rw [List.filter_eq_self]; intro skill hskill; simpa using hskills skill hskill
    have hback : (surface.skills ++ skills).filter (· ∉ skills) = surface.skills := by
      rw [List.filter_append]
      have hnone : skills.filter (· ∉ skills) = [] := by
        rw [List.filter_eq_nil_iff]; intro skill hskill; simpa using hskill
      have hall : surface.skills.filter (· ∉ skills) = surface.skills := by
        rw [List.filter_eq_self]; intro skill hskill
        simp only [decide_eq_true_eq]
        intro hin; exact hskills skill hin hskill
      rw [hnone, hall, List.append_nil]
    have htargetsBack :
        releaseTargets [] job.agentTargets (addMissing surface.targets job.agentTargets)
          = surface.targets := by
      unfold releaseTargets
      rw [addMissing_filter _ _ _ (by intro item hitem; simp [namedBy, hitem])]
      rw [List.filter_eq_self]
      intro target htarget
      simp only [namedBy, List.any_nil, Bool.not_false, Bool.and_true, Bool.not_eq_true',
        decide_eq_false_iff_not]
      intro hin; exact htargets target hin htarget
    simp only [unwire, pinEntry, hplugins, List.find?_cons, decide_true, List.isEmpty_nil,
      if_true, List.eraseP_cons, List.isEmpty_iff, Option.map_some', Option.some.injEq,
      quiet, Option.map, Option.getD, hfilter, hback, htargetsBack]
    cases surface
    simp_all
  · simp [hvalid] at h

/-- The limit of `unwire_wire_restores`, concretely: an Agent that could
already start `reviewer` loses it when a plugin whose block also named
`reviewer` is unwired. -/
theorem unwire_takes_a_target_held_before :
    let surface : Surface := { plugins := [], targets := ["reviewer"], enabled := none, skills := [] }
    let plugin : Plugin :=
      { coordinate := "acme/runner", digest := "sha256:aa", runsJobs := true, ownerPack := none }
    let job : Job := { defaultJob plugin with agentTargets := ["reviewer"] }
    ((wire surface plugin (some job) [] []).bind fun wired =>
      (unwire wired.surface plugin [] []).map (·.surface.targets)) = some [] := by
  decide

end Pack.PluginWiring
