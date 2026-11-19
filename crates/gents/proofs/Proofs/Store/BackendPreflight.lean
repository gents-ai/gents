import Lean
import Proofs.Store.StoreKeyCustody

namespace BackendPreflight

inductive CopyResult where
  | compatible | incompatible | unavailable
  deriving BEq, DecidableEq, Repr, Lean.ToJson

/-- A failed inspection of a copy is not permission to open the original.
Only an observed compatible copy permits the protected store to open. -/
def decision : CopyResult → String
  | .compatible => "open"
  | .incompatible => "incompatible"
  | .unavailable => "unavailable"

theorem unavailable_never_opens : decision .unavailable ≠ "open" := by decide
theorem incompatible_never_opens : decision .incompatible ≠ "open" := by decide

/-- A store's lineage marker is a file in its own data directory, named this
prefix and then the lineage version in decimal. The version moves with the
node grants and the Household policy a store is created under as well as with
its format (gents#3747, gents#3794): those policy changes keep the policy ID,
so only the marker tells one lineage's store from another's before DefraDB
refuses to register the policy. It is read from the directory's names alone,
so a refusal leaves the store unopened. -/
def markerPrefix : String := ".gents-private-backend-v"

/-- What a directory entry's name says as a lineage marker. -/
inductive Marker where
  | version (n : Nat)
  /-- The prefix followed by anything but a version's canonical decimal. No
  release writes one, so the version it stands for is unknown. -/
  | unparseable
  deriving DecidableEq, Repr

def marker? (name : String) : Option Marker :=
  if name.startsWith markerPrefix then
    let suffix := name.drop markerPrefix.length
    some <| match suffix.toNat? with
      | some n => if toString n == suffix then .version n else .unparseable
      | none => .unparseable
  else none

/-- Whether a marker makes its store newer than lineage `current`: a later
version does, and so does one that cannot be read, which fails closed as not
older. -/
def Marker.newerThan (current : Nat) : Marker → Bool
  | .version n => current < n
  | .unparseable => true

inductive Lineage where
  | current | older | newer | unmarked
  deriving DecidableEq, Repr

/-- The lineage of a store whose directory holds `markers`, for a build of
lineage `current`. This build's own marker wins over any other, so a store
this build made opens. Any newer marker then makes the store newer, because
the release that wrote it may still want it. Only markers that are all of
earlier versions make a store older. -/
def classify (current : Nat) (markers : List Marker) : Lineage :=
  if Marker.version current ∈ markers then .current
  else if markers.any (Marker.newerThan current) then .newer
  else if markers.isEmpty then .unmarked
  else .older

def lineage (current : Nat) (names : List String) : Lineage :=
  classify current (names.filterMap marker?)

inductive Refusal where
  | older | newer
  deriving DecidableEq, Repr, Lean.ToJson

/-- The refusal of a store of a lineage. `beforeLock` is a read before the
store lock is taken, because taking it rewrites the lock file: there a store
with no marker at all may be one another process is still initializing, so the
lock decides it. Once the lock is held, and in a read-only look that reports
what opening will decide, a store with no marker was made before backend
protection or never finished initializing (its marker is written last), and is
older. -/
def refusal (beforeLock : Bool) : Lineage → Option Refusal
  | .current => none
  | .older => some .older
  | .newer => some .newer
  | .unmarked => if beforeLock then none else some .older

/-- Whether starting fresh is offered for a refusal, as
`StoreKeyCustody.Step.older` records it for a refused store: a newer lineage is
that owner's foreign version. -/
def Refusal.offersFreshStart : Refusal → Option Bool
  | .older => some true
  | .newer => StoreKeyCustody.Outcome.foreignVersion.older

private theorem classify_older {current : Nat} {markers : List Marker}
    (h : classify current markers = .older) :
    ∀ m ∈ markers, ∃ n, m = .version n ∧ n < current := by
  intro m hm
  unfold classify at h
  by_cases hc : Marker.version current ∈ markers
  · simp [hc] at h
  · by_cases ha : markers.any (Marker.newerThan current) = true
    · simp [hc, ha] at h
    · rw [List.any_eq_true] at ha
      cases m with
      | unparseable => exact absurd ⟨_, hm, rfl⟩ ha
      | version n =>
        refine ⟨n, rfl, ?_⟩
        have hn : n ≠ current := fun e => hc (e ▸ hm)
        have hl : ¬ current < n := fun lt => ha ⟨_, hm, by simpa [Marker.newerThan] using lt⟩
        omega

private theorem classify_unmarked {current : Nat} {markers : List Marker}
    (h : classify current markers = .unmarked) : markers = [] := by
  unfold classify at h
  by_cases hc : Marker.version current ∈ markers
  · simp [hc] at h
  · by_cases ha : markers.any (Marker.newerThan current) = true
    · simp [hc, ha] at h
    · cases markers with
      | nil => rfl
      | cons => simp [hc, ha] at h

/-- Starting fresh is offered, at any read, only for a store each of whose
markers names a version earlier than this build's. -/
theorem deletion_is_offered_only_for_earlier_markers (beforeLock : Bool) (current : Nat)
    (markers : List Marker) (r : Refusal)
    (refused : refusal beforeLock (classify current markers) = some r)
    (offered : r.offersFreshStart = some true) :
    ∀ m ∈ markers, ∃ n, m = .version n ∧ n < current := by
  cases r with
  | newer => simp [Refusal.offersFreshStart, StoreKeyCustody.Outcome.older] at offered
  | older =>
    cases hl : classify current markers with
    | older => exact classify_older hl
    | unmarked => simp [classify_unmarked hl]
    | current => simp [hl, refusal] at refused
    | newer => simp [hl, refusal] at refused

/-- A store holding a later version's marker and not this build's is refused
as newer at every read, and never offered for deletion: the verdict
`StoreKeyCustody` gives a foreign version. -/
theorem a_newer_store_is_never_offered_for_deletion (beforeLock keyPresent : Bool)
    (current n : Nat) (markers : List Marker)
    (later : Marker.version n ∈ markers) (h : current < n)
    (notCurrent : Marker.version current ∉ markers) :
    refusal beforeLock (classify current markers) = some .newer
      ∧ Refusal.newer.offersFreshStart = (StoreKeyCustody.load false keyPresent).older
      ∧ Refusal.newer.offersFreshStart = some false := by
  have any : markers.any (Marker.newerThan current) = true :=
    List.any_eq_true.mpr ⟨_, later, by simpa [Marker.newerThan] using h⟩
  refine ⟨by simp [classify, notCurrent, any, refusal], ?_, rfl⟩
  rw [StoreKeyCustody.a_foreign_version_is_never_offered_for_deletion]
  rfl

theorem an_unreadable_marker_is_never_older (current : Nat) (markers : List Marker)
    (h : Marker.unparseable ∈ markers) : classify current markers ≠ .older :=
  fun older => by simpa using classify_older older _ h

/-- A store whose markers are all of earlier versions is refused as older at
every read, and starting fresh is offered for it. -/
theorem an_older_store_is_offered_a_fresh_start (beforeLock : Bool) (current : Nat)
    (markers : List Marker) (marked : markers ≠ [])
    (earlier : ∀ m ∈ markers, ∃ n, m = .version n ∧ n < current) :
    refusal beforeLock (classify current markers) = some .older
      ∧ Refusal.older.offersFreshStart = some true := by
  have notCurrent : Marker.version current ∉ markers := fun hm => by
    obtain ⟨n, e, lt⟩ := earlier _ hm
    cases e
    omega
  have noneLater : markers.any (Marker.newerThan current) = false := by
    rw [Bool.eq_false_iff, ne_eq, List.any_eq_true]
    rintro ⟨m, hm, later⟩
    obtain ⟨n, rfl, lt⟩ := earlier m hm
    simp [Marker.newerThan] at later
    omega
  have nonempty : markers.isEmpty = false := by
    cases markers with
    | nil => exact absurd rfl marked
    | cons => rfl
  simp [classify, notCurrent, noneLater, nonempty, refusal, Refusal.offersFreshStart]

/-- This build's marker opens the store at every read, whatever else the
directory holds. -/
theorem the_current_marker_opens (beforeLock : Bool) (current : Nat) (markers : List Marker)
    (h : Marker.version current ∈ markers) :
    refusal beforeLock (classify current markers) = none := by
  simp [classify, h, refusal]

/-- A store with no marker is left to the lock before it is taken, and refused
as older once it is held. -/
theorem an_unmarked_store_is_left_to_the_lock (current : Nat) :
    refusal true (classify current []) = none
      ∧ refusal false (classify current []) = some .older := by
  simp [classify, refusal]

end BackendPreflight
