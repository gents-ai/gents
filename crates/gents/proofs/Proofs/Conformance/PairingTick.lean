import Proofs.P2P.PairingReconcile.Tick
import Proofs.Conformance.PairingOwnedDiff

namespace Conformance.PairingTick

open Lean PairingReconcile.OwnedDiff PairingReconcile.Tick

deriving instance ToJson for Replicator, Sweep

structure Input where
  name : String
  replicators : List Replicator
  applied : Applied
  sweeps : List Sweep
  deriving ToJson

/-- A replicator as an observer of the transport lists it. -/
structure LiveView where
  address : String
  collections : List String
  filter : Option (List Scope)
  deriving ToJson

structure TickOutput where
  readFailed : Bool
  dialed : Bool
  peerActive : Bool
  ops : List Op
  liveMatches : Bool
  applied : List String
  appliedFilter : List Scope
  live : List LiveView
  deriving ToJson

structure Output where
  ticks : List TickOutput
  deriving ToJson

private def byCollection (scopes : List Scope) : List Scope :=
  scopes.mergeSort fun a b => !decide (b.collection < a.collection)

private def view (replicator : Replicator) : LiveView :=
  { address := replicator.address.text
    collections := ordered replicator.collections
    filter := replicator.filter.map fun scopes => byCollection (scopes.map (·.scope)) }

private def run : World → List Sweep → List TickOutput
  | _, [] => []
  | world, sweep :: rest =>
      let outcome := tick sweep world
      { readFailed := outcome.readFailed
        dialed := outcome.dialed
        peerActive := outcome.peerActive
        ops := outcome.ops
        liveMatches := outcome.liveMatches
        applied := ordered (texts outcome.world.applied.routes)
        appliedFilter := byCollection outcome.world.applied.filter
        live := (outcome.world.replicators.map view).mergeSort
          fun a b => !decide (b.address < a.address) } :: run outcome.world rest

private def home : String := "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb"
private def away : String := "7fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bc"

private def dialable (peer : String) (port : Nat) : Address :=
  let dial := s!"127.0.0.1:{port}"
  { text := s!"{dial}/p2p/{peer}", peer := some peer, dial := some dial }

private def plain (text : String) : Address :=
  { text := text, peer := none, dial := none }

private def first : Address := dialable home 56091
private def second : Address := dialable home 56092
private def nameless : Address := { text := s!"iroh://{home}", peer := some home, dial := none }
private def unparsed : Address := plain "addr1"
private def manual : Address := plain "manual"

private def scope : Scope := ⟨"AgentRequest", "node_did", "did:key:mandrake"⟩

private def wiring (routes : List Address) (collections : List String := ["AgentRequest"])
    (filter : List Scope := []) : Desired :=
  { routes, collections, filter }

private def reads (desired : Option Desired) (activePeers : List String := [])
    (activeReadable : Bool := true) : Sweep :=
  { desiredReadable := true, desired, activeReadable, activePeers }

private def unreadable : Sweep :=
  { desiredReadable := false, desired := none, activeReadable := true, activePeers := [] }

private def held (address : Address) (collections : List String := ["AgentRequest"])
    (filter : Option (List LiveScope) := some []) : Replicator :=
  { address, collections, filter }

private def nothing : Applied := { routes := [], filter := [] }

private def inputs : List Input :=
  [ { name := "first_tick_installs_and_the_second_is_quiet"
      replicators := [], applied := nothing
      sweeps := [reads (some (wiring [unparsed])), reads (some (wiring [unparsed]))] }
  , { name := "stale_applied_endpoint_is_torn_down_once"
      replicators := [], applied := { routes := [first, second], filter := [] }
      sweeps := [reads (some (wiring [second])) [home], reads (some (wiring [second])) [home]] }
  , { name := "grown_collections_reinstall_then_settle"
      replicators := [], applied := nothing
      sweeps :=
        [ reads (some (wiring [unparsed] ["AgentRequest"] [scope]))
        , reads (some (wiring [unparsed] ["Network", "AgentRequest"] [scope]))
        , reads (some (wiring [unparsed] ["Network", "AgentRequest"] [scope])) ] }
  , { name := "unreadable_active_peers_dial"
      replicators := [held first], applied := { routes := [first], filter := [] }
      sweeps := [reads (some (wiring [first])) [home] false] }
  , { name := "another_active_peer_dials"
      replicators := [held first], applied := { routes := [first], filter := [] }
      sweeps := [reads (some (wiring [first])) [away]] }
  , { name := "active_peer_is_not_redialed"
      replicators := [held first], applied := { routes := [first], filter := [] }
      sweeps := [reads (some (wiring [first])) [away, home]] }
  , { name := "route_without_a_dial_address_is_always_dialed"
      replicators := [held nameless], applied := { routes := [nameless], filter := [] }
      sweeps := [reads (some (wiring [nameless])) [home]] }
  , { name := "active_peer_scope_upgrade_reinstalls_without_dialing"
      replicators := [held first ["Network"]], applied := { routes := [first], filter := [] }
      sweeps := [reads (some (wiring [first] [] [scope])) [home]] }
  , { name := "moved_endpoint_dials_an_active_peer"
      replicators := [held first], applied := { routes := [first], filter := [] }
      sweeps := [reads (some (wiring [second])) [home], reads (some (wiring [second])) [home]] }
  , { name := "empty_desired_tears_down_only_applied_routes"
      replicators := [held unparsed, held manual], applied := { routes := [unparsed], filter := [] }
      sweeps := [reads (some (wiring [] []))] }
  , { name := "absent_desired_tears_down_and_forgets"
      replicators := [held unparsed, held manual], applied := { routes := [unparsed], filter := [scope] }
      sweeps := [reads none] }
  , { name := "absent_desired_with_nothing_applied_is_quiet"
      replicators := [held manual], applied := nothing
      sweeps := [reads none] }
  , { name := "absent_desired_forgets_a_route_the_transport_lost"
      replicators := [], applied := { routes := [unparsed], filter := [scope] }
      sweeps := [reads none] }
  , { name := "unreadable_desired_changes_nothing_until_it_reads"
      replicators := [held unparsed], applied := { routes := [unparsed], filter := [] }
      sweeps := [unreadable, reads none] }
  , { name := "unreported_filters_repair_a_scoped_route_once"
      replicators := [held unparsed ["AgentRequest"] none]
      applied := { routes := [unparsed], filter := [scope] }
      sweeps :=
        [ reads (some (wiring [unparsed] ["AgentRequest"] [scope]))
        , reads (some (wiring [unparsed] ["AgentRequest"] [scope])) ] }
  , { name := "empty_live_filter_repairs_a_scoped_route"
      replicators := [held unparsed]
      applied := { routes := [unparsed], filter := [scope] }
      sweeps := [reads (some (wiring [unparsed] ["AgentRequest"] [scope]))] }
  , { name := "scalar_live_filter_certifies_a_scoped_route"
      replicators := [held unparsed ["AgentRequest"] (some [⟨scope, .scalar⟩])]
      applied := { routes := [unparsed], filter := [scope] }
      sweeps := [reads (some (wiring [unparsed] ["AgentRequest"] [scope]))] }
  , { name := "unowned_live_route_is_left_alone"
      replicators := [held unparsed ["Network"]], applied := nothing
      sweeps := [reads (some (wiring [unparsed]))] }
  , { name := "applied_route_the_transport_lost_is_reinstalled"
      replicators := [], applied := { routes := [unparsed], filter := [] }
      sweeps := [reads (some (wiring [unparsed]))] } ]

@[conformance_family]
def cases : Family Input Output where
  inputs := inputs
  model := fun i => { ticks := run { replicators := i.replicators, applied := i.applied } i.sweeps }

end Conformance.PairingTick
