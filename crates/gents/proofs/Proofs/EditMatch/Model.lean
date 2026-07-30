namespace Proofs.EditMatch

structure Line where
  lead  : Nat
  body  : List Char
  trail : Nat
  deriving Repr, DecidableEq

abbrev Doc := List Line

inductive Strategy
  | exact
  | trailingWs
  | trim
  | unicode
  deriving Repr, DecidableEq

namespace Strategy

def ladder : List Strategy := [.exact, .trailingWs, .trim, .unicode]

end Strategy

def keyAt (fold : Char → Char) : Strategy → Line → Nat × List Char × Nat
  | .exact,      l => (l.lead, l.body, l.trail)
  | .trailingWs, l => (l.lead, l.body, 0)
  | .trim,       l => (0, l.body, 0)
  | .unicode,    l => (0, l.body.map fold, 0)

def windowMatches (fold : Char → Char) (s : Strategy) (window pat : Doc) : Bool :=
  window.length == pat.length
    && (window.zip pat).all fun (a, b) => keyAt fold s a == keyAt fold s b

def matchesAt (fold : Char → Char) (s : Strategy) (doc pat : Doc) (i : Nat) : Bool :=
  windowMatches fold s ((doc.drop i).take pat.length) pat

def occurrences (fold : Char → Char) (s : Strategy) (doc pat : Doc) : List Nat :=
  (List.range (doc.length + 1 - pat.length)).filter fun i =>
    matchesAt fold s doc pat i

def ladderMatch (fold : Char → Char) (doc pat : Doc) :
    Option (Strategy × List Nat) :=
  go Strategy.ladder
where
  go : List Strategy → Option (Strategy × List Nat)
    | [] => none
    | s :: rest =>
      let occ := occurrences fold s doc pat
      if occ.isEmpty then go rest else some (s, occ)

def selectDisjointGo (len : Nat) : Nat → List Nat → List Nat
  | 0, _ => []
  | _, [] => []
  | fuel + 1, i :: rest =>
    i :: selectDisjointGo len fuel (rest.filter (fun j => i + len ≤ j))

def selectDisjoint (len : Nat) (occ : List Nat) : List Nat :=
  selectDisjointGo len occ.length occ

def splice (doc : Doc) (i len : Nat) (repl : Doc) : Doc :=
  doc.take i ++ repl ++ doc.drop (i + len)

def reindent (s : Strategy) (doc pat repl : Doc) (i : Nat) : Doc :=
  match s with
  | .exact => repl
  | .trailingWs => repl
  | _ =>
    match doc.drop i, pat with
    | m :: _, p :: _ =>
      repl.map fun l => { l with lead := l.lead + m.lead - p.lead }
    | _, _ => repl

structure Request where
  pattern     : Doc
  replacement : Doc
  replaceAll  : Bool
  expected    : Option Doc
  deriving Repr

inductive Outcome
  | applied (result : Doc) (strategy : Strategy)
  | rejectedStale
  | notFound
  | ambiguous (strategy : Strategy) (count : Nat)
  | noop (strategy : Strategy)
  deriving Repr

def spliceAll (s : Strategy) (doc pat repl : Doc)
    (occ : List Nat) : Doc :=
  occ.reverse.foldl
    (fun acc i => splice acc i pat.length (reindent s doc pat repl i))
    doc

def chosenResult (s : Strategy) (doc : Doc) (req : Request)
    (occ : List Nat) : Doc :=
  if req.replaceAll then
    spliceAll s doc req.pattern req.replacement occ
  else
    match occ with
    | i :: _ =>
      splice doc i req.pattern.length
        (reindent s doc req.pattern req.replacement i)
    | [] => doc

def decideMatched (fold : Char → Char) (doc : Doc) (req : Request) : Outcome :=
  match ladderMatch fold doc req.pattern with
  | none => .notFound
  | some (s, occ) =>
    if (selectDisjoint req.pattern.length occ).length = 1
        ∨ req.replaceAll = true then
      if chosenResult s doc req (selectDisjoint req.pattern.length occ) = doc
      then .noop s
      else .applied (chosenResult s doc req (selectDisjoint req.pattern.length occ)) s
    else
      .ambiguous s (selectDisjoint req.pattern.length occ).length

def decide (fold : Char → Char) (doc : Doc) (req : Request) : Outcome :=
  match req.expected with
  | some expected =>
    if expected = doc then decideMatched fold doc req else .rejectedStale
  | none => decideMatched fold doc req

def applyFs (fold : Char → Char) (doc : Doc) (req : Request) : Doc :=
  match decide fold doc req with
  | .applied result _ => result
  | _ => doc

def dryRunFs (_fold : Char → Char) (doc : Doc) (_req : Request) : Doc := doc

def insertAfter (pat text : Doc) (expected : Option Doc) : Request :=
  { pattern := pat, replacement := pat ++ text, replaceAll := false
  , expected := expected }

def insertBefore (pat text : Doc) (expected : Option Doc) : Request :=
  { pattern := pat, replacement := text ++ pat, replaceAll := false
  , expected := expected }

def deleteText (pat : Doc) (expected : Option Doc) : Request :=
  { pattern := pat, replacement := [], replaceAll := false
  , expected := expected }

end Proofs.EditMatch
