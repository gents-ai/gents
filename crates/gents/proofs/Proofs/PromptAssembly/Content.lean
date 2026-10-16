import Proofs.PromptAssembly.State

/-!
# Assistant content ordering

`Proofs.PromptAssembly.Executable` models a message as a single `MessageKind`
row; the Rust `normalize_assistant_content_order` composition reorders assistant
content for grouped-order providers, while Claude's native replay preserves the
original block order because moving a signed thinking block changes the exact
continuation witness. Both order modes leave announced tool calls unchanged, so
the row abstraction is invariant under them (`callsOf_normalizeFor`).
-/

namespace PromptAssembly.Content

inductive Item where
  | text (index : Nat)
  | other (index : Nat)
  | call (callId : ToolExecution.ToolCallId)
  deriving DecidableEq, Repr

namespace Item

def isText : Item → Bool
  | .text _ => true
  | _ => false

def isOther : Item → Bool
  | .other _ => true
  | _ => false

def isCall : Item → Bool
  | .call _ => true
  | _ => false

def callId? : Item → Option ToolExecution.ToolCallId
  | .call callId => some callId
  | _ => none

@[simp] theorem isText_text (index : Nat) : (Item.text index).isText := rfl
@[simp] theorem isText_other (index : Nat) : (Item.other index).isText = false := rfl
@[simp] theorem isText_call (callId : ToolExecution.ToolCallId) :
    (Item.call callId).isText = false := rfl

@[simp] theorem isOther_text (index : Nat) : (Item.text index).isOther = false := rfl
@[simp] theorem isOther_other (index : Nat) : (Item.other index).isOther := rfl
@[simp] theorem isOther_call (callId : ToolExecution.ToolCallId) :
    (Item.call callId).isOther = false := rfl

@[simp] theorem isCall_text (index : Nat) : (Item.text index).isCall = false := rfl
@[simp] theorem isCall_other (index : Nat) : (Item.other index).isCall = false := rfl
@[simp] theorem isCall_call (callId : ToolExecution.ToolCallId) :
    (Item.call callId).isCall := rfl

end Item

/-- Mirrors Rust `normalize_assistant_content_order`. -/
def normalize (items : List Item) : List Item :=
  items.filter Item.isText ++ items.filter Item.isOther ++ items.filter Item.isCall

/-- The selected provider-input order, not an inference from message contents.
Claude uses `nativePreserved`; grouped-order providers use `grouped`. -/
inductive OrderMode where
  | grouped
  | nativePreserved
  deriving DecidableEq, Repr

def normalizeFor : OrderMode → List Item → List Item
  | .grouped, items => normalize items
  | .nativePreserved, items => items

@[simp] theorem normalizeFor_grouped (items : List Item) :
    normalizeFor .grouped items = normalize items := rfl

/-- Claude's native content order, including each reasoning block's position,
is unchanged by the ordering stage. -/
@[simp] theorem normalizeFor_nativePreserved (items : List Item) :
    normalizeFor .nativePreserved items = items := rfl

/-- The tool calls a content list announces. -/
def callsOf (items : List Item) : Finset ToolExecution.ToolCallId :=
  items.foldr
    (fun item acc =>
      match item with
      | .call callId => insert callId acc
      | _ => acc)
    ∅

@[simp] theorem callsOf_nil : callsOf [] = ∅ := rfl

theorem callsOf_cons (item : Item) (rest : List Item) :
    callsOf (item :: rest) =
      (match item with
        | .call callId => insert callId (callsOf rest)
        | _ => callsOf rest) := rfl

@[simp] theorem callsOf_cons_text (index : Nat) (rest : List Item) :
    callsOf (Item.text index :: rest) = callsOf rest := rfl

@[simp] theorem callsOf_cons_other (index : Nat) (rest : List Item) :
    callsOf (Item.other index :: rest) = callsOf rest := rfl

@[simp] theorem callsOf_cons_call (callId : ToolExecution.ToolCallId)
    (rest : List Item) :
    callsOf (Item.call callId :: rest) = insert callId (callsOf rest) := rfl

theorem callsOf_append (a b : List Item) :
    callsOf (a ++ b) = callsOf a ∪ callsOf b := by
  induction a with
  | nil => simp
  | cons item rest ih =>
    cases item with
    | text index => simpa using ih
    | other index => simpa using ih
    | call callId =>
      rw [List.cons_append, callsOf_cons_call, callsOf_cons_call, ih,
        Finset.insert_union]

theorem mem_callsOf_of_mem {items : List Item} {callId : ToolExecution.ToolCallId}
    (h : Item.call callId ∈ items) : callId ∈ callsOf items := by
  induction items with
  | nil => simp at h
  | cons head tail ih =>
    rcases List.mem_cons.mp h with rfl | htail
    · simp
    · have hmem := ih htail
      cases head with
      | text index => simpa using hmem
      | other index => simpa using hmem
      | call other =>
        simp only [callsOf_cons_call]
        exact Finset.mem_insert_of_mem hmem

theorem callsOf_eq_empty_of_no_calls (items : List Item)
    (h : ∀ item ∈ items, item.isCall = false) : callsOf items = ∅ := by
  induction items with
  | nil => rfl
  | cons item rest ih =>
    have hrest : ∀ i ∈ rest, i.isCall = false := fun i hi =>
      h i (List.mem_cons_of_mem _ hi)
    cases item with
    | text index => simpa using ih hrest
    | other index => simpa using ih hrest
    | call callId =>
      have := h (Item.call callId) (List.mem_cons_self _ _)
      simp at this

@[simp] theorem callsOf_filter_isText (items : List Item) :
    callsOf (items.filter Item.isText) = ∅ := by
  refine callsOf_eq_empty_of_no_calls _ ?_
  intro item hitem
  have := (List.mem_filter.mp hitem).2
  cases item <;> simp_all

@[simp] theorem callsOf_filter_isOther (items : List Item) :
    callsOf (items.filter Item.isOther) = ∅ := by
  refine callsOf_eq_empty_of_no_calls _ ?_
  intro item hitem
  have := (List.mem_filter.mp hitem).2
  cases item <;> simp_all

@[simp] theorem callsOf_filter_isCall (items : List Item) :
    callsOf (items.filter Item.isCall) = callsOf items := by
  induction items with
  | nil => rfl
  | cons item rest ih =>
    cases item with
    | text index => simpa using ih
    | other index => simpa using ih
    | call callId => simp [ih]

theorem callsOf_normalize (items : List Item) :
    callsOf (normalize items) = callsOf items := by
  unfold normalize
  rw [callsOf_append, callsOf_append, callsOf_filter_isText,
    callsOf_filter_isOther, callsOf_filter_isCall]
  simp

theorem callsOf_normalizeFor (mode : OrderMode) (items : List Item) :
    callsOf (normalizeFor mode items) = callsOf items := by
  cases mode with
  | grouped => exact callsOf_normalize items
  | nativePreserved => rfl

theorem exists_unique_bucket (item : Item) :
    (item.isText ∧ ¬ item.isOther ∧ ¬ item.isCall) ∨
      (¬ item.isText ∧ item.isOther ∧ ¬ item.isCall) ∨
      (¬ item.isText ∧ ¬ item.isOther ∧ item.isCall) := by
  cases item <;> simp

theorem normalize_perm (items : List Item) :
    List.Perm (normalize items) items := by
  induction items with
  | nil => simp [normalize]
  | cons item rest ih =>
    simp only [normalize, List.append_assoc] at ih ⊢
    cases item with
    | text index =>
      simp only [List.filter_cons, Item.isText_text, Item.isOther_text,
        Item.isCall_text, cond_true, cond_false, List.cons_append]
      exact List.Perm.cons _ ih
    | other index =>
      simp only [List.filter_cons, Item.isText_other, Item.isOther_other,
        Item.isCall_other, cond_true, cond_false, List.cons_append]
      exact List.Perm.trans List.perm_middle (List.Perm.cons _ ih)
    | call callId =>
      simp only [List.filter_cons, Item.isText_call, Item.isOther_call,
        Item.isCall_call, cond_true, cond_false, List.cons_append]
      refine List.Perm.trans ?_ (List.Perm.cons _ ih)
      exact List.Perm.trans (List.Perm.append_left _ List.perm_middle)
        List.perm_middle

theorem length_normalize (items : List Item) :
    (normalize items).length = items.length :=
  (normalize_perm items).length_eq

@[simp] theorem filter_isText_normalize (items : List Item) :
    (normalize items).filter Item.isText = items.filter Item.isText := by
  unfold normalize
  rw [List.filter_append, List.filter_append]
  have hother : (items.filter Item.isOther).filter Item.isText = [] := by
    refine List.filter_eq_nil_iff.mpr ?_
    intro item hitem
    have := (List.mem_filter.mp hitem).2
    cases item <;> simp_all
  have hcall : (items.filter Item.isCall).filter Item.isText = [] := by
    refine List.filter_eq_nil_iff.mpr ?_
    intro item hitem
    have := (List.mem_filter.mp hitem).2
    cases item <;> simp_all
  rw [hother, hcall, List.filter_filter]
  simp

@[simp] theorem filter_isOther_normalize (items : List Item) :
    (normalize items).filter Item.isOther = items.filter Item.isOther := by
  unfold normalize
  rw [List.filter_append, List.filter_append]
  have htext : (items.filter Item.isText).filter Item.isOther = [] := by
    refine List.filter_eq_nil_iff.mpr ?_
    intro item hitem
    have := (List.mem_filter.mp hitem).2
    cases item <;> simp_all
  have hcall : (items.filter Item.isCall).filter Item.isOther = [] := by
    refine List.filter_eq_nil_iff.mpr ?_
    intro item hitem
    have := (List.mem_filter.mp hitem).2
    cases item <;> simp_all
  rw [htext, hcall, List.filter_filter]
  simp

@[simp] theorem filter_isCall_normalize (items : List Item) :
    (normalize items).filter Item.isCall = items.filter Item.isCall := by
  unfold normalize
  rw [List.filter_append, List.filter_append]
  have htext : (items.filter Item.isText).filter Item.isCall = [] := by
    refine List.filter_eq_nil_iff.mpr ?_
    intro item hitem
    have := (List.mem_filter.mp hitem).2
    cases item <;> simp_all
  have hother : (items.filter Item.isOther).filter Item.isCall = [] := by
    refine List.filter_eq_nil_iff.mpr ?_
    intro item hitem
    have := (List.mem_filter.mp hitem).2
    cases item <;> simp_all
  rw [htext, hother, List.filter_filter]
  simp

theorem normalize_idempotent (items : List Item) :
    normalize (normalize items) = normalize items := by
  conv_lhs => rw [normalize]
  rw [filter_isText_normalize, filter_isOther_normalize, filter_isCall_normalize]
  rfl

theorem normalizeFor_idempotent (mode : OrderMode) (items : List Item) :
    normalizeFor mode (normalizeFor mode items) = normalizeFor mode items := by
  cases mode with
  | grouped => exact normalize_idempotent items
  | nativePreserved => rfl

theorem normalizeFor_perm (mode : OrderMode) (items : List Item) :
    List.Perm (normalizeFor mode items) items := by
  cases mode with
  | grouped => exact normalize_perm items
  | nativePreserved => exact List.Perm.refl items

end PromptAssembly.Content
