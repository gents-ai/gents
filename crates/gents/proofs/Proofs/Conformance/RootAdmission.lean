import Proofs.PeerRegistryDiscovery.PersonaRequest
import Proofs.Conformance.Contracts.Json.Helpers

/-! Model-generated cases for the canonical descendant-root admission bridge. -/

namespace Conformance.RootAdmissionContracts

open PeerRegistryDiscovery.RootAdmission
open PeerRegistryDiscovery.PersonaRequest
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
  storedRequiresRoot : Bool := false
  deriving DecidableEq, Repr

private def path (components : List String) : CanonicalPath := ⟨"sandbox", components⟩
private def workspace := path ["workspace"]
private def root := path ["workspace", "root"]
private def nested := path ["workspace", "root", "project"]

/-- Filesystem-shaped labels record which Rust resolver observation each
abstract canonical-path case must refine. They do not model filesystem effects. -/
def cases : List Case :=
  [ ⟨"exact_create", "create", "configured", false, "existing", true, some workspace,
      [root], [root], some root, true, false⟩
  , ⟨"descendant_create", "create", "configured", false, "existing", true, some workspace,
      [root], [root], some nested, true, false⟩
  , ⟨"descendant_edit", "edit_set", "configured", false, "existing", true, some workspace,
      [root], [root], some nested, true, false⟩
  , ⟨"whitespace_create_explicit", "create", "   ", true, "blank", true, some workspace,
      [root], [root], none, false, false⟩
  , ⟨"whitespace_edit_explicit", "edit_set", "   ", true, "blank", true, some workspace,
      [root], [root], none, false, false⟩
  , ⟨"clear_edit_explicit", "edit_clear", "", true, "clear", true, some workspace,
      [root], [root], none, false, false⟩
  , ⟨"clear_edit_unconfigured", "edit_clear", "", true, "clear", false, some root,
      [], [root], none, true, false⟩
  , ⟨"omitted_edit_preserves_admitted_stored_root", "edit_omitted", "stored", false,
      "existing", true, some workspace, [root], [root], some nested, true, true⟩
  , ⟨"omitted_edit_rejects_blank_stored_root_under_policy", "edit_omitted", "   ", true,
      "blank", true, some workspace, [root], [root], none, false, true⟩
  , ⟨"omitted_edit_rejects_revoked_stored_root", "edit_omitted", "stored", false,
      "existing", true, some workspace, [nested], [nested], some root, false, true⟩
  , ⟨"omitted_edit_allows_blank_stored_root_without_policy", "edit_omitted", "", true,
      "blank", false, some root, [], [root], none, true, true⟩
  , ⟨"omitted_edit_allows_inactive_rootless_tools", "edit_omitted", "", true,
      "inactive", true, some workspace, [root], [root], none, true, false⟩
  , ⟨"whitespace_create_unconfigured", "create", "   ", true, "blank", false, some root,
      [], [root], none, true, false⟩
  , ⟨"no_documents_uses_ceiling", "create", "configured", false, "existing", false, some root,
      [], [root], some root, true, false⟩
  , ⟨"no_policy_no_ceiling_authored_root", "create", "configured", false, "existing", false, none,
      [], [], some root, true, false⟩
  , ⟨"all_disabled_does_not_fallback", "create", "configured", false, "existing", true, some root,
      [], [], some root, false, false⟩
  , ⟨"invalid_ceiling_does_not_fallback", "create", "configured", false, "invalid_ceiling",
      true, none, [], [], some root, false, false⟩
  , ⟨"prefix_sibling", "create", "configured", false, "existing", true, some workspace,
      [root], [root], some (path ["workspace", "root-other"]), false, false⟩
  , ⟨"traversal_normalizes_inside", "create", "configured", false, "traversal", true, some workspace,
      [root], [root], some nested, true, false⟩
  , ⟨"traversal_normalizes_outside", "create", "configured", false, "traversal", true, some workspace,
      [root], [root], some (path ["workspace", "outside"]), false, false⟩
  , ⟨"symlink_target_inside", "create", "configured", false, "symlink", true, some workspace,
      [root], [root], some nested, true, false⟩
  , ⟨"symlink_target_outside", "create", "configured", false, "symlink", true, some workspace,
      [root], [root], some (path ["outside", "project"]), false, false⟩
  , ⟨"nonexistent_suffix", "create", "configured", false, "nonexistent", true, some workspace,
      [root], [root], some (path ["workspace", "root", "future", "project"]), true, false⟩
  , ⟨"resolution_failure", "create", "configured", false, "unresolved", true, some workspace,
      [root], [root], none, false, false⟩
  , ⟨"explicit_restriction_does_not_widen", "create", "configured", false,
      "explicit_restriction", true, some root, [nested], [nested],
      some (path ["workspace", "root", "other"]), false, false⟩
  , ⟨"one_of_multiple_published_roots", "create", "configured", false, "existing", true,
      some (path []), [root, path ["srv", "source"]], [root, path ["srv", "source"]],
      some (path ["srv", "source", "repo"]), true, false⟩
  , ⟨"different_anchor", "create", "configured", false, "existing", true, none,
      [root], [root], some ⟨"other-volume", ["workspace", "root"]⟩, false, false⟩
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
    | "edit_set" =>
        decide (rootEditSelectionOk c.configured (publishedRoots (policy c))
          "" none false (.set c.authored) c.candidate)
    | "edit_clear" =>
        decide (rootEditSelectionOk c.configured (publishedRoots (policy c))
          "" none false .clear c.candidate)
    | "edit_omitted" =>
        decide (rootEditSelectionOk c.configured (publishedRoots (policy c))
          c.authored c.candidate c.storedRequiresRoot .omitted none)
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
  ",\"expected\":" ++ boolJson c.expected ++
  ",\"stored_requires_root\":" ++ boolJson c.storedRequiresRoot ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

end Conformance.RootAdmissionContracts
