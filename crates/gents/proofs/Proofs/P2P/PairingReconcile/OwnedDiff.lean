import Proofs.P2P.PairingReconcile.Transition
import Mathlib.Data.List.Dedup

/-! The owned pairing diff: which replicator installs and teardowns one sweep
issues for a peer, given the desired wiring, the live routes the transport
reports and the routes this node recorded as applied. -/

namespace PairingReconcile.OwnedDiff

/-- A route address as production keys it (`text`) and as the transport reads
it.

Premise (transport address grammar): `host:port/p2p/<key>` names the peer
`<key>` with one dial address, `iroh://<key>` names the same peer with none,
and any other text is not a transport address. The `p2p` crate's address
parser owns that grammar; the generated cases carry both forms so a parser
that reads them differently fails the replay. -/
structure Address where
  text : String
  peer : Option String
  dial : Option String
  deriving DecidableEq, Repr

/-- Two addresses name the same route when they are the same text, or the same
peer and at most one of them carries a dial address that the other lacks.

Decision: the transport reports a restored peer by identity alone, so an
identity-only observation certifies a dialable route; two different dial
addresses of one peer do not certify each other. -/
def Address.equivalent (left right : Address) : Bool :=
  left.text == right.text ||
    (left.peer.isSome && left.peer == right.peer &&
      (left.dial == right.dial || left.dial.isNone || right.dial.isNone))

/-- One collection's equality scope: documents whose `field` equals `value`. -/
structure Scope where
  collection : String
  field : String
  value : String
  deriving DecidableEq, Repr

/-- The transport reports an equality scope either as a bare field and value
or as a one-condition predicate. -/
inductive LiveForm where
  | scalar
  | predicate
  deriving DecidableEq, Repr

structure LiveScope where
  scope : Scope
  form : LiveForm
  deriving DecidableEq, Repr

/-- What the transport reports for one installed replicator. `none` means the
transport did not report that aspect. `filterUnobserved` records that the
transport cannot report filters at all for this route. -/
structure LiveRoute where
  address : Address
  collections : Option (List String)
  filterUnobserved : Bool
  filter : Option (List LiveScope)
  observed : Option (List Address)
  deriving DecidableEq, Repr

structure Desired where
  routes : List Address
  collections : List String
  filter : List Scope
  deriving DecidableEq, Repr

structure Applied where
  routes : List Address
  filter : List Scope
  deriving DecidableEq, Repr

inductive OpKind where
  | install
  | teardown
  deriving DecidableEq, Repr

structure Op where
  kind : OpKind
  address : String
  deriving DecidableEq, Repr

def sameMembers {α : Type} [BEq α] (left right : List α) : Bool :=
  left.all right.contains && right.all left.contains

def texts (routes : List Address) : List String :=
  routes.map (·.text)

/-- The live filter certifies the desired one when it carries exactly the
desired scopes, whichever form the transport used to report each. -/
def filterCertified (desired : List Scope) (live : List LiveScope) : Bool :=
  sameMembers desired (live.map (·.scope))

/-- An unreported filter cannot certify a scoped route, and does not disturb an
unscoped one. -/
def liveFilterChanged (desired : Desired) (live : LiveRoute) : Bool :=
  if live.filterUnobserved then !desired.filter.isEmpty
  else match live.filter with
    | none => !desired.filter.isEmpty
    | some reported => !filterCertified desired.filter reported

def collectionsChanged (desired : Desired) (live : LiveRoute) : Bool :=
  match live.collections with
  | none => false
  | some carried => !sameMembers carried desired.collections

/-- The route moved when the transport reports anything other than exactly one
address equivalent to the route's own. -/
def endpointChanged (live : LiveRoute) : Bool :=
  match live.observed with
  | none => false
  | some observed =>
      observed.dedup.length != 1 || observed.any (!·.equivalent live.address)

def drifted (desired : Desired) (applied : Applied) (live : LiveRoute) : Bool :=
  !sameMembers desired.filter applied.filter ||
    liveFilterChanged desired live || collectionsChanged desired live || endpointChanged live

def ordered (addresses : List String) : List String :=
  (addresses.mergeSort fun left right => !decide (right < left)).dedup

/-- Applied routes that are no longer desired. -/
def extras (desired : Desired) (applied : Applied) : List String :=
  (ordered (texts applied.routes)).filter (!(texts desired.routes).contains ·)

/-- Owned, still desired, live routes whose live state no longer matches. -/
def repairs (desired : Desired) (live : List LiveRoute) (applied : Applied) : List String :=
  (ordered (texts (live.map (·.address)))).filter fun address =>
    (texts applied.routes).contains address && (texts desired.routes).contains address &&
      live.any fun route => route.address.text == address && drifted desired applied route

/-- Desired routes with no live replicator. -/
def missing (desired : Desired) (live : List LiveRoute) : List String :=
  (ordered (texts desired.routes)).filter (!(texts (live.map (·.address))).contains ·)

/-- One sweep's operations, in the order they are issued: owned extras are
torn down, drifted owned routes are torn down and reinstalled, missing routes
are installed. -/
def ownedDiff (desired : Desired) (live : List LiveRoute) (applied : Applied) : List Op :=
  (extras desired applied).map (⟨.teardown, ·⟩) ++
    (repairs desired live applied).flatMap (fun address =>
      [⟨.teardown, address⟩, ⟨.install, address⟩]) ++
    (missing desired live).map (⟨.install, ·⟩)

/-- The live wiring is certified: nothing to do, and no scoped route rests on
a filter the transport could not report. -/
def liveMatches (desired : Desired) (live : List LiveRoute) (applied : Applied) : Bool :=
  (ownedDiff desired live applied).isEmpty &&
    (desired.filter.isEmpty || desired.routes.all fun route =>
      !live.any fun observed => observed.address.text == route.text && observed.filterUnobserved)

theorem mem_ordered {addresses : List String} {address : String} :
    address ∈ ordered addresses ↔ address ∈ addresses := by
  simp [ordered, List.mem_dedup, List.mem_mergeSort]

/-- A replicator this node did not apply is never torn down. -/
theorem teardown_is_applied {desired : Desired} {live : List LiveRoute} {applied : Applied}
    {address : String}
    (h : (⟨.teardown, address⟩ : Op) ∈ ownedDiff desired live applied) :
    address ∈ texts applied.routes := by
  simp only [ownedDiff, List.mem_append, List.mem_map, List.mem_flatMap] at h
  rcases h with (⟨a, ha, h⟩ | ⟨a, ha, h⟩) | ⟨a, _, h⟩
  · cases h
    exact mem_ordered.mp (List.mem_filter.mp ha).1
  · have hmem := (List.mem_filter.mp ha).2
    simp only [Bool.and_eq_true, List.contains_iff_mem] at hmem
    simp at h
    simpa [h] using hmem.1.1
  · cases h

/-- Only a desired route is installed. -/
theorem install_is_desired {desired : Desired} {live : List LiveRoute} {applied : Applied}
    {address : String}
    (h : (⟨.install, address⟩ : Op) ∈ ownedDiff desired live applied) :
    address ∈ texts desired.routes := by
  simp only [ownedDiff, List.mem_append, List.mem_map, List.mem_flatMap] at h
  rcases h with (⟨a, _, h⟩ | ⟨a, ha, h⟩) | ⟨a, ha, h⟩
  · cases h
  · have hmem := (List.mem_filter.mp ha).2
    simp only [Bool.and_eq_true, List.contains_iff_mem] at hmem
    simp at h
    simpa [h] using hmem.1.2
  · cases h
    exact mem_ordered.mp (List.mem_filter.mp ha).1

/-- A sweep is quiet exactly when every applied route is still desired, every
desired route is live, and no owned desired route has drifted. -/
theorem ownedDiff_nil_iff {desired : Desired} {live : List LiveRoute} {applied : Applied} :
    ownedDiff desired live applied = [] ↔
      extras desired applied = [] ∧ repairs desired live applied = [] ∧
        missing desired live = [] := by
  have h_repairs : (repairs desired live applied).flatMap
      (fun address => [(⟨.teardown, address⟩ : Op), ⟨.install, address⟩]) = [] ↔
      repairs desired live applied = [] := by
    cases repairs desired live applied <;> simp
  simp only [ownedDiff, List.append_eq_nil_iff, List.map_eq_nil_iff, h_repairs, and_assoc]

/-- A certified sweep issues nothing. -/
theorem liveMatches_quiet {desired : Desired} {live : List LiveRoute} {applied : Applied}
    (h : liveMatches desired live applied = true) :
    ownedDiff desired live applied = [] := by
  simp only [liveMatches, Bool.and_eq_true] at h
  exact List.isEmpty_iff.mp h.1

/-- The reported form of a scope never decides a repair. -/
theorem filterCertified_ignores_form (desired : List Scope) (live : List LiveScope)
    (form : LiveForm) :
    filterCertified desired (live.map fun scope => { scope with form := form }) =
      filterCertified desired live := by
  simp [filterCertified, List.map_map, Function.comp_def]

/-- An identity-only observation of the route's own peer is not a move. -/
theorem identity_observation_keeps_route (route observed : Address)
    (h_peer : route.peer.isSome = true) (h_same : observed.peer = route.peer)
    (h_identity : observed.dial = none) (live : LiveRoute)
    (h_route : live.address = route) (h_observed : live.observed = some [observed]) :
    endpointChanged live = false := by
  subst h_route
  simp [endpointChanged, h_observed, Address.equivalent, h_same, h_identity, h_peer,
    List.dedup_cons_of_not_mem]

end PairingReconcile.OwnedDiff
