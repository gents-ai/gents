import Batteries.Tactic.OpenPrivate
import Proofs.PluginJob.Adoption
import Proofs.ToolPolicy.PluginCalls

open private admitStep_pinned liveCount_addSpawns from Proofs.PluginJob.Admission

/-!
# Plugin-job steps

A step's spawns, interrupts, pin and progress are idempotent: spawns are keyed,
interrupts latch, the first pin wins, and progress is last-writer-wins under a
revision the host owns. A duplicate runtime that applies the same step twice
therefore adds no spawn, interrupt, pin or progress change. Messages are not
deduplicated: a duplicated message is bounded by `max_messages_per_slot`. A
stale step of a prefix-monotone guest adds nothing to a fresher
one. `jobMeasure` decreases with every admitted spawn or message and every
observed child terminal, so a job admits finitely many steps. A step
invocation that fails three times in a row ends the row `failed` through the
kill path, and steps after step 0 run outside the owned loop, which is the
only live dispatcher.
-/

namespace PluginJob.Step

/-- The guest's `JobProgress`. Its `revision` is the guest's value, which the
host ignores. -/
structure GuestProgress where
  body : String
  revision : Nat
  deriving DecidableEq, Repr

/-- What one admitted step writes to its row: its effects, the host facts that
complete a guest pin, and the guest's progress, if any. -/
structure StepWrite where
  effects : Effects
  facts : HostFacts
  progress : Option GuestProgress

/-- The job row a step writes: the job, the slots whose interrupt has latched,
and the stored `job_progress` body with the revision the host assigned. -/
structure Row where
  job : Job
  latched : List String
  progress : Option String
  revision : Nat
  /-- The guest's own state, stored verbatim and never read by the host. -/
  guest : Option String
  deriving DecidableEq

/-- The first pin wins: a stored pin is kept, whatever a later step carries. -/
def firstPin (stored incoming : Option Pin) : Option Pin :=
  match stored with
  | some p => some p
  | none => incoming

/-- The host pin a step's guest pin would be written as. -/
def incomingPin (w : StepWrite) : Option Pin :=
  w.effects.pin.map (hostPin · w.facts)

/-- Keyed spawns and the first-wins pin. Messages are not replayed here:
admission bounds them per slot (`messagesAdmitted`). -/
def applyJob (w : StepWrite) (job : Job) : Job :=
  { addSpawns job (freshSpawns job w.effects.spawns) with
    pin := firstPin job.pin (incomingPin w) }

/-- An interrupt latches: a slot already latched is not latched again. -/
def latch (latched slots : List String) : List String :=
  latched ++ slots.filter (· ∉ latched)

/-- Last writer wins and is written only on change; the host bumps the
revision it stored and ignores the guest's. -/
def writeProgress (stored : Option String) (revision : Nat) :
    Option GuestProgress → Option String × Nat
  | none => (stored, revision)
  | some g => if stored = some g.body then (stored, revision) else (some g.body, revision + 1)

def apply (w : StepWrite) (r : Row) : Row :=
  { job := applyJob w r.job
    latched := latch r.latched w.effects.interrupts
    progress := (writeProgress r.progress r.revision w.progress).1
    revision := (writeProgress r.progress r.revision w.progress).2
    guest := r.guest }

/-- A step's guest state replaces the stored one when given and keeps it when
absent; it is written beside the step's other records and read by no host rule. -/
def applyGuest (incoming : Option String) (r : Row) : Row :=
  { r with guest := incoming <|> r.guest }

/-- The guest state is opaque: writing it changes no other field of the row, a
step's effects never change it, and an absent write keeps the stored state. -/
theorem guest_state_opaque (incoming : Option String) (w : StepWrite) (r : Row) :
    (applyGuest incoming r).job = r.job ∧ (applyGuest incoming r).latched = r.latched ∧
      (applyGuest incoming r).progress = r.progress ∧
      (applyGuest incoming r).revision = r.revision ∧
      (applyGuest incoming r).guest = (incoming <|> r.guest) ∧
      (apply w r).guest = r.guest ∧ applyGuest none r = r := by
  refine ⟨rfl, rfl, rfl, rfl, rfl, rfl, ?_⟩
  simp [applyGuest]

inductive PinWrite | commit | replay | conflict
  deriving DecidableEq, Repr

/-- `job_pin` is written once: an identical replay is accepted and any other
value conflicts, the `PluginReceipt.decide` shape restated for the pin. -/
def pinWrite (stored : Option Pin) (incoming : Pin) : PinWrite :=
  match stored with
  | none => .commit
  | some p => if p = incoming then .replay else .conflict

/-- Pack premise (`boundary.plugin-job.guest-determinism`): what a guest emits
on an observation prefix it also emits on every extension of that prefix. -/
def PrefixMonotone {α : Type} (guest : List α → Effects) : Prop :=
  ∀ P P' : List α, P <+: P' →
    (∀ s ∈ (guest P).spawns, s ∈ (guest P').spawns) ∧
      (∀ x ∈ (guest P).interrupts, x ∈ (guest P').interrupts) ∧
      ((guest P).pin.isSome → (guest P').pin.isSome)

/-- The driver steps a running row when a child terminal is unobserved, or
when a control or resume is pending. -/
def wakes (job : Job) (controlOrResume : Bool) : Bool :=
  job.state == .running && (decide (job.observedTerminal < terminalCount job) || controlOrResume)

def ownChildren (job : Job) : List Child :=
  job.children.filter fun ch => ch.key.root == job.root

def remainingSpawns (budget : Nat) (job : Job) : Nat := budget - job.spent

/-- `cap` messages for every spawn still allowed, plus what each own child has
left under `cap`, so a spawn moves allowance from one term to the other. -/
def remainingMessages (budget cap : Nat) (job : Job) : Nat :=
  cap * remainingSpawns budget job + ((ownChildren job).map fun ch => cap - ch.messages).sum

/-- μ = 2·remainingSpawns + 2·remainingMessages + live, for the pinned
agent budget and the per-slot message cap. -/
def jobMeasure (budget cap : Nat) (job : Job) : Nat :=
  2 * remainingSpawns budget job + 2 * remainingMessages budget cap job + liveCount job

def maxConsecutiveStepFailures : Nat := 3

/-- One failed step invocation: a guest trap, budget exhaustion or a malformed
output. The third in a row raises `.stepFailed`: the row ends `failed` and
`Control.kill` runs over its own children, as for any refusal (`settleStep`). -/
def onInvocationFailure (job : Job) (obs : SpawnKey → Recovery.KillObservation) :
    Job × List (SpawnKey × Recovery.KillAction) × Option Refusal :=
  if maxConsecutiveStepFailures ≤ job.consecutiveFailures + 1 then
    ((terminalizeRow { job with consecutiveFailures := job.consecutiveFailures + 1 } .failed).1,
      (Control.kill job .stop obs).2, some .stepFailed)
  else ({ job with consecutiveFailures := job.consecutiveFailures + 1 }, [], none)

private theorem firstOfKey_covers (key : Spawn → String) :
    ∀ (seen : List String) (l : List Spawn) (s : Spawn), s ∈ l →
      key s ∈ seen ∨ ∃ t ∈ firstOfKey key seen l, key t = key s := by
  intro seen l
  induction l generalizing seen with
  | nil => intro s hs; simp at hs
  | cons x rest ih =>
    intro s hs
    by_cases hx : key x ∈ seen
    · simp only [firstOfKey, hx, if_true]
      rcases List.mem_cons.mp hs with rfl | hs
      · exact Or.inl hx
      · exact ih seen s hs
    · simp only [firstOfKey, hx, if_false]
      rcases List.mem_cons.mp hs with rfl | hs
      · exact Or.inr ⟨s, List.mem_cons_self _ _, rfl⟩
      · rcases ih (key x :: seen) s hs with h | ⟨t, ht, he⟩
        · rcases List.mem_cons.mp h with h | h
          · exact Or.inr ⟨x, List.mem_cons_self _ _, h.symm⟩
          · exact Or.inl h
        · exact Or.inr ⟨t, List.mem_cons_of_mem _ ht, he⟩

/-- After a step, every spawn it named has a child with its retry key. -/
private theorem spawns_known_after (w : StepWrite) (job : Job) :
    ∀ s ∈ w.effects.spawns, known (applyJob w job) (spawnKey (applyJob w job) s) = true := by
  intro s hs
  show known (applyJob w job) (spawnKey job s) = true
  have hch : (applyJob w job).children = job.children ++
      (freshSpawns job w.effects.spawns).map fun t =>
        ({ key := spawnKey job t, target := t.target, state := .live, messages := 0 } : Child) :=
    rfl
  simp only [known, List.any_eq_true, beq_iff_eq]
  rw [hch]
  by_cases hkn : known job (spawnKey job s) = true
  · simp only [known, List.any_eq_true, beq_iff_eq] at hkn
    obtain ⟨ch, hm, he⟩ := hkn
    exact ⟨ch, List.mem_append_left _ hm, he⟩
  · have hmem : s ∈ w.effects.spawns.filter fun t => !known job (spawnKey job t) :=
      List.mem_filter.mpr ⟨hs, by simpa using hkn⟩
    rcases firstOfKey_covers (fun t => (spawnKey job t).retryKey) [] _ s hmem with h | ⟨t, ht, he⟩
    · simp at h
    · exact ⟨_, List.mem_append_right _ (List.mem_map_of_mem _ ht), he⟩

private theorem freshSpawns_nil_of_known (job : Job) (spawns : List Spawn)
    (h : ∀ s ∈ spawns, known job (spawnKey job s) = true) : freshSpawns job spawns = [] := by
  have hf : spawns.filter (fun s => !known job (spawnKey job s)) = [] :=
    List.filter_eq_nil_iff.mpr fun s hs => by simp [h s hs]
  unfold freshSpawns
  rw [hf]
  simp only [firstOfKey]

private theorem addSpawns_nil (job : Job) : addSpawns job [] = job := by
  cases job
  simp [addSpawns]

/-- A step whose spawns and pin a step already applied to the row hold adds
nothing to the job. -/
private theorem applyJob_subsumed (w w' : StepWrite) (job : Job)
    (hs : ∀ s ∈ w.effects.spawns, s ∈ w'.effects.spawns)
    (hp : w.effects.pin.isSome → w'.effects.pin.isSome) :
    applyJob w (applyJob w' job) = applyJob w' job := by
  have hfresh : freshSpawns (applyJob w' job) w.effects.spawns = [] :=
    freshSpawns_nil_of_known _ _ fun s hm => spawns_known_after w' job s (hs s hm)
  have hpin : firstPin (applyJob w' job).pin (incomingPin w) = (applyJob w' job).pin := by
    show firstPin (firstPin job.pin (incomingPin w')) (incomingPin w) = firstPin job.pin (incomingPin w')
    cases job.pin with
    | some p => rfl
    | none =>
      cases h' : w'.effects.pin with
      | some g' => simp [firstPin, incomingPin, h']
      | none =>
        cases h : w.effects.pin with
        | some g => simp [h, h'] at hp
        | none => simp [firstPin, incomingPin, h, h']
  show { addSpawns (applyJob w' job) (freshSpawns (applyJob w' job) w.effects.spawns) with
      pin := firstPin (applyJob w' job).pin (incomingPin w) } = applyJob w' job
  rw [hfresh, addSpawns_nil, hpin]

private theorem latch_subsumed (l xs xs' : List String) (h : ∀ x ∈ xs, x ∈ xs') :
    latch (latch l xs') xs = latch l xs' := by
  have hc : ∀ x ∈ xs, x ∈ latch l xs' := fun x hx => by
    by_cases hl : x ∈ l
    · exact List.mem_append_left _ hl
    · exact List.mem_append_right _ (List.mem_filter.mpr ⟨h x hx, by simpa using hl⟩)
  have hf : xs.filter (· ∉ latch l xs') = [] :=
    List.filter_eq_nil_iff.mpr fun s hs => by simp [hc s hs]
  show latch l xs' ++ xs.filter (· ∉ latch l xs') = latch l xs'
  rw [hf, List.append_nil]

private theorem writeProgress_idem (s : Option String) (v : Nat) (g : Option GuestProgress) :
    writeProgress (writeProgress s v g).1 (writeProgress s v g).2 g = writeProgress s v g := by
  cases g with
  | none => rfl
  | some g =>
    by_cases h : s = some g.body
    · simp [writeProgress, h]
    · simp [writeProgress, h]

/-- Applying a step's writes twice is applying them once: spawns are keyed,
interrupts latch, the first pin wins, and an unchanged progress body is not
rewritten, so its revision does not move. -/
theorem step_effects_idempotent (w : StepWrite) (r : Row) :
    Step.apply w (Step.apply w r) = Step.apply w r := by
  have hj := applyJob_subsumed w w r.job (fun _ h => h) id
  have hl := latch_subsumed r.latched w.effects.interrupts w.effects.interrupts (fun _ h => h)
  have hp := writeProgress_idem r.progress r.revision w.progress
  simp only [Step.apply, hj, hl, hp]

/-- A stored pin is kept, whatever a later step carries. As a write, an
identical pin replays and any other pin conflicts. -/
theorem pin_first_wins (w : StepWrite) (r : Row) (p q : Pin) (h : r.job.pin = some p) :
    (Step.apply w r).job.pin = some p ∧ (pinWrite (some p) q = .replay ↔ q = p) ∧
      (q ≠ p → pinWrite (some p) q = .conflict) ∧ pinWrite none q = .commit := by
  refine ⟨?_, ?_, fun hq => ?_, rfl⟩
  · show firstPin r.job.pin (incomingPin w) = some p
    rw [h]
    rfl
  · by_cases hq : q = p
    · subst hq
      simp [pinWrite]
    · simp [pinWrite, hq, Ne.symm hq]
  · simp [pinWrite, Ne.symm hq]

/-- A changed progress body is stored with revision `prior + 1`, and the
revision the guest sent changes nothing. -/
theorem progress_revision_host_owned (w : StepWrite) (r : Row) (g : GuestProgress) (n : Nat)
    (h_w : w.progress = some g) (h_changed : r.progress ≠ some g.body) :
    (Step.apply w r).progress = some g.body ∧ (Step.apply w r).revision = r.revision + 1 ∧
      Step.apply { w with progress := some { g with revision := n } } r = Step.apply w r := by
  obtain ⟨e, f, q⟩ := w
  simp only at h_w
  subst h_w
  refine ⟨?_, ?_, ?_⟩ <;>
    simp [Step.apply, applyJob, incomingPin, writeProgress, h_changed]

/-- Under the prefix-monotone premise, the effects of a stale step, computed
on a prefix of what a fresher step observed, add nothing to the job or its
latched interrupts once the fresher step is applied. Progress is
last-writer-wins and is not subsumed. -/
theorem stale_step_subsumed {α : Type} (guest : List α → Effects) (h_mono : PrefixMonotone guest)
    (P P' : List α) (h_prefix : P <+: P') (facts facts' : HostFacts)
    (progress progress' : Option GuestProgress) (r : Row) :
    (Step.apply ⟨guest P, facts, progress⟩ (Step.apply ⟨guest P', facts', progress'⟩ r)).job =
        (Step.apply ⟨guest P', facts', progress'⟩ r).job ∧
      (Step.apply ⟨guest P, facts, progress⟩ (Step.apply ⟨guest P', facts', progress'⟩ r)).latched =
        (Step.apply ⟨guest P', facts', progress'⟩ r).latched := by
  obtain ⟨hs, hi, hp⟩ := h_mono P P' h_prefix
  exact ⟨applyJob_subsumed ⟨guest P, facts, progress⟩ ⟨guest P', facts', progress'⟩ r.job hs hp,
    latch_subsumed r.latched (guest P).interrupts (guest P').interrupts hi⟩

/-- An admitted effects step observes every child terminal, so the driver
steps the row again only once a further terminal is observed or a control or
resume is pending. -/
theorem wake_requires_observation_growth {ctx : StepContext} {job job' : Job} {e : Effects}
    {p : Pin} (h : admitStep ctx job (.effects e) = .ok job')
    (hp : stepPin ctx job (.effects e) = some p) :
    job'.observedTerminal = terminalCount job' ∧
      ∀ (later : Job) (controlOrResume : Bool), later.observedTerminal = job'.observedTerminal →
        wakes later controlOrResume = true →
          job'.observedTerminal < terminalCount later ∨ controlOrResume = true := by
  refine ⟨?_, fun later c hobs hw => ?_⟩
  · have h2 : admitEffects ctx p { stepRow ctx job (.effects e) with pin := some p, consecutiveFailures := 0 } e =
        .ok job' :=
      admitStep_pinned h hp
    unfold admitEffects at h2
    dsimp only at h2
    split at h2
    · cases h2
    · split_ifs at h2
      cases h2
      rfl
  · simp only [wakes, Bool.and_eq_true, beq_iff_eq, Bool.or_eq_true, decide_eq_true_eq] at hw
    rw [← hobs]
    exact hw.2

private theorem sum_map_le {α : Type} (f g : α → Nat) :
    ∀ l : List α, (∀ x ∈ l, g x ≤ f x) → (l.map g).sum ≤ (l.map f).sum
  | [], _ => Nat.le_refl _
  | y :: ys, h => by
    simp only [List.map_cons, List.sum_cons]
    have h1 := sum_map_le f g ys fun x hx => h x (List.mem_cons_of_mem _ hx)
    have h2 := h y (List.mem_cons_self _ _)
    omega

private theorem sum_map_succ_le {α : Type} (f g : α → Nat) :
    ∀ l : List α, (∀ x ∈ l, g x ≤ f x) → ∀ x0 ∈ l, g x0 + 1 ≤ f x0 →
      (l.map g).sum + 1 ≤ (l.map f).sum
  | [], _, x0, hx0, _ => by simp at hx0
  | y :: ys, h, x0, hx0, hlt => by
    simp only [List.map_cons, List.sum_cons]
    have hys := sum_map_le f g ys fun x hx => h x (List.mem_cons_of_mem _ hx)
    have hy := h y (List.mem_cons_self _ _)
    rcases List.mem_cons.mp hx0 with rfl | hx0
    · omega
    · have := sum_map_succ_le f g ys (fun x hx => h x (List.mem_cons_of_mem _ hx)) x0 hx0 hlt
      omega

private theorem foldl_mem (F : Option Child → Child → Option Child)
    (hF : ∀ acc x, F acc x = acc ∨ F acc x = some x) :
    ∀ (l : List Child) (acc : Option Child) (ch : Child),
      l.foldl F acc = some ch → acc = some ch ∨ ch ∈ l := by
  intro l
  induction l with
  | nil => intro acc ch h; exact Or.inl h
  | cons x xs ih =>
    intro acc ch h
    rcases ih (F acc x) ch h with h' | h'
    · rcases hF acc x with hx | hx
      · exact Or.inl (hx ▸ h')
      · rw [hx] at h'
        cases h'
        exact Or.inr (List.mem_cons_self _ _)
    · exact Or.inr (List.mem_cons_of_mem _ h')

private theorem latestAttempt_mem {job : Job} {slot : String} {ch : Child}
    (h : latestAttempt job slot = some ch) : ch ∈ job.children ∧ ch.key.root = job.root := by
  unfold latestAttempt at h
  have hF := foldl_mem _ (fun acc x => by
    rcases acc with _ | a
    · exact Or.inr rfl
    · by_cases hlt : a.key.attempt < x.key.attempt
      · exact Or.inr (by simp [hlt])
      · exact Or.inl (by simp [hlt])) _ none ch h
  rcases hF with h' | h'
  · cases h'
  · simp only [List.mem_filter, Bool.and_eq_true, beq_iff_eq] at h'
    exact ⟨h'.1, h'.2.1⟩

private theorem msgSum_map (root : DocId) (cap : Nat) (φ : Child → Child)
    (hk : ∀ c, (φ c).key = c.key) (hm : ∀ c, (φ c).messages = c.messages) :
    ∀ l : List Child,
      (((l.map φ).filter fun c => c.key.root == root).map fun c => cap - c.messages).sum =
        ((l.filter fun c => c.key.root == root).map fun c => cap - c.messages).sum
  | [] => rfl
  | x :: xs => by
    have ih := msgSum_map root cap φ hk hm xs
    simp only [List.map_cons, List.filter_cons, hk]
    by_cases h : (x.key.root == root) = true
    · simp only [h, ↓reduceIte, List.map_cons, List.sum_cons, hm, ih]
    · simp only [h, Bool.false_eq_true, ↓reduceIte, ih]

private theorem live_observe (root : DocId) (k : SpawnKey) (s : ChildState) (hs : s ≠ .live) :
    ∀ l : List Child, (l.map (·.key)).Nodup →
      (∃ ch ∈ l, ch.key = k ∧ ch.key.root = root ∧ ch.state = .live) →
      ((l.map fun ch => if ch.key = k then { ch with state := s } else ch).filter
          fun ch => ch.key.root == root && ch.state == .live).length + 1 =
        (l.filter fun ch => ch.key.root == root && ch.state == .live).length
  | [], _, ⟨_, hm, _⟩ => by simp at hm
  | x :: xs, hnd, ⟨ch, hm, hk, hroot, hlive⟩ => by
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at hnd
    obtain ⟨hx_notin, hnd⟩ := hnd
    by_cases hxk : x.key = k
    · have hch : ch = x := by
        rcases List.mem_cons.mp hm with rfl | hm
        · rfl
        · exact absurd ⟨ch, hm, hk.trans hxk.symm⟩ hx_notin
      have hx_root : x.key.root = root := hch ▸ hroot
      have hx_live : x.state = .live := hch ▸ hlive
      have hid : (xs.map fun c => if c.key = k then { c with state := s } else c) = xs := by
        conv_rhs => rw [← List.map_id xs]
        apply List.map_congr_left
        intro c hc
        have hck : c.key ≠ k := fun h => hx_notin ⟨c, hc, h.trans hxk.symm⟩
        simp [hck]
      have hs' : (s == ChildState.live) = false := by
        cases s <;> first | rfl | exact absurd rfl hs
      simp only [List.map_cons, if_pos hxk, hid, List.filter_cons, hx_root, hx_live, hs',
        beq_self_eq_true, Bool.and_true, Bool.and_false, ↓reduceIte, List.length_cons]
      simp
    · have hch_xs : ch ∈ xs := by
        rcases List.mem_cons.mp hm with rfl | hm
        · exact absurd hk hxk
        · exact hm
      have ih := live_observe root k s hs xs hnd ⟨ch, hch_xs, hk, hroot, hlive⟩
      simp only [List.map_cons, if_neg hxk, List.filter_cons]
      by_cases hp : (x.key.root == root && x.state == .live) = true
      · simp only [hp, ↓reduceIte, List.length_cons]
        omega
      · simp only [hp, ↓reduceIte]
        exact ih

private theorem live_relive (root : DocId) (k : SpawnKey) (φ : Child → Child)
    (hφ : ∀ c, c.key ≠ k → φ c = c) :
    ∀ l : List Child, (l.map (·.key)).Nodup → (∃ c ∈ l, c.key = k) →
      ((l.map φ).filter fun ch => ch.key.root == root && ch.state == .live).length ≤
        (l.filter fun ch => ch.key.root == root && ch.state == .live).length + 1
  | [], _, ⟨_, hm, _⟩ => by simp at hm
  | x :: xs, hnd, ⟨c, hm, hk⟩ => by
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at hnd
    obtain ⟨hx_notin, hnd⟩ := hnd
    by_cases hxk : x.key = k
    · have hid : xs.map φ = xs := by
        conv_rhs => rw [← List.map_id xs]
        apply List.map_congr_left
        intro y hy
        exact hφ y fun h => hx_notin ⟨y, hy, h.trans hxk.symm⟩
      simp only [List.map_cons, hid, List.filter_cons]
      split <;> split <;> (try simp only [List.length_cons]) <;> omega
    · have hc_xs : c ∈ xs := by
        rcases List.mem_cons.mp hm with rfl | hm
        · exact absurd hk hxk
        · exact hm
      have ih := live_relive root k φ hφ xs hnd ⟨c, hc_xs, hk⟩
      simp only [List.map_cons, hφ x hxk, List.filter_cons]
      split
      · simp only [List.length_cons]
        omega
      · exact ih

/-- Each admitted spawn lowers μ by at least 1 (rule 6 keeps `spent` within
the pinned budget), each admitted message lowers it by at least 1 (rule 7
keeps the slot under its cap: μ drops by 2 for the message and rises by at
most 1 for the slot it re-lives), and each observed terminal of a live own child
lowers it by exactly 1. -/
theorem admission_decreases_measure (budget cap : Nat) (job : Job) :
    (∀ s : Spawn, job.spent < budget →
        jobMeasure budget cap (addSpawns job [s]) + 1 ≤ jobMeasure budget cap job) ∧
      (∀ (slot : String) (ch : Child), (job.children.map (·.key)).Nodup →
        latestAttempt job slot = some ch → ch.messages < cap →
        jobMeasure budget cap (addMessages job [slot]) + 1 ≤ jobMeasure budget cap job) ∧
      (∀ (k : SpawnKey) (s : ChildState), (job.children.map (·.key)).Nodup → s ≠ .live →
        (∃ ch ∈ job.children, ch.key = k ∧ ch.key.root = job.root ∧ ch.state = .live) →
        jobMeasure budget cap (observeChild job k s) + 1 = jobMeasure budget cap job) := by
  refine ⟨fun s hlt => ?_, fun slot ch hnd hlat hlt => ?_, fun k s hnd hs hex => ?_⟩
  · have hown : ownChildren (addSpawns job [s]) =
        ownChildren job ++ [{ key := spawnKey job s, target := s.target, state := .live, messages := 0 }] := by
      simp [ownChildren, addSpawns, List.filter_append, List.filter_cons, spawnKey]
    have hlive : liveCount (addSpawns job [s]) = liveCount job + 1 := liveCount_addSpawns job [s]
    have hspent : (addSpawns job [s]).spent = job.spent + 1 := rfl
    obtain ⟨m, hm⟩ : ∃ m, budget - job.spent = m + 1 := ⟨budget - job.spent - 1, by omega⟩
    have hm' : budget - (job.spent + 1) = m := by omega
    simp only [jobMeasure, remainingMessages, remainingSpawns, hown, hlive, hspent, hm, hm',
      List.map_append, List.sum_append, List.map_cons, List.map_nil, List.sum_cons, List.sum_nil,
      Nat.sub_zero, Nat.mul_add, Nat.mul_one]
    omega
  · obtain ⟨hmem, hroot⟩ := latestAttempt_mem hlat
    have hown : ownChildren (addMessages job [slot]) = (ownChildren job).map fun c =>
        { c with
          messages := c.messages +
            ([slot].filter fun sl => (latestAttempt job sl).map (·.key) == some c.key).length,
          state := if ([slot].filter fun sl => (latestAttempt job sl).map (·.key) == some c.key).length = 0
            then c.state else .live } := by
      simp only [ownChildren, addMessages, List.filter_map]
      rfl
    have h1 : ([slot].filter fun sl => (latestAttempt job sl).map (·.key) == some ch.key).length = 1 := by
      simp [hlat]
    have hsum : ((ownChildren (addMessages job [slot])).map fun c => cap - c.messages).sum + 1 ≤
        ((ownChildren job).map fun c => cap - c.messages).sum := by
      rw [hown, List.map_map]
      refine sum_map_succ_le _ _ _ (fun x _ => ?_) ch
        (List.mem_filter.mpr ⟨hmem, by simp [hroot]⟩) ?_
      · simp only [Function.comp_apply]
        omega
      · show cap - (ch.messages +
          ([slot].filter fun sl => (latestAttempt job sl).map (·.key) == some ch.key).length) + 1 ≤
            cap - ch.messages
        rw [h1]
        omega
    have hlive : liveCount (addMessages job [slot]) ≤ liveCount job + 1 :=
      live_relive job.root ch.key _ (fun c hc => by simp [hc, Ne.symm hc, hlat]) job.children hnd
        ⟨ch, hmem, rfl⟩
    have hspent : (addMessages job [slot]).spent = job.spent := rfl
    simp only [jobMeasure, remainingMessages, remainingSpawns, hlive, hspent]
    omega
  · have hsum : ((ownChildren (observeChild job k s)).map fun c => cap - c.messages).sum =
        ((ownChildren job).map fun c => cap - c.messages).sum :=
      msgSum_map job.root cap (fun ch => if ch.key = k then { ch with state := s } else ch)
        (fun c => by by_cases h : c.key = k <;> simp [h])
        (fun c => by by_cases h : c.key = k <;> simp [h]) job.children
    have hlive : liveCount (observeChild job k s) + 1 = liveCount job :=
      live_observe job.root k s hs job.children hnd hex
    have hspent : (observeChild job k s).spent = job.spent := rfl
    simp only [jobMeasure, remainingMessages, remainingSpawns, hsum, hspent]
    omega

/-- A row whose engine digest differs from its pin, with no `engine_current`
resume, is refused with `plugin_job_engine_changed`: it ends `failed` and the
kill runs over its own children. -/
theorem engine_digest_mismatch_fails_closed (ctx : StepContext) (job : Job) (out : StepOutput)
    (obs : SpawnKey → Recovery.KillObservation) (p : Pin)
    (h_running : job.state = .running) (h_available : ctx.available = true)
    (h_operator : ctx.engineProvenanceOperator = true) (h_pin : job.pin = some p)
    (h_digest : ctx.engineDigest ≠ p.engineDigest)
    (h_current : ∀ r, out.resume = some r → r.engineCurrent = false) :
    admitStep ctx job out = .error .engineChanged ∧
      Refusal.code .engineChanged = some "plugin_job_engine_changed" ∧
      (settleStep ctx job out obs).1.state = .failed ∧
      (settleStep ctx job out obs).2 = (Control.kill job .stop obs).2 := by
  have h_changed : engineChanged ctx job out = true := by
    cases hr : out.resume with
    | none => simp [engineChanged, h_pin, h_digest, hr]
    | some r => simp [engineChanged, h_pin, h_digest, hr, h_current r hr]
  have h_admit : admitStep ctx job out = .error .engineChanged := by
    simp [admitStep, h_running, h_available, h_operator, h_changed]
  refine ⟨h_admit, rfl, ?_, ?_⟩
  · simp [settleStep, h_admit, terminalizeRow, h_running]
  · simp [settleStep, h_admit]

/-- The dispatcher a plugin invocation of `row` sees. Only the owned loop
supplies a live one, to the call it awaits in the foreground; natively an
invocation without it carries `host_tools: false` in its receipt. -/
def stepDispatcher (row : ToolExecution.ToolCallContext) : Bool :=
  decide (row.awaitMode = .foreground)

private theorem transition_keeps_operation {pre post : ToolExecution.ToolCallContext}
    (h : ToolExecution.ToolCallContext.Transition pre post) : post.operation = pre.operation := by
  cases h <;> subst_vars <;> rfl

private theorem plugin_job_trace_stays_background {c c' : ToolExecution.ToolCallContext}
    (h : ToolExecution.ToolCallContext.Trace c c') :
    c.operation = .pluginJob → c.awaitMode = .background →
      c'.operation = .pluginJob ∧ c'.awaitMode = .background := by
  induction h with
  | refl => exact fun h_op h_mode => ⟨h_op, h_mode⟩
  | step h_step _ ih =>
    exact fun h_op h_mode => ih ((transition_keeps_operation h_step).trans h_op)
      (ToolExecution.ToolCallContext.plugin_job_row_never_foregrounds h_op h_mode h_step)

/-- Step 0 is the owned loop's foreground call and keeps its Tools entry's
grant. Every later step is driven from the adopted row, which no trace returns
to the foreground, so it delegates no tool call, whatever its grant. -/
theorem job_step_has_no_dispatcher (row : ToolExecution.ToolCallContext) (pin : Pin)
    (e : Effects) (hg : e.pin.isSome = true) (g : Bool) :
    (row.awaitMode = .foreground →
        ToolPolicy.PluginCalls.available g (stepDispatcher row) = g) ∧
      ∀ post, ToolExecution.ToolCallContext.Trace (adopt row pin e).1 post →
        ToolPolicy.PluginCalls.available g (stepDispatcher post) = false := by
  have h_adopted : (adopt row pin e).1.operation = .pluginJob ∧
      (adopt row pin e).1.awaitMode = .background := by
    simp [adopt, hg]
  refine ⟨fun h_fg => by simp [ToolPolicy.PluginCalls.available, stepDispatcher, h_fg],
    fun post h_trace => ?_⟩
  obtain ⟨_, h_bg⟩ := plugin_job_trace_stays_background h_trace h_adopted.1 h_adopted.2
  simp [ToolPolicy.PluginCalls.available, stepDispatcher, h_bg]

private theorem onInvocationFailure_raise (job : Job) (obs : SpawnKey → Recovery.KillObservation)
    (h : maxConsecutiveStepFailures ≤ job.consecutiveFailures + 1) :
    onInvocationFailure job obs =
      ((terminalizeRow { job with consecutiveFailures := job.consecutiveFailures + 1 } .failed).1,
        (Control.kill job .stop obs).2, some .stepFailed) := by
  rw [onInvocationFailure, if_pos h]

private theorem onInvocationFailure_count (job : Job) (obs : SpawnKey → Recovery.KillObservation)
    (h : ¬ maxConsecutiveStepFailures ≤ job.consecutiveFailures + 1) :
    onInvocationFailure job obs = ({ job with consecutiveFailures := job.consecutiveFailures + 1 }, [], none) := by
  rw [onInvocationFailure, if_neg h]

/-- `onInvocationFailure` raises `.stepFailed` exactly when the consecutive
failures reach `maxConsecutiveStepFailures`; the row then ends `failed`, the
kill runs over its own children, and no further guest step is admitted. From
a row with no failures, the first two failures raise nothing and the third
raises `.stepFailed`. -/
theorem step_failed_after_three_consecutive_failures (job : Job) (h_running : job.state = .running)
    (obs : SpawnKey → Recovery.KillObservation) :
    ((onInvocationFailure job obs).2.2 = some .stepFailed ↔
        maxConsecutiveStepFailures ≤ job.consecutiveFailures + 1) ∧
      (maxConsecutiveStepFailures ≤ job.consecutiveFailures + 1 →
        (onInvocationFailure job obs).1.state = .failed ∧
          (onInvocationFailure job obs).2.1 = (Control.kill job .stop obs).2 ∧
          ∀ (ctx : StepContext) (out : StepOutput),
            admitStep ctx (onInvocationFailure job obs).1 out = .error .terminal) ∧
      (job.consecutiveFailures = 0 →
        (onInvocationFailure job obs).2.2 = none ∧
          (onInvocationFailure (onInvocationFailure job obs).1 obs).2.2 = none ∧
          (onInvocationFailure (onInvocationFailure (onInvocationFailure job obs).1 obs).1
            obs).2.2 = some .stepFailed) := by
  refine ⟨?_, fun hle => ?_, fun h0 => ?_⟩
  · by_cases hle : maxConsecutiveStepFailures ≤ job.consecutiveFailures + 1
    · rw [onInvocationFailure_raise job obs hle]
      exact ⟨fun _ => hle, fun _ => rfl⟩
    · rw [onInvocationFailure_count job obs hle]
      simp [hle]
  · rw [onInvocationFailure_raise job obs hle]
    refine ⟨?_, rfl, fun ctx out => ?_⟩
    · simp [terminalizeRow, h_running]
    · simp [admitStep, terminalizeRow, h_running]
  · simp [onInvocationFailure, maxConsecutiveStepFailures, h0]

/-- An admitted job output resets the consecutive step failures. -/
theorem admitted_step_resets_failures {ctx : StepContext} {job job' : Job} {out : StepOutput}
    {p : Pin} (h : admitStep ctx job out = .ok job') (hp : stepPin ctx job out = some p) :
    job'.consecutiveFailures = 0 := by
  have h' := admitStep_pinned h hp
  cases out with
  | effects e =>
    have h2 : admitEffects ctx p { stepRow ctx job (.effects e) with pin := some p, consecutiveFailures := 0 } e =
        .ok job' :=
      h'
    unfold admitEffects at h2
    dsimp only at h2
    split at h2
    · cases h2
    · split_ifs at h2
      cases h2
      rfl
  | result x c g r =>
    simp only [admitJobOutput, endRow] at h'
    split_ifs at h'
    cases h'
    rfl
  | fail x =>
    simp only [admitJobOutput, endRow] at h'
    split_ifs at h'
    cases h'
    rfl
  | suspend k rs x g r =>
    simp only [admitJobOutput, endRow] at h'
    split_ifs at h'
    cases h'
    rfl

end PluginJob.Step
