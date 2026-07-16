/-!
# EditMatch — model (#738, #724)

The `edit_file` matcher: a deterministic relaxation ladder over lines
(exact → trailing-whitespace-insensitive → trim/indentation-insensitive →
unicode-normalized), an optimistic-concurrency precondition (expected
content identity), uniqueness gating, and a single pure decision function
shared by dry-run and apply.

A line is abstracted as leading blanks, body, and trailing blanks; interior
whitespace lives in `body`, so trim-level strategies never see it. The
unicode fold is an arbitrary idempotent character map (the runtime's
punctuation/space normalization is one instance). Content identity is
modeled by the document itself: the runtime's cryptographic hash is the
boundary that makes `expected = current` checkable without the bytes.
-/

namespace Proofs.EditMatch

/-- A line: leading blank count, body characters, trailing blank count. -/
structure Line where
  lead  : Nat
  body  : List Char
  trail : Nat
  deriving Repr, DecidableEq

abbrev Doc := List Line

/-- Ladder strategies, strictest first. -/
inductive Strategy
  | exact
  | trailingWs
  | trim
  | unicode
  deriving Repr, DecidableEq

namespace Strategy

/-- Ladder order: every strategy the matcher tries, strictest first. -/
def ladder : List Strategy := [.exact, .trailingWs, .trim, .unicode]

end Strategy

/-- The comparison key a strategy sees for a line. Coarser strategies
    project away more of the line. `fold` is the unicode normalization. -/
def keyAt (fold : Char → Char) : Strategy → Line → Nat × List Char × Nat
  | .exact,      l => (l.lead, l.body, l.trail)
  | .trailingWs, l => (l.lead, l.body, 0)
  | .trim,       l => (0, l.body, 0)
  | .unicode,    l => (0, l.body.map fold, 0)

/-- Two line windows match under a strategy iff their keys agree pointwise. -/
def windowMatches (fold : Char → Char) (s : Strategy) (window pat : Doc) : Bool :=
  window.length == pat.length
    && (window.zip pat).all fun (a, b) => keyAt fold s a == keyAt fold s b

/-- Does `pat` match `doc` at index `i` under strategy `s`? -/
def matchesAt (fold : Char → Char) (s : Strategy) (doc pat : Doc) (i : Nat) : Bool :=
  windowMatches fold s ((doc.drop i).take pat.length) pat

/-- All match positions for `pat` in `doc` under `s`. -/
def occurrences (fold : Char → Char) (s : Strategy) (doc pat : Doc) : List Nat :=
  (List.range (doc.length + 1 - pat.length)).filter fun i =>
    matchesAt fold s doc pat i

/-- The ladder decision: the first strategy (strictest first) with at least
    one occurrence, together with all its occurrences. -/
def ladderMatch (fold : Char → Char) (doc pat : Doc) :
    Option (Strategy × List Nat) :=
  go Strategy.ladder
where
  go : List Strategy → Option (Strategy × List Nat)
    | [] => none
    | s :: rest =>
      let occ := occurrences fold s doc pat
      if occ.isEmpty then go rest else some (s, occ)

/-- Greedy non-overlapping selection from (ascending) match positions:
    overlapping windows would invalidate each other's line ranges when
    splicing. Mirrors the runtime's `window_occurrences` selection. Fuel
    form (fuel = input length suffices: filtering never grows the list)
    keeps the proofs structural. -/
def selectDisjointGo (len : Nat) : Nat → List Nat → List Nat
  | 0, _ => []
  | _, [] => []
  | fuel + 1, i :: rest =>
    i :: selectDisjointGo len fuel (rest.filter (fun j => i + len ≤ j))

def selectDisjoint (len : Nat) (occ : List Nat) : List Nat :=
  selectDisjointGo len occ.length occ

/-- Splice `repl` over the `len`-line window at `i`. -/
def splice (doc : Doc) (i len : Nat) (repl : Doc) : Doc :=
  doc.take i ++ repl ++ doc.drop (i + len)

/-- Re-indent the replacement to the matched site: shift every replacement
    line's lead by the delta between the matched window's first line and the
    pattern's first line. Only coarse strategies (which ignored `lead` when
    matching) re-indent; exact and trailing-ws matched the lead literally. -/
def reindent (s : Strategy) (doc pat repl : Doc) (i : Nat) : Doc :=
  match s with
  | .exact => repl
  | .trailingWs => repl
  | _ =>
    match doc.drop i, pat with
    | m :: _, p :: _ =>
      repl.map fun l => { l with lead := l.lead + m.lead - p.lead }
    | _, _ => repl

/-- One edit request. `expected` is the optimistic-concurrency precondition:
    the document version the model believes it is editing (#724). -/
structure Request where
  pattern     : Doc
  replacement : Doc
  replaceAll  : Bool
  expected    : Option Doc
  deriving Repr

/-- Decision outcomes. `applied` carries the full result document: dry-run
    and apply are projections of the same decision. -/
inductive Outcome
  | applied (result : Doc) (strategy : Strategy)
  | rejectedStale
  | notFound
  | ambiguous (strategy : Strategy) (count : Nat)
  | noop (strategy : Strategy)
  deriving Repr

/-- Replace every occurrence, right-to-left so indices stay valid. -/
def spliceAll (s : Strategy) (doc pat repl : Doc)
    (occ : List Nat) : Doc :=
  occ.reverse.foldl
    (fun acc i => splice acc i pat.length (reindent s doc pat repl i))
    doc

/-- The result document for an accepted match set. -/
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

/-- The single pure decision shared by dry-run and apply. Order of gates:
    stale precondition, ladder match, uniqueness, no-op. -/
def decide (fold : Char → Char) (doc : Doc) (req : Request) : Outcome :=
  match req.expected with
  | some expected =>
    if expected = doc then decideMatched fold doc req else .rejectedStale
  | none => decideMatched fold doc req

/-- A trivial filesystem: the current document. Apply writes the decided
    result; dry-run never writes. -/
def applyFs (fold : Char → Char) (doc : Doc) (req : Request) : Doc :=
  match decide fold doc req with
  | .applied result _ => result
  | _ => doc

def dryRunFs (_fold : Char → Char) (doc : Doc) (_req : Request) : Doc := doc

/-- Convenience operations desugar onto `Request`; there is exactly one
    matcher (#738 insert_after / insert_before / delete). -/
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
