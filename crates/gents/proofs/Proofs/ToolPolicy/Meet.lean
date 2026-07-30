import Proofs.ToolPolicy.Types

namespace ToolPolicy

def FileCap.rank : FileCap → Nat
  | .off => 0
  | .readOnly => 1
  | .readWrite => 2

def FileCap.meet (a b : FileCap) : FileCap :=
  if a.rank ≤ b.rank then a else b

theorem FileCap.rank_inj {a b : FileCap} (h : a.rank = b.rank) : a = b := by
  cases a <;> cases b <;> simp_all [FileCap.rank]

theorem FileCap.meet_rank_le_left (a b : FileCap) :
    (a.meet b).rank ≤ a.rank := by
  cases a <;> cases b <;> simp [FileCap.meet, FileCap.rank]

theorem FileCap.meet_rank_le_right (a b : FileCap) :
    (a.meet b).rank ≤ b.rank := by
  cases a <;> cases b <;> simp [FileCap.meet, FileCap.rank]

namespace EndpointScope

variable {K V : Type} [DecidableEq K]

def permits : EndpointScope K V → K → Prop
  | .none, _ => False
  | .all, _ => True
  | .only keys _, k => k ∈ keys

instance permitsDecidable (sc : EndpointScope K V) (k : K) :
    Decidable (sc.permits k) := by
  cases sc <;> simp [permits] <;> infer_instance

def lookup : EndpointScope K V → K → Option V
  | .none, _ => Option.none
  | .all, _ => Option.none
  | .only keys val, k => if k ∈ keys then some (val k) else Option.none

def meet (a : EndpointScope K V) (vm : ValueMeet V)
    (b : EndpointScope K V) : EndpointScope K V :=
  match a, b with
  | .none, _ => .none
  | _, .none => .none
  | .all, b => b
  | a, .all => a
  | .only ka va, .only kb vb =>
      .only (ka ∩ kb) (fun k => vm.vmeet (va k) (vb k))

theorem meet_permits_left (vm : ValueMeet V)
    (a b : EndpointScope K V) (k : K) :
    (a.meet vm b).permits k → a.permits k := by
  cases a <;> cases b <;> simp [meet, permits, Finset.mem_inter]
  · intro hka _hkb
    exact hka

theorem meet_permits_right (vm : ValueMeet V)
    (a b : EndpointScope K V) (k : K) :
    (a.meet vm b).permits k → b.permits k := by
  cases a <;> cases b <;> simp [meet, permits, Finset.mem_inter]

theorem meet_lookup_vle_right (vm : ValueMeet V)
    (a b : EndpointScope K V) (k : K) (w w' : V)
    (hm : (a.meet vm b).lookup k = some w) (hb : b.lookup k = some w') :
    vm.vle w w' := by
  cases a with
  | none =>
      simp [meet, lookup] at hm
  | all =>
      cases b with
      | none =>
          simp [meet, lookup] at hb
      | all =>
          simp [meet, lookup] at hb
      | only keys val =>
          simp [meet, lookup] at hm hb
          rw [← hm.2, ← hb.2]
          exact vm.vle_refl (val k)
  | only ka va =>
      cases b with
      | none =>
          simp [meet, lookup] at hb
      | all =>
          simp [meet, lookup] at hb
      | only kb vb =>
          by_cases hk : k ∈ ka ∩ kb
          · have hka : k ∈ ka := (Finset.mem_inter.mp hk).1
            have hkb : k ∈ kb := (Finset.mem_inter.mp hk).2
            simp [meet, lookup, hk, hkb] at hm hb
            subst hm
            subst hb
            exact vm.vmeet_le_right _ _
          · by_cases hkb : k ∈ kb
            · simp [meet, lookup, hk, hkb] at hm
            · simp [meet, lookup, hk, hkb] at hb

theorem meet_lookup_vle_left (vm : ValueMeet V)
    (a b : EndpointScope K V) (k : K) (w w' : V)
    (hm : (a.meet vm b).lookup k = some w) (ha : a.lookup k = some w') :
    vm.vle w w' := by
  cases a with
  | none =>
      simp [meet, lookup] at ha
  | all =>
      cases b with
      | none =>
          simp [meet, lookup] at hm
      | all =>
          simp [meet, lookup] at ha
      | only keys val =>
          simp [meet, lookup] at ha
  | only ka va =>
      cases b with
      | none =>
          simp [meet, lookup] at hm
      | all =>
          simp [meet, lookup] at hm ha
          rw [← hm.2, ← ha.2]
          exact vm.vle_refl (va k)
      | only kb vb =>
          by_cases hk : k ∈ ka ∩ kb
          · have hka : k ∈ ka := (Finset.mem_inter.mp hk).1
            have hkb : k ∈ kb := (Finset.mem_inter.mp hk).2
            simp [meet, lookup, hk, hka] at hm ha
            subst hm
            subst ha
            exact vm.vmeet_le_left _ _
          · by_cases hka : k ∈ ka
            · simp [meet, lookup, hk, hka] at hm
            · simp [meet, lookup, hk, hka] at ha

@[simp] theorem meet_all_right (vm : ValueMeet V) (a : EndpointScope K V) :
    a.meet vm .all = a := by
  cases a <;> rfl

@[simp] theorem meet_all_left (vm : ValueMeet V) (a : EndpointScope K V) :
    (EndpointScope.all : EndpointScope K V).meet vm a = a := by
  cases a <;> rfl

end EndpointScope

def unitVM : ValueMeet Unit :=
  { vmeet := fun _ _ => ()
  , vle := fun _ _ => True
  , vle_refl := by intro _; trivial
  , vmeet_le_left := by intro _ _; trivial
  , vmeet_le_right := by intro _ _; trivial }

def ExecMode.rank : ExecMode → Nat
  | .readOnly => 0
  | .workspaceWrite => 1
  | .unrestricted => 2

def NetMode.rank : NetMode → Nat
  | .disabled => 0
  | .inherit => 1
  | .enabled => 2

structure CmdReq where
  argv : List String
  cmdHead : String
  wantsNetwork : Bool
  wantsWrite : Bool

def prefixOf (needle haystack : List String) : Prop :=
  ∃ suffix, haystack = needle ++ suffix

def BashPolicy.allowedPrefixMatched (p : BashPolicy) (req : CmdReq) : Prop :=
  match p.allowed with
  | .all => True
  | .only keys _ => ∃ pre ∈ keys, prefixOf pre req.argv
  | .none => False

def BashPolicy.allowedGate (p : BashPolicy) (req : CmdReq) : Prop :=
  match p.allowed with
  | .all => True
  | .none => False
  | .only keys _ => ∃ pre ∈ keys, prefixOf pre req.argv

def BashPolicy.modeGate (p : BashPolicy) (req : CmdReq) : Prop :=
  match p.mode with
  | .readOnly =>
      ¬ req.wantsWrite ∧ (p.readOnly.permits req.cmdHead ∨ p.allowedPrefixMatched req)
  | .workspaceWrite => True
  | .unrestricted => True

def BashPolicy.permits (p : BashPolicy) (req : CmdReq) : Prop :=
  p.sandbox = true
  ∧ (∀ f ∈ p.forbidden, ¬ prefixOf f req.argv)
  ∧ p.allowedGate req
  ∧ (req.wantsNetwork → p.network.rank ≥ NetMode.rank .inherit)
  ∧ p.modeGate req

def BashPolicy.meet (a b : BashPolicy) : BashPolicy :=
  { mode := if a.mode.rank ≤ b.mode.rank then a.mode else b.mode
  , network := if a.network.rank ≤ b.network.rank then a.network else b.network
  , forbidden := a.forbidden ∪ b.forbidden
  , allowed := a.allowed.meet unitVM b.allowed
  , readOnly := a.readOnly.meet unitVM b.readOnly
  , sandbox := a.sandbox && b.sandbox }

@[simp] theorem bool_and_left {a b : Bool} (h : (a && b) = true) : a = true :=
  by cases a <;> cases b <;> simp at h ⊢

@[simp] theorem bool_and_right {a b : Bool} (h : (a && b) = true) : b = true :=
  by cases a <;> cases b <;> simp at h ⊢

end ToolPolicy
