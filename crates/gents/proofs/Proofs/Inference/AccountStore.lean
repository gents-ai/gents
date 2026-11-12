import Proofs.Shared.Basic

/-! The OAuth account rows of one node: what a sign-in, a label change, a
disable and a removal do to them, and which rows own a backend.

A row's identity is the set of provider account keys its tokens show. How a
provider's tokens show a key (JWT claims, the organization and account of a
Claude login) is that provider's wire format; the model takes the shown keys as
given. -/

namespace AccountStore

inductive Provider where
  | chatGpt
  | claude
  | grok
  | cloud
  deriving DecidableEq, Repr

namespace Provider

/-- A sign-in is stored only for a product a backend preset reads. A row of any
other provider (a cloud workspace token) is a credential and never an account. -/
def account : Provider → Bool
  | .cloud => false
  | _ => true

def name : Provider → String
  | .chatGpt => "ChatGPT"
  | .claude => "Claude"
  | .grok => "Grok"
  | .cloud => "gents-cloud"

end Provider

structure Row where
  id : Nat
  provider : Provider
  tokenKeys : List String
  key : Option String
  accountId : Option String
  label : Option String
  original : Bool
  enabled : Bool
  token : String
  deriving DecidableEq, Repr

structure Backend where
  row : Nat
  name : String
  deriving DecidableEq, Repr

/-- `rows` is in the resolver's order: connection time, then credential id. The
original slot's id is a prefix of every added slot's, so it sorts first among
rows connected in the same second. -/
structure State where
  rows : List Row
  backends : List Backend
  next : Nat
  deriving DecidableEq, Repr

/-- Production trims Unicode whitespace; `String.trim` trims the ASCII subset. -/
def present (value : Option String) : Option String :=
  (value.map String.trim).filter fun text => text != ""

/-- A stored key is trusted only where nothing else shows the identity. ChatGPT
and Grok tokens show their own keys, so a key stored beside them is ignored; a
ChatGPT token without an account id shows none. -/
def identityKeys (provider : Provider) (tokenKeys : List String)
    (key accountId : Option String) : List String :=
  let shown := match provider with
    | .chatGpt => if accountId.any (fun id => id != "") then tokenKeys else []
    | .grok => tokenKeys
    | _ => key.toList
  shown.filterMap fun shownKey => present (some shownKey)

def Row.keys (row : Row) : List String :=
  identityKeys row.provider row.tokenKeys row.key row.accountId

def Row.effectiveLabel (row : Row) : String :=
  row.label.getD row.provider.name

/-- How strongly a stored row is the signing-in account: `0` shares a key, `1`
has no key and the same account id, `2` has no identity at all. A row that
shows a key or an account id the sign-in does not share is another account. -/
def matchTier (row : Row) (keys : List String) (accountId : Option String) : Option Nat :=
  if !row.keys.isEmpty && !keys.any row.keys.contains then none
  else if (present row.accountId).isSome && present accountId != present row.accountId then none
  else some (if !row.keys.isEmpty then 0 else if (present row.accountId).isSome then 1 else 2)

def findMatch (rows : List Row) (keys : List String) (accountId : Option String) :
    Option (Nat × Row) :=
  [0, 1, 2].findSome? fun tier =>
    (rows.find? fun row => matchTier row keys accountId == some tier).map fun row => (tier, row)

def isControl (c : Char) : Bool :=
  c.toNat < 0x20 || (0x7f ≤ c.toNat && c.toNat ≤ 0x9f)

def maxLabelChars : Nat := 64

def validLabel (raw : String) : Option String :=
  let label := raw.trim
  if label == "" || label.length > maxLabelChars || label.any isControl then none else some label

def labelTaken (rows : List Row) (label : String) : Bool :=
  rows.any fun row => row.effectiveLabel == label

def nextFreeLabel (rows : List Row) (provider : Provider) : String :=
  let candidates := provider.name ::
    (List.range rows.length).map fun n => s!"{provider.name} {n + 2}"
  (candidates.find? fun label => !labelTaken rows label).getD provider.name

/-- An added account owns one backend named after it. A backend follows its
account's label only while it still carries the old one, so a name someone
chose survives a relabel. The original slot is served by the provider's preset
backend and owns none. -/
def syncBackend (backends : List Backend) (row : Row) (oldLabel : Option String) :
    List Backend :=
  if row.original then backends
  else if !backends.any (fun backend => backend.row == row.id) then
    backends ++ [{ row := row.id, name := row.effectiveLabel }]
  else match oldLabel with
    | none => backends
    | some old =>
      if old == row.effectiveLabel then backends
      else backends.map fun backend =>
        if backend.row == row.id && backend.name == old then
          { backend with name := row.effectiveLabel }
        else backend

structure SignIn where
  provider : Provider
  tokenKeys : List String
  key : Option String
  accountId : Option String
  label : Option String
  token : String
  deriving DecidableEq, Repr

inductive Outcome where
  | added
  | refreshed
  | changed
  | rejected
  deriving DecidableEq, Repr

structure StepResult where
  outcome : Outcome
  row : Option Nat
  identityMatched : Bool
  deriving DecidableEq, Repr

def rejected : StepResult :=
  { outcome := .rejected, row := none, identityMatched := false }

def changed (row : Nat) : StepResult :=
  { outcome := .changed, row := some row, identityMatched := false }

/-- A refresh replaces what the tokens carry and keeps the slot. The stored key
stays when the sign-in still shows it; otherwise the sign-in's own key replaces
it. A disabled account that signs in again is enabled. -/
def refreshRow (row : Row) (signIn : SignIn) (keys : List String) (label : Option String) :
    Row :=
  { row with
    tokenKeys := signIn.tokenKeys
    key := if (present row.key).any keys.contains then row.key else signIn.key
    accountId := signIn.accountId
    label := if label.isSome then label else row.label
    enabled := true
    token := signIn.token }

/-- The first account of a provider takes the original slot and shows the
product's name; every later one is added beside it under the next free name. -/
def addRow (id : Nat) (rows : List Row) (signIn : SignIn) (label : Option String) : Row :=
  { id
    provider := signIn.provider
    tokenKeys := signIn.tokenKeys
    key := signIn.key
    accountId := signIn.accountId
    label :=
      if label.isSome then label
      else if rows.isEmpty then none
      else some (nextFreeLabel rows signIn.provider)
    original := rows.isEmpty
    enabled := true
    token := signIn.token }

def replaceRow (rows : List Row) (fresh : Row) : List Row :=
  rows.map fun row => if row.id == fresh.id then fresh else row

inductive Decision where
  | reject
  | refresh (tier : Nat) (row : Row) (label : Option String)
  | add (label : Option String)
  deriving DecidableEq, Repr

def reconcile (state : State) (signIn : SignIn) : Decision :=
  let rows := state.rows.filter fun row => row.provider == signIn.provider
  let keys := identityKeys signIn.provider signIn.tokenKeys signIn.key signIn.accountId
  if !signIn.provider.account then .reject
  else match signIn.label.map validLabel with
    | some none => .reject
    | label =>
      match findMatch rows keys signIn.accountId with
      | some (tier, row) =>
        if label.join.any (labelTaken (rows.filter fun other => other.id != row.id)) then .reject
        else .refresh tier row label.join
      | none => if label.join.any (labelTaken rows) then .reject else .add label.join

def signIn (state : State) (signIn : SignIn) : State × StepResult :=
  let rows := state.rows.filter fun row => row.provider == signIn.provider
  let keys := identityKeys signIn.provider signIn.tokenKeys signIn.key signIn.accountId
  match reconcile state signIn with
  | .reject => (state, rejected)
  | .refresh tier row label =>
    let fresh := refreshRow row signIn keys label
    ( { state with
        rows := replaceRow state.rows fresh
        backends := syncBackend state.backends fresh (some row.effectiveLabel) }
    , { outcome := .refreshed, row := some row.id, identityMatched := tier < 2 } )
  | .add label =>
    let fresh := addRow state.next rows signIn label
    ( { rows := state.rows ++ [fresh]
        backends := syncBackend state.backends fresh none
        next := state.next + 1 }
    , { outcome := .added, row := some state.next, identityMatched := false } )

/-- The runtime's own write of a credential it holds addresses the provider's
original slot by its id and stores the credential as given: tokens, key and
account id are replaced, a key the credential lacks is cleared, and the label
and slot stay. It does not reconcile identities; that is the sign-in. -/
def upsertRow (row : Row) (signIn : SignIn) : Row :=
  { row with
    tokenKeys := signIn.tokenKeys
    key := signIn.key
    accountId := signIn.accountId
    enabled := true
    token := signIn.token }

def upsert (state : State) (signIn : SignIn) : State × StepResult :=
  match state.rows.find? fun row => row.provider == signIn.provider && row.original with
  | some row =>
    ( { state with rows := replaceRow state.rows (upsertRow row signIn) }
    , { outcome := .refreshed, row := some row.id, identityMatched := false } )
  | none =>
    ( { state with
        rows := state.rows ++ [{ addRow state.next [] signIn none with label := none }]
        next := state.next + 1 }
    , { outcome := .added, row := some state.next, identityMatched := false } )

def account (state : State) (id : Nat) : Option Row :=
  state.rows.find? fun row => row.id == id && row.provider.account

inductive Action where
  | signIn (signIn : SignIn)
  | upsert (signIn : SignIn)
  | setLabel (row : Nat) (label : String)
  | setEnabled (row : Nat) (enabled : Bool)
  | remove (row : Nat)
  | loseBackend (row : Nat)
  | renameBackend (row : Nat) (name : String)
  deriving DecidableEq, Repr

def setLabel (state : State) (id : Nat) (raw : String) : State × StepResult :=
  match validLabel raw, account state id with
  | some label, some row =>
    let others := state.rows.filter fun other =>
      other.provider == row.provider && other.id != row.id
    if labelTaken others label then (state, rejected)
    else
      let fresh := { row with label := some label }
      ( { state with
          rows := replaceRow state.rows fresh
          backends := syncBackend state.backends fresh (some row.effectiveLabel) }
      , changed id )
  | _, _ => (state, rejected)

/-- Removal deletes the account's row and nothing else; a backend that named it
stays until someone removes it. -/
def step (state : State) : Action → State × StepResult
  | .signIn request => signIn state request
  | .upsert request => upsert state request
  | .setLabel id label => setLabel state id label
  | .setEnabled id enabled =>
    match account state id with
    | some row => ({ state with rows := replaceRow state.rows { row with enabled } }, changed id)
    | none => (state, rejected)
  | .remove id =>
    match account state id with
    | some _ => ({ state with rows := state.rows.filter fun row => row.id != id }, changed id)
    | none => (state, rejected)
  | .loseBackend id =>
    ({ state with backends := state.backends.filter fun backend => backend.row != id }, changed id)
  | .renameBackend id name =>
    ( { state with
        backends := state.backends.map fun backend =>
          if backend.row == id then { backend with name } else backend }
    , changed id )

def run (state : State) : List Action → State × List StepResult
  | [] => (state, [])
  | action :: rest =>
    let (next, result) := step state action
    let (final, results) := run next rest
    (final, result :: results)

theorem keyless_sign_in_is_not_a_keyed_row (row : Row) (accountId : Option String)
    (keyed : row.keys ≠ []) : matchTier row [] accountId = none := by
  cases shown : row.keys with
  | nil => exact absurd shown keyed
  | cons _ _ => simp [matchTier, shown]

theorem another_account_id_never_matches (row : Row) (keys : List String)
    (accountId : Option String) (stored : (present row.accountId).isSome)
    (differs : present accountId ≠ present row.accountId) :
    matchTier row keys accountId = none := by
  unfold matchTier
  split
  · rfl
  · simp [stored, differs]

theorem a_key_match_shares_a_key (row : Row) (keys : List String) (accountId : Option String)
    (matched : matchTier row keys accountId = some 0) :
    ∃ key, key ∈ keys ∧ key ∈ row.keys := by
  unfold matchTier at matched
  cases shown : row.keys with
  | nil =>
    simp [shown] at matched
    split at matched <;> simp_all
  | cons head tail =>
    by_cases shared : keys.any (head :: tail).contains
    · simpa using shared
    · simp [shown, shared] at matched

theorem a_refresh_keeps_the_slot (row : Row) (request : SignIn) (keys : List String)
    (label : Option String) :
    (refreshRow row request keys label).id = row.id
      ∧ (refreshRow row request keys label).provider = row.provider
      ∧ (refreshRow row request keys label).original = row.original
      ∧ (refreshRow row request keys label).enabled = true :=
  ⟨rfl, rfl, rfl, rfl⟩

theorem a_refresh_without_a_label_keeps_the_label (row : Row) (request : SignIn)
    (keys : List String) : (refreshRow row request keys none).label = row.label := rfl

theorem a_rejected_sign_in_changes_nothing (state : State) (request : SignIn)
    (refused : (signIn state request).2.outcome = .rejected) :
    (signIn state request).1 = state := by
  unfold signIn at refused ⊢
  cases decision : reconcile state request <;> simp_all

theorem a_sign_in_adds_at_most_one_row (state : State) (request : SignIn) :
    (signIn state request).1.rows.length ≤ state.rows.length + 1 := by
  unfold signIn
  cases decision : reconcile state request <;> simp [replaceRow]

theorem the_first_account_takes_the_original_slot (state : State) (request : SignIn)
    (label : Option String)
    (first : (state.rows.filter fun row => row.provider == request.provider) = [])
    (added : reconcile state request = .add label) :
    ∃ fresh, (signIn state request).1.rows = state.rows ++ [fresh]
      ∧ fresh.original = true ∧ fresh.id = state.next := by
  unfold signIn
  simp [added, first, addRow]

theorem a_later_account_is_added_beside (state : State) (request : SignIn)
    (label : Option String)
    (later : (state.rows.filter fun row => row.provider == request.provider) ≠ [])
    (added : reconcile state request = .add label) :
    ∃ fresh, (signIn state request).1.rows = state.rows ++ [fresh]
      ∧ fresh.original = false ∧ fresh.label.isSome := by
  unfold signIn
  cases rows : (state.rows.filter fun row => row.provider == request.provider) with
  | nil => exact absurd rows later
  | cons _ _ => cases label <;> simp [added, rows, addRow]

theorem a_sign_in_leaves_other_rows (state : State) (request : SignIn) (other : Row)
    (stored : other ∈ state.rows)
    (untouched : ∀ tier row label, reconcile state request = .refresh tier row label →
      other.id ≠ row.id) :
    other ∈ (signIn state request).1.rows := by
  unfold signIn
  cases decision : reconcile state request with
  | reject => simpa using stored
  | add label => simp [stored]
  | refresh tier row label =>
    have differs := untouched tier row label decision
    simp only [replaceRow, List.mem_map]
    exact ⟨other, stored, by simp [refreshRow, differs]⟩

theorem an_upsert_keeps_the_label_and_slot (row : Row) (request : SignIn) :
    (upsertRow row request).label = row.label
      ∧ (upsertRow row request).original = row.original
      ∧ (upsertRow row request).id = row.id
      ∧ (upsertRow row request).key = request.key :=
  ⟨rfl, rfl, rfl, rfl⟩

theorem an_upsert_never_touches_the_backends (state : State) (request : SignIn) :
    (upsert state request).1.backends = state.backends := by
  unfold upsert
  split <;> rfl

theorem a_valid_label_is_trimmed (raw label : String) (valid : validLabel raw = some label) :
    label = raw.trim := by
  simp only [validLabel] at valid
  split at valid <;> simp_all

end AccountStore
