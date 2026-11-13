import Proofs.Tools.ToolPolicy.PluginData
import Proofs.Conformance.Family

namespace Conformance.PluginDataWrites

open Lean ToolPolicy.PluginData

deriving instance ToJson for DataOp, Invocation, Stamp, OnConflict, ConflictReceipt

inductive RefusalKind where
  | stampUnavailable
  | other
  deriving ToJson

structure RefusalWire where
  kind : RefusalKind
  stamp : Option Stamp
  invocation : Option Invocation
  deriving ToJson

def RefusalWire.ofModel : Refusal → RefusalWire
  | .stampUnavailable s inv => { kind := .stampUnavailable, stamp := some s, invocation := some inv }
  | .other => { kind := .other, stamp := none, invocation := none }

/-- An output of `count` copies of one write to `Note` or, when the grant does
not list it, `Other`; the grant declares `owner` stamped from `stamp`, and
knows the written collection's fields `owner` and `body` when `fieldsKnown`. -/
structure AdmitInput where
  count : Nat
  op : DataOp
  collectionGranted : Bool
  fieldsKnown : Bool
  opGranted : Bool
  docId : Bool
  onConflict : Bool
  guestStamped : Bool
  stamp : Stamp
  invocation : Invocation
  deriving ToJson

structure AdmitOutput where
  refusal : Option RefusalWire
  stampedFields : List String
  deriving ToJson

def AdmitInput.write (i : AdmitInput) : GrantedWrite :=
  { op := i.op, collectionGranted := i.collectionGranted, fieldsKnown := i.fieldsKnown,
    opGranted := i.opGranted, docId := i.docId, onConflict := i.onConflict,
    guestStamped := i.guestStamped }

def noteFields : List String := ["owner", "body"]

def bools : List Bool := [false, true]

def ops : List DataOp := [.insert, .update, .delete]

def clear (op : DataOp) (stamp : Stamp) (invocation : Invocation) : AdmitInput :=
  { count := 1, op, collectionGranted := true, fieldsKnown := true, opGranted := true,
    docId := op != .insert, onConflict := false, guestStamped := false, stamp, invocation }

@[conformance_family]
def admit : Family AdmitInput AdmitOutput where
  inputs :=
    (ops.flatMap fun op => bools.flatMap fun collectionGranted => bools.flatMap fun fieldsKnown =>
      bools.flatMap fun opGranted => bools.flatMap fun docId => bools.flatMap fun onConflict =>
        bools.map fun guestStamped =>
          { count := 1, op, collectionGranted, fieldsKnown, opGranted, docId, onConflict,
            guestStamped, stamp := .callerDid, invocation := .toolCall })
    ++ ([Stamp.callerDid, .sessionId, .requestDocId, .toolCallDocId, .jobRowDocId,
          .chainRootDocId].flatMap fun stamp =>
        [Invocation.toolCall, .jobStep, .callback, .ux].map fun invocation =>
          clear .insert stamp invocation)
    ++ [maxWrites, maxWrites + 1].map fun count =>
      { clear .insert .callerDid .toolCall with count }
  model := fun i =>
    let declared := [("owner", i.stamp)]
    let refusal := admitWrites declared noteFields i.invocation (List.replicate i.count i.write)
    { refusal := refusal.map RefusalWire.ofModel
      stampedFields := if refusal.isSome then [] else stampedFields declared noteFields i.write }

structure RowWire where
  rank : Option Nat
  body : Option Nat
  deriving ToJson

/-- One `on_conflict` insert on a node: rows already stored under the case's
own `key` in `_docID` order with their `created_at`, the grant's unique-key
count, whether the write names the key field, the stored row it repeats,
and its `on_conflict`. A twin needs a collection without a unique index. -/
structure ConflictInput where
  collection : String
  key : String
  uniqueKeys : Nat
  keyPresent : Bool
  stored : List (Option Nat)
  sameAs : Option Nat
  onConflict : OnConflict
  deriving ToJson

structure ConflictOutput where
  receipt : Option ConflictReceipt
  existing : Option Nat
  error : Option String
  rows : List RowWire
  deriving ToJson

def ConflictInput.write (i : ConflictInput) : ConflictWrite :=
  { uniqueKeys := i.uniqueKeys, keyPresent := i.keyPresent, stored := i.stored,
    sameAs := i.sameAs, onConflict := i.onConflict }

def conflictShapes : List (Nat × Bool × List (Option Nat) × Option Nat × OnConflict) :=
  let storeds : List (List (Option Nat)) :=
    [[], [some 1], [none], [some 1, some 2], [some 2, some 1], [some 3, some 3], [none, some 1],
     [some 1, none]]
  let resolved := storeds.flatMap fun stored =>
    (none :: (List.range stored.length).map some).flatMap fun sameAs =>
      [OnConflict.ignore, .fail].map fun onConflict => (1, true, stored, sameAs, onConflict)
  let refused := [([] : List (Option Nat)), [some 1]].flatMap fun stored =>
    [OnConflict.ignore, .fail].flatMap fun onConflict =>
      [(0, true, stored, none, onConflict), (2, true, stored, none, onConflict),
       (1, false, stored, none, onConflict)]
  resolved ++ refused

@[conformance_family]
def conflict : Family ConflictInput ConflictOutput where
  inputs := conflictShapes.zipIdx.map fun ((uniqueKeys, keyPresent, stored, sameAs, onConflict), n) =>
    { collection := if stored.length ≤ 1 then "Keyed" else "Twin", key := s!"k{n}", uniqueKeys,
      keyPresent, stored, sameAs, onConflict }
  model := fun i =>
    let w := i.write
    let rows := (rowsAfter w).map fun (rank, body) => { rank, body }
    match resolveConflict w with
    | .ok (receipt, existing) => { receipt := some receipt, existing, error := none, rows }
    | .error refusal =>
      { receipt := none, existing := none, rows,
        error := some (conflictRefusalText i.collection i.key i.uniqueKeys refusal) }

end Conformance.PluginDataWrites
