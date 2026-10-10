import Proofs.PeerRegistryDiscovery.RootAdmission
import Proofs.Conformance.Contracts.Json.Helpers

/-! Model-generated cases for the canonical descendant-root admission bridge. -/

namespace Conformance.RootAdmissionContracts

open PeerRegistryDiscovery.RootAdmission
open Conformance.Contracts

structure Case where
  name : String
  operation : String
  authored : String
  blank : Bool
  observation : String
  configured : Bool
  ceiling : Option CanonicalPath
  enabled : List CanonicalPath
  published : List CanonicalPath
  candidate : Option CanonicalPath
  expected : Bool
  deriving DecidableEq, Repr

private def path (components : List String) : CanonicalPath := ⟨"sandbox", components⟩
private def workspace := path ["workspace"]
private def root := path ["workspace", "root"]
private def nested := path ["workspace", "root", "project"]

/-- Filesystem-shaped labels record which Rust resolver observation each
abstract canonical-path case must refine. They do not model filesystem effects. -/
def cases : List Case :=
  [ ⟨"exact_create", "create", "configured", false, "existing", true, some workspace,
      [root], [root], some root, true⟩
  , ⟨"descendant_create", "create", "configured", false, "existing", true, some workspace,
      [root], [root], some nested, true⟩
  , ⟨"whitespace_create_explicit", "create", "   ", true, "blank", true, some workspace,
      [root], [root], none, false⟩
  , ⟨"whitespace_create_unconfigured", "create", "   ", true, "blank", false, some root,
      [], [root], none, true⟩
  , ⟨"no_documents_uses_ceiling", "create", "configured", false, "existing", false, some root,
      [], [root], some root, true⟩
  , ⟨"no_policy_no_ceiling_authored_root", "create", "configured", false, "existing", false, none,
      [], [], some root, true⟩
  , ⟨"all_disabled_does_not_fallback", "create", "configured", false, "existing", true, some root,
      [], [], some root, false⟩
  , ⟨"invalid_ceiling_does_not_fallback", "create", "configured", false, "invalid_ceiling",
      true, none, [], [], some root, false⟩
  , ⟨"prefix_sibling", "create", "configured", false, "existing", true, some workspace,
      [root], [root], some (path ["workspace", "root-other"]), false⟩
  , ⟨"traversal_normalizes_inside", "create", "configured", false, "traversal", true, some workspace,
      [root], [root], some nested, true⟩
  , ⟨"traversal_normalizes_outside", "create", "configured", false, "traversal", true, some workspace,
      [root], [root], some (path ["workspace", "outside"]), false⟩
  , ⟨"symlink_target_inside", "create", "configured", false, "symlink", true, some workspace,
      [root], [root], some nested, true⟩
  , ⟨"symlink_target_outside", "create", "configured", false, "symlink", true, some workspace,
      [root], [root], some (path ["outside", "project"]), false⟩
  , ⟨"nonexistent_suffix", "create", "configured", false, "nonexistent", true, some workspace,
      [root], [root], some (path ["workspace", "root", "future", "project"]), true⟩
  , ⟨"resolution_failure", "create", "configured", false, "unresolved", true, some workspace,
      [root], [root], none, false⟩
  , ⟨"explicit_restriction_does_not_widen", "create", "configured", false,
      "explicit_restriction", true, some root, [nested], [nested],
      some (path ["workspace", "root", "other"]), false⟩
  , ⟨"one_of_multiple_published_roots", "create", "configured", false, "existing", true,
      some (path []), [root, path ["srv", "source"]], [root, path ["srv", "source"]],
      some (path ["srv", "source", "repo"]), true⟩
  , ⟨"different_anchor", "create", "configured", false, "existing", true, none,
      [root], [root], some ⟨"other-volume", ["workspace", "root"]⟩, false⟩
  ]

private def policy (c : Case) : PublicationPolicy :=
  ⟨c.configured, c.enabled.toFinset, c.ceiling⟩

def evaluates (c : Case) : Bool :=
  decide (publishedRoots (policy c) = c.published.toFinset) &&
    decide (c.blank = (c.authored.trim == "")) &&
    match c.operation with
    | "create" =>
        decide (rootSelectionOk c.configured (publishedRoots (policy c))
          (c.authored.trim == "") c.candidate)
    | _ => false

theorem cases_replay : ∀ c ∈ cases, evaluates c = c.expected := by native_decide

private def pathJson (value : CanonicalPath) : String :=
  "{\"anchor\":" ++ jsonString value.anchor ++
    ",\"components\":" ++ jsonArray (value.components.map jsonString) ++ "}"

private def candidateJson : Option CanonicalPath → String
  | none => "null"
  | some value => pathJson value

private def boolJson (value : Bool) : String := if value then "true" else "false"

private def caseJson (c : Case) : String :=
  "{\"name\":" ++ jsonString c.name ++
  ",\"operation\":" ++ jsonString c.operation ++
  ",\"authored\":" ++ jsonString c.authored ++
  ",\"blank\":" ++ boolJson c.blank ++
  ",\"observation\":" ++ jsonString c.observation ++
  ",\"configured\":" ++ boolJson c.configured ++
  ",\"ceiling\":" ++ candidateJson c.ceiling ++
  ",\"enabled\":" ++ jsonArray (c.enabled.map pathJson) ++
  ",\"published\":" ++ jsonArray (c.published.map pathJson) ++
  ",\"candidate\":" ++ candidateJson c.candidate ++
  ",\"expected\":" ++ boolJson c.expected ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

end Conformance.RootAdmissionContracts
