import Proofs.Inference.AccountResolver
import Proofs.Conformance.Family

namespace AccountResolver

deriving instance Lean.ToJson for Account

structure Input where
  name : String
  owner : String
  provider : String
  pickDefault : Bool
  accountRef : Option String
  rows : List Account
  deriving Lean.ToJson

structure Resolution where
  pool : List String
  selected : Option String
  deriving Lean.ToJson

private def node : String := "did:key:zResolver"

private def foreignNode : String := "did:key:zOther"

private def xai : String := "xai-oauth"

private def codex : String := "chatgpt-codex"

private def account (owner provider credentialId : String) (accountRef : Option String)
    (connectedAt : Option Nat) (enabled : Bool) : Account :=
  { owner, provider, credentialId, accountRef, connectedAt, enabled }

private def scenario (name owner provider : String) (pickDefault : Bool)
    (accountRef : Option String) (rows : List Account) : Input :=
  { name, owner, provider, pickDefault, accountRef, rows }

private def referenceRows : List Account :=
  [ account node xai "earlier" (some "earlier") (some 1000) true
  , account node xai "original" none (some 2000) true
  , account node xai "b" (some "b") (some 3000) true ]

private def cases : List Input :=
  [ scenario "pool_holds_every_enabled_account_in_resolver_order" node xai true none
      [ account node xai "late" (some "late") (some 2000) true
      , account node xai "off" (some "off") (some 500) false
      , account foreignNode xai "foreign" (some "f") (some 100) true
      , account node codex "other-provider" none (some 100) true
      , account node xai "original" none none true
      , account node xai "early" (some "early") (some 1000) true ]
  , scenario "pool_is_empty_without_an_enabled_account" node xai true none
      [ account node xai "off" none none false ]
  , scenario "picks_the_earliest_connected_enabled_account" node xai true none
      [ account node xai "late" (some "late") (some 2000) true
      , account node xai "early" (some "early") (some 1000) true ]
  , scenario "skips_a_disabled_account" node xai true none
      [ account node xai "off" (some "off") (some 500) false
      , account node xai "on" (some "on") (some 1000) true ]
  , scenario "sorts_a_missing_connection_time_first" node xai true none
      [ account node xai "timed" (some "timed") (some 1000) true
      , account node xai "untimed" none none true ]
  , scenario "breaks_equal_times_by_credential_id" node xai true none
      [ account node xai "b" (some "b") (some 1000) true
      , account node xai "a" (some "a") (some 1000) true ]
  , scenario "never_returns_another_node_or_provider" node xai true none
      [ account foreignNode xai "other-did" none none true
      , account node codex "other-provider" none none true
      , account node xai "mine" none (some 1000) true ]
  , scenario "a_foreign_reference_never_resolves" node xai false (some "b")
      [ account foreignNode xai "b" (some "b") none true ]
  , scenario "returns_nothing_when_no_account_is_enabled" node xai true none
      [ account node xai "off" none none false ]
  , scenario "an_unset_reference_returns_nothing_without_an_enabled_account"
      node xai false none
      [ account node xai "off" none none false ]
  , scenario "reference_names_exactly_one_account" node xai false none referenceRows
  , scenario "a_reference_names_its_account" node xai false (some "b") referenceRows
  , scenario "the_provider_default_ignores_references" node xai true none referenceRows
  , scenario "a_disabled_reference_does_not_resolve" node xai false (some "b")
      [ account node xai "original" none none true
      , account node xai "b" (some "b") (some 1000) false ]
  , scenario "an_empty_pool_selects_nothing" node xai true none []
  , scenario "an_unset_reference_matches_only_unreferenced_rows" node xai false none
      [ account node xai "referenced" (some "x") (some 100) true
      , account node xai "unreferenced" none (some 200) true ]
  , scenario "pool_breaks_equal_times_by_credential_id" node xai true none
      [ account node xai "b" (some "b") (some 1000) true
      , account node xai "a" (some "a") (some 1000) true ]
  , scenario "a_disabled_row_never_joins_the_pool" node xai true none
      [ account node xai "off" (some "off") (some 500) false
      , account node xai "on" (some "on") (some 1000) true ]
  ]

@[conformance_family]
def order : Conformance.Family Input Resolution where
  inputs := cases
  model := fun i =>
    { pool := (pool i.owner i.provider i.rows).map (·.credentialId),
      selected :=
        (selected i.owner i.provider i.pickDefault i.accountRef i.rows).map
          (·.credentialId) }

end AccountResolver
