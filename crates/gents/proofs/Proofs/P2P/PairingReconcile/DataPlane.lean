import Proofs.P2P.PairingReconcile.OwnedDiff
import Proofs.Documents.ScopeTemplates.Derivation

/-! How a stored pairing row and the signed enrollment endpoint become the
desired wiring of one peer. The row is a witness that a route was asked for;
the signed endpoint decides where it goes and the local DID decides what it
carries. -/

namespace PairingReconcile.DataPlane

open ScopeTemplates hiding Scope
open PairingReconcile.OwnedDiff (Scope ordered)

/-- A stored `PeerPairingDesired` row. Every field is as written by whoever
wrote the row, so none of it is trusted for identity or address. -/
structure Row where
  routeKey : Option String
  nodeDid : Option String
  addresses : List String
  template : Option String
  source : Option String
  requestDigest : Option String
  sequence : Option Nat
  expiresAt : Option String
  deriving DecidableEq, Repr

/-- The endpoint a verified enrollment names, with the authorization
generation that verified it. -/
structure Endpoint where
  desiredId : String
  peerId : String
  nodeDid : String
  address : String
  requestDigest : String
  sequence : Nat
  expiresAt : String
  deriving DecidableEq, Repr

inductive Refusal where
  | foreignScope
  | missingTemplate
  | unknownTemplate
  | missingRouteKey
  | undirectedRouteKey
  | unsignedPeer
  deriving DecidableEq, Repr

structure Desired where
  addresses : List String
  collections : List String
  filter : List Scope
  templates : List String
  deriving DecidableEq, Repr

/-- A stored optional string counts only when it is not blank. -/
def stated (value : Option String) : Option String :=
  (value.map String.trim).filter (· ≠ "")

def enrollmentSource : String := "enrollment"

/-- The direction a client route key ends with, or `none` when it carries no
recognized direction after a non-blank directory identity. -/
def routeDirection (key : String) : Option RouteDirection :=
  let key := key.trim
  let directed (suffix : String) (direction : RouteDirection) : Option (Option RouteDirection) :=
    if key.endsWith suffix then
      some (if (key.dropRight suffix.length).trim = "" then none else some direction)
    else none
  match directed ":client-to-runtime" .clientToRuntime with
  | some verdict => verdict
  | none => (directed ":runtime-to-client" .runtimeToClient).join

/-- Decision: an enrollment base route is keyed by the member peer, not by a
directional suffix, and always runs from the runtime to the client. -/
def clientDirection (row : Row) : Except Refusal RouteDirection :=
  match stated row.routeKey with
  | none => .error .missingRouteKey
  | some key =>
      match routeDirection key with
      | some direction => .ok direction
      | none =>
          if row.source = some enrollmentSource then .ok .runtimeToClient
          else .error .undirectedRouteKey

def signedAddresses (endpoint : Endpoint) : List String :=
  [endpoint.address.trim].filter (· ≠ "")

/-- What a client route carries in one direction. -/
def clientWiring (direction : RouteDirection) (endpoint : Endpoint) (selfDid : String)
    (template : Template) : Desired :=
  let (requester, owner) := match direction with
    | .clientToRuntime => (selfDid, endpoint.nodeDid.trim)
    | .runtimeToClient => (endpoint.nodeDid.trim, selfDid)
  { addresses := signedAddresses endpoint
    collections := ordered (clientRouteCollections direction)
    filter := (clientRouteFilters direction requester owner).flatMap
      fun predicate => predicate.clauses.map fun clause =>
        ⟨predicate.collection, clause.field, clause.value⟩
    templates := [template.id] }

/-- The desired wiring one row yields. The address is the signed endpoint's
whatever the row says, and every scope is on the local DID. -/
def desiredOf (row : Row) (endpoint : Endpoint) (selfDid : String) : Except Refusal Desired :=
  if (stated row.nodeDid).any (· ≠ selfDid) then .error .foreignScope
  else match stated row.template with
    | none => .error .missingTemplate
    | some id =>
        match resolveTemplate builtinCatalog id with
        | none => .error .unknownTemplate
        | some template =>
            match template.scope with
            | .perCollection rules =>
                .ok { addresses := signedAddresses endpoint
                      collections := ordered template.collections
                      filter := (scopeFilter (.perCollection rules) template.collections
                        endpoint.nodeDid.trim selfDid).map fun f => ⟨f.collection, f.field, f.value⟩
                      templates := [template.id] }
            | .clientRoute =>
                match clientDirection row with
                | .error refusal => .error refusal
                | .ok direction =>
                    if endpoint.nodeDid.trim = "" then .error .unsignedPeer
                    else .ok (clientWiring direction endpoint selfDid template)

/-- The enrollment that admits a peer's routes: the one keyed by that route
and naming another node. A node never routes to itself. -/
def admitted (entries : List Endpoint) (key selfDid : String) : Option Endpoint :=
  entries.find? fun entry => entry.desiredId == key && entry.nodeDid != selfDid

/-- The row enrollment owns: an enrollment row witnessing exactly the admitted
generation, rewritten from that enrollment so nothing the row claimed
survives. -/
def enrollmentBase (row : Row) (entry : Endpoint) (selfDid : String) : Option Row :=
  if row.source = some enrollmentSource ∧ row.requestDigest = some entry.requestDigest ∧
      row.sequence = some entry.sequence ∧ row.expiresAt = some entry.expiresAt then
    some { row with
      routeKey := some entry.desiredId
      nodeDid := some selfDid
      addresses := [entry.address]
      template := some clientTemplate.id }
  else none

/-- The row a local writer owns: any stated source other than enrollment. It
chooses a template; peer, scope and address still come from the enrollment. -/
def localOverlay (row : Row) (entry : Endpoint) (selfDid : String) : Option Row :=
  match row.source.map String.trim with
  | none => none
  | some source =>
      if source = "" ∨ source = enrollmentSource then none
      else some { row with
        routeKey := some entry.peerId
        nodeDid := some selfDid
        addresses := [entry.address] }

/-- Addresses, collections and templates are sets and a filter holds one scope
per collection; this is the one order they are written in. -/
def Desired.canonical (desired : Desired) : Desired :=
  { addresses := ordered desired.addresses
    collections := ordered desired.collections
    filter := desired.filter.mergeSort fun a b => !decide (b.collection < a.collection)
    templates := ordered desired.templates }

/-- The base and data-plane layers of one peer as one desired wiring. The
data-plane layer comes from the verified registry, so its address replaces the
base one and its scope replaces the base scope of the same collection; a node
has no base layer towards itself. -/
def mergeLayered (localDid peerDid : String) (base dataPlane : Option Desired) :
    Option Desired :=
  match (if peerDid = localDid then none else base), dataPlane with
  | none, none => none
  | some desired, none | none, some desired => some desired.canonical
  | some left, some right =>
      some (Desired.canonical
        { addresses := if right.addresses.isEmpty then left.addresses else right.addresses
          collections := left.collections ++ right.collections
          filter := right.filter ++ left.filter.filter fun scope =>
            !right.filter.any (·.collection == scope.collection)
          templates := left.templates ++ right.templates })

/-- A row scoped to another node's DID is refused whatever else it says. -/
theorem foreign_scope_refused (row : Row) (endpoint : Endpoint) (selfDid did : String)
    (h_stated : stated row.nodeDid = some did) (h_foreign : did ≠ selfDid) :
    desiredOf row endpoint selfDid = .error .foreignScope := by
  simp [desiredOf, h_stated, h_foreign]

/-- The address a row claims never reaches the desired wiring. -/
theorem desired_address_is_signed {row : Row} {endpoint : Endpoint} {selfDid : String}
    {desired : Desired} (h : desiredOf row endpoint selfDid = .ok desired) :
    desired.addresses = signedAddresses endpoint := by
  unfold desiredOf at h
  split at h
  · cases h
  · split at h
    · cases h
    · split at h
      · cases h
      · split at h
        · cases h; rfl
        · split at h
          · cases h
          · split at h
            · cases h
            · cases h; rfl

/-- Every scope of an accepted row is on the local DID: neither the row nor
the peer chooses whose documents a route carries. -/
theorem desired_scopes_local_did {row : Row} {endpoint : Endpoint} {selfDid : String}
    {desired : Desired} (h : desiredOf row endpoint selfDid = .ok desired) :
    ∀ scope ∈ desired.filter, scope.value = selfDid := by
  unfold desiredOf at h
  split at h
  · cases h
  · split at h
    · cases h
    · split at h
      · cases h
      · split at h
        · cases h
          intro scope h_scope
          simp only [scopeFilter, List.map_map, List.mem_map, Function.comp] at h_scope
          obtain ⟨rule, _, rfl⟩ := h_scope
          cases rule.source <;> rfl
        · split at h
          · cases h
          · split at h
            · cases h
            · rename_i direction _ _
              cases h
              intro scope h_scope
              cases direction <;> simp [clientWiring, clientRouteFilters] at h_scope <;>
                simp [h_scope]

/-- Both directions of a client route carry the same wiring: the registry,
scoped to the local DID, towards the signed address. The direction a route key
spells decides whether the key is admitted, never what the route carries. -/
theorem client_wiring_ignores_direction (endpoint : Endpoint) (selfDid : String)
    (template : Template) :
    clientWiring .clientToRuntime endpoint selfDid template =
      clientWiring .runtimeToClient endpoint selfDid template := by
  simp [clientWiring, clientRouteFilters, clientRouteCollections, clientToRuntimeCollections,
    clientCollections]

/-- Enrollment and a local writer never own the same row. -/
theorem owners_disjoint (row : Row) (entry : Endpoint) (selfDid : String) :
    enrollmentBase row entry selfDid = none ∨ localOverlay row entry selfDid = none := by
  by_cases h : row.source = some enrollmentSource
  · right
    have h_trim : enrollmentSource.trim = enrollmentSource := by native_decide
    simp [localOverlay, h, h_trim]
  · left
    simp [enrollmentBase, h]

/-- What the enrollment-owned row routes is the enrollment's, not the row's. -/
theorem enrollmentBase_is_canonical {row canonical : Row} {entry : Endpoint} {selfDid : String}
    (h : enrollmentBase row entry selfDid = some canonical) :
    canonical.routeKey = some entry.desiredId ∧ canonical.nodeDid = some selfDid ∧
      canonical.addresses = [entry.address] ∧ canonical.template = some "client" := by
  unfold enrollmentBase at h
  split at h
  · cases h; exact ⟨rfl, rfl, rfl, rfl⟩
  · cases h

theorem self_has_no_base_layer (did : String) (base : Option Desired) :
    mergeLayered did did base none = none := by
  simp [mergeLayered]

end PairingReconcile.DataPlane
