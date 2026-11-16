import Lean

/-! How a node's inference backend resolves an OAuth account. A pool holds
every enabled row of the requested provider the executing node owns; a pick
chooses one of them. Both consider the same rows, and the resolver orders them
earliest connection first, breaking equal connection times on the credential
id. A named reference matches its account reference exactly and never falls
back; an unset reference is itself a name, matching only rows with no
reference. -/

namespace AccountResolver

structure Account where
  owner : String
  provider : String
  credentialId : String
  accountRef : Option String
  connectedAt : Option Nat
  enabled : Bool
  deriving DecidableEq, Repr

/-- The resolver's order: an absent connection time is earliest, an earlier
time precedes a later one, and equal times (or two absent ones) compare the
credential id. -/
def before (a b : Account) : Bool :=
  match a.connectedAt, b.connectedAt with
  | none, none => compare a.credentialId b.credentialId != .gt
  | none, some _ => true
  | some _, none => false
  | some x, some y =>
      if x = y then compare a.credentialId b.credentialId != .gt else decide (x < y)

def owned (owner provider : String) (r : Account) : Bool :=
  r.owner == owner && r.provider == provider && r.enabled

def matchesRef (pickDefault : Bool) (accountRef : Option String) (r : Account) : Bool :=
  pickDefault || (r.accountRef == accountRef)

def pool (owner provider : String) (rows : List Account) : List Account :=
  (rows.filter fun r => owned owner provider r).mergeSort before

def candidates (owner provider : String) (pickDefault : Bool) (accountRef : Option String)
    (rows : List Account) : List Account :=
  (rows.filter fun r => owned owner provider r && matchesRef pickDefault accountRef r).mergeSort
    before

def selected (owner provider : String) (pickDefault : Bool) (accountRef : Option String)
    (rows : List Account) : Option Account :=
  (candidates owner provider pickDefault accountRef rows).find? fun _ => true

theorem a_missing_connection_time_sorts_before_a_timed_one (a b : Account) (t : Nat)
    (ha : a.connectedAt = none) (hb : b.connectedAt = some t) : before a b = true := by
  simp [before, ha, hb]

theorem an_earlier_connection_sorts_before_a_later_one (a b : Account) (x y : Nat)
    (ha : a.connectedAt = some x) (hb : b.connectedAt = some y) (h : x < y) :
    before a b = true := by
  have hne : x ≠ y := Nat.ne_of_lt h
  simp [before, ha, hb, hne, h]

theorem equal_connection_times_break_on_the_credential_id (a b : Account) (t : Option Nat)
    (ha : a.connectedAt = t) (hb : b.connectedAt = t) :
    before a b = (compare a.credentialId b.credentialId != .gt) := by
  cases t with
  | none => simp [before, ha, hb]
  | some x => simp [before, ha, hb]

theorem the_pool_holds_every_enabled_owned_row (owner provider : String) (rows : List Account)
    (r : Account) (hmem : r ∈ rows) (howner : r.owner = owner) (hprovider : r.provider = provider)
    (henabled : r.enabled = true) : r ∈ pool owner provider rows := by
  rw [pool, List.mem_mergeSort, List.mem_filter]
  refine ⟨hmem, ?_⟩
  have howner' : (r.owner == owner) = true := by simp [howner]
  have hprovider' : (r.provider == provider) = true := by simp [hprovider]
  simp [owned, howner', hprovider', henabled]

theorem owned_owner (owner provider : String) (r : Account) (h : owned owner provider r = true) :
    r.owner = owner := by
  simp only [owned, Bool.and_eq_true, beq_iff_eq] at h
  exact h.1.1

theorem owned_provider (owner provider : String) (r : Account) (h : owned owner provider r = true) :
    r.provider = provider := by
  simp only [owned, Bool.and_eq_true, beq_iff_eq] at h
  exact h.1.2

theorem owned_enabled (owner provider : String) (r : Account) (h : owned owner provider r = true) :
    r.enabled = true := by
  simp only [owned, Bool.and_eq_true, beq_iff_eq] at h
  exact h.2

theorem the_pool_holds_only_enabled_owned_rows (owner provider : String) (rows : List Account)
    (r : Account) (h : r ∈ pool owner provider rows) :
    r ∈ rows ∧ r.owner = owner ∧ r.provider = provider ∧ r.enabled = true := by
  simp only [pool, List.mem_mergeSort, List.mem_filter] at h
  exact ⟨h.1, owned_owner owner provider r h.2, owned_provider owner provider r h.2,
    owned_enabled owner provider r h.2⟩

theorem a_chosen_account_is_enabled (owner provider : String) (pickDefault : Bool)
    (accountRef : Option String) (rows : List Account) (r : Account)
    (h : selected owner provider pickDefault accountRef rows = some r) : r.enabled = true := by
  have hmem := List.mem_of_find?_eq_some h
  simp only [candidates, List.mem_mergeSort, List.mem_filter, Bool.and_eq_true] at hmem
  exact owned_enabled owner provider r hmem.2.1

theorem a_chosen_account_belongs_to_the_node_and_provider (owner provider : String)
    (pickDefault : Bool) (accountRef : Option String) (rows : List Account) (r : Account)
    (h : selected owner provider pickDefault accountRef rows = some r) :
    r.owner = owner ∧ r.provider = provider := by
  have hmem := List.mem_of_find?_eq_some h
  simp only [candidates, List.mem_mergeSort, List.mem_filter, Bool.and_eq_true] at hmem
  exact ⟨owned_owner owner provider r hmem.2.1, owned_provider owner provider r hmem.2.1⟩

theorem a_reference_names_exactly_its_account (owner provider : String)
    (accountRef : Option String) (rows : List Account) (r : Account)
    (h : selected owner provider false accountRef rows = some r) :
    r.accountRef = accountRef := by
  have hmem := List.mem_of_find?_eq_some h
  simp only [candidates, List.mem_mergeSort, List.mem_filter, Bool.and_eq_true] at hmem
  have hmatch : matchesRef false accountRef r = true := hmem.2.2
  simp only [matchesRef, Bool.false_or, beq_iff_eq] at hmatch
  exact hmatch

theorem a_pool_without_an_enabled_owned_row_is_empty (owner provider : String)
    (rows : List Account) (h : ∀ r ∈ rows, owned owner provider r = false) :
    pool owner provider rows = [] := by
  have hnil : rows.filter (fun r => owned owner provider r) = [] := by
    rw [List.filter_eq_nil_iff]
    intro r hr
    simp [h r hr]
  simp [pool, hnil]

theorem nothing_is_selected_without_an_enabled_owned_row (owner provider : String)
    (pickDefault : Bool) (accountRef : Option String) (rows : List Account)
    (h : ∀ r ∈ rows, owned owner provider r = false) :
    selected owner provider pickDefault accountRef rows = none := by
  have hnil :
      rows.filter (fun r => owned owner provider r && matchesRef pickDefault accountRef r) = [] := by
    rw [List.filter_eq_nil_iff]
    intro r hr
    simp [h r hr]
  simp [selected, candidates, hnil]

theorem the_pool_is_in_resolver_order :
    (pool "did:key:zResolver" "xai-oauth"
      [ { owner := "did:key:zResolver", provider := "xai-oauth", credentialId := "late", accountRef := some "late", connectedAt := some 2000, enabled := true }
      , { owner := "did:key:zResolver", provider := "xai-oauth", credentialId := "off", accountRef := some "off", connectedAt := some 500, enabled := false }
      , { owner := "did:key:zOther", provider := "xai-oauth", credentialId := "foreign", accountRef := some "f", connectedAt := some 100, enabled := true }
      , { owner := "did:key:zResolver", provider := "chatgpt-codex", credentialId := "other-provider", accountRef := none, connectedAt := some 100, enabled := true }
      , { owner := "did:key:zResolver", provider := "xai-oauth", credentialId := "original", accountRef := none, connectedAt := none, enabled := true }
      , { owner := "did:key:zResolver", provider := "xai-oauth", credentialId := "early", accountRef := some "early", connectedAt := some 1000, enabled := true }
      ]).map (·.credentialId) = ["original", "early", "late"] := by
  native_decide

theorem the_earliest_connected_enabled_account_is_chosen :
    selected "did:key:zResolver" "xai-oauth" true none
      [ { owner := "did:key:zResolver", provider := "xai-oauth", credentialId := "late", accountRef := some "late", connectedAt := some 2000, enabled := true }
      , { owner := "did:key:zResolver", provider := "xai-oauth", credentialId := "early", accountRef := some "early", connectedAt := some 1000, enabled := true }
      ] =
    some
      { owner := "did:key:zResolver", provider := "xai-oauth", credentialId := "early", accountRef := some "early", connectedAt := some 1000, enabled := true } := by
  native_decide

end AccountResolver
