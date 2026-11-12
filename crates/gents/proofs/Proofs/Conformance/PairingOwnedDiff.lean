import Proofs.P2P.PairingReconcile.OwnedDiff
import Proofs.Conformance.Family

namespace Conformance.PairingOwnedDiff

open Lean PairingReconcile.OwnedDiff

deriving instance ToJson for Address, Scope, LiveForm, LiveScope, LiveRoute, Desired, Applied,
  OpKind, Op

structure Input where
  name : String
  desired : Desired
  live : List LiveRoute
  applied : Applied
  deriving ToJson

structure Output where
  ops : List Op
  liveMatches : Bool
  deriving ToJson

private def home : String := "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb"
private def away : String := "7fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bc"

private def dialable (peer : String) (port : Nat) : Address :=
  let dial := s!"127.0.0.1:{port}"
  { text := s!"{dial}/p2p/{peer}", peer := some peer, dial := some dial }

private def identity (peer : String) : Address :=
  { text := s!"iroh://{peer}", peer := some peer, dial := none }

private def plain (text : String) : Address :=
  { text := text, peer := none, dial := none }

private def route : Address := dialable home 56091
private def moved : Address := dialable home 56090
private def other : Address := dialable away 56092
private def manual : Address := plain "manual"

private def phone : Scope := ⟨"AgentRequest", "did", "did:key:phone"⟩
private def stranger : Scope := ⟨"AgentRequest", "did", "did:key:someone-else"⟩
private def directory : Scope := ⟨"NodeDirectoryEntry", "source_did", "did:key:phone"⟩

private def bare (address : Address) : LiveRoute :=
  { address, collections := none, filterUnobserved := false, filter := none, observed := none }

private def reporting (address : Address) (scopes : List Scope) (form : LiveForm) : LiveRoute :=
  { bare address with filter := some (scopes.map (⟨·, form⟩)) }

private def owned (filter : List Scope := []) (collections : List String := [])
    (routes : List Address := [route]) : Desired × Applied :=
  ({ routes, collections, filter }, { routes, filter })

private def scenario (name : String) (wiring : Desired × Applied) (live : List LiveRoute) : Input :=
  { name, desired := wiring.1, live, applied := wiring.2 }

private def inputs : List Input :=
  [ scenario "scalar_report_certifies_scope" (owned [phone]) [reporting route [phone] .scalar]
  , scenario "predicate_report_certifies_scope" (owned [phone]) [reporting route [phone] .predicate]
  , scenario "scalar_report_of_another_value_repairs" (owned [phone])
      [reporting route [stranger] .scalar]
  , scenario "report_missing_a_scope_repairs" (owned [phone, directory])
      [reporting route [phone] .predicate]
  , scenario "report_with_an_extra_scope_repairs" (owned [phone])
      [reporting route [phone, directory] .predicate]
  , scenario "two_scopes_certified_in_mixed_forms" (owned [phone, directory])
      [{ bare route with filter := some [⟨directory, .scalar⟩, ⟨phone, .predicate⟩] }]
  , scenario "empty_report_repairs_scoped_route" (owned [phone]) [reporting route [] .predicate]
  , scenario "empty_report_certifies_unscoped_route" (owned) [reporting route [] .predicate]
  , scenario "unreported_filter_repairs_scoped_route" (owned [phone]) [bare route]
  , scenario "unreported_filter_keeps_unscoped_route" (owned) [bare route]
  , scenario "unobservable_filter_repairs_scoped_route" (owned [phone])
      [{ bare route with filterUnobserved := true }]
  , scenario "unobservable_filter_keeps_unscoped_route" (owned)
      [{ bare route with filterUnobserved := true }]
  , { name := "unobservable_filter_on_unowned_route_is_uncertified"
      desired := { routes := [route], collections := [], filter := [phone] }
      live := [{ bare route with filterUnobserved := true }]
      applied := { routes := [], filter := [phone] } }
  , { name := "applied_scope_change_repairs"
      desired := { routes := [route], collections := [], filter := [phone] }
      live := [reporting route [phone] .predicate]
      applied := { routes := [route], filter := [stranger] } }
  , scenario "identity_observation_keeps_dialable_route" (owned)
      [{ bare route with observed := some [identity home] }]
  , scenario "own_observation_keeps_route" (owned) [{ bare route with observed := some [route] }]
  , scenario "dialable_observation_keeps_identity_route" (owned [] [] [identity home])
      [{ bare (identity home) with observed := some [route] }]
  , scenario "stale_dial_observation_repairs" (owned)
      [{ bare route with observed := some [moved] }]
  , scenario "other_peer_observation_repairs" (owned)
      [{ bare route with observed := some [identity away] }]
  , scenario "two_observations_repair" (owned)
      [{ bare route with observed := some [route, identity home] }]
  , scenario "no_observation_repairs" (owned) [{ bare route with observed := some [] }]
  , scenario "opaque_route_keeps_only_its_own_text" (owned [] [] [manual])
      [{ bare manual with observed := some [manual] }]
  , scenario "opaque_route_repairs_on_any_other_text" (owned [] [] [manual])
      [{ bare manual with observed := some [plain "elsewhere"] }]
  , scenario "carried_collections_match" (owned [] ["Network", "AgentRequest"])
      [{ bare route with collections := some ["AgentRequest", "Network"] }]
  , scenario "carried_collections_shrunk_repairs" (owned [] ["Network", "AgentRequest"])
      [{ bare route with collections := some ["AgentRequest"] }]
  , scenario "carried_collections_grown_repairs" (owned [] ["Network"])
      [{ bare route with collections := some ["AgentRequest", "Network"] }]
  , scenario "unreported_collections_keep_route" (owned [] ["Network"]) [bare route]
  , { name := "only_applied_extras_are_torn_down"
      desired := { routes := [], collections := [], filter := [] }
      live := [bare manual, bare route]
      applied := { routes := [route], filter := [] } }
  , { name := "applied_extra_absent_from_live_is_torn_down"
      desired := { routes := [], collections := [], filter := [] }
      live := []
      applied := { routes := [route], filter := [] } }
  , { name := "unowned_live_route_is_not_repaired"
      desired := { routes := [route], collections := ["Network"], filter := [] }
      live := [{ bare route with collections := some [] }]
      applied := { routes := [], filter := [] } }
  , scenario "missing_route_is_installed" (owned) []
  , { name := "extras_then_repairs_then_installs"
      desired := { routes := [route, other], collections := ["Network"], filter := [] }
      live := [{ bare route with collections := some [] }, bare manual]
      applied := { routes := [manual, route], filter := [] } }
  , { name := "every_drifted_route_is_repaired_in_address_order"
      desired := { routes := [other, route], collections := [], filter := [phone] }
      live := [bare other, bare route]
      applied := { routes := [other, route], filter := [phone] } }
  , { name := "nothing_desired_nothing_applied"
      desired := { routes := [], collections := [], filter := [] }
      live := [bare manual]
      applied := { routes := [], filter := [] } } ]

@[conformance_family]
def cases : Family Input Output where
  inputs := inputs
  model := fun i =>
    { ops := ownedDiff i.desired i.live i.applied
      liveMatches := liveMatches i.desired i.live i.applied }

end Conformance.PairingOwnedDiff
