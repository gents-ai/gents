import Proofs.Tools.ToolPolicy.WorkspacePermissions
import Proofs.Tools.ToolPolicy.OperatorPaths
import Proofs.Conformance.Family

namespace Conformance.WorkspacePermissions

open Lean ToolPolicy.WorkspacePermissions

deriving instance ToJson for Mode, Route, Review, Scope, Grant, Input, Preference, WriteResult

structure RoutingInput where
  input : Input
  review : Review
  reviewedGrant : Grant
  deriving ToJson

structure RoutingOutput where
  validGrant : Bool
  route : Route
  reviewedRoute : Route
  deriving ToJson

def RoutingInput.ofCase (c : Case) : RoutingInput :=
  { input := c.input, review := c.review, reviewedGrant := c.reviewedGrant }

@[conformance_family]
def routing : Family RoutingInput RoutingOutput where
  inputs := cases.map RoutingInput.ofCase
  model := fun i =>
    { validGrant := validGrant i.input
      route := route i.input
      reviewedRoute := reviewedRoute i.input i.review i.reviewedGrant }

/-- The background-command scenario configures one workspace, a crossing
command without a stored grant, and a reviewer that either answers for the
exact action or is absent; a revoked review is the exact action at a stale
revision, because the scenario changes the mode while the review runs. -/
def backgroundCases : List RoutingInput :=
  (cases.filter fun c =>
    c.input.grant.isNone && c.input.ceilingAllows && c.input.crossesBoundary &&
    c.input.configuredWorkspace == c.input.current.workspace &&
    (c.input.mode != .autoReview ||
      (c.reviewedGrant.scope.workspace == c.input.current.workspace &&
        c.reviewedGrant.action == c.input.action &&
        (c.review == .allow ||
          (c.review == .unavailable &&
            c.reviewedGrant.scope.revision == c.input.current.revision))))).map
    RoutingInput.ofCase

def backgroundAllowed (sandboxEnforced : Bool) (i : RoutingInput) : Bool :=
  reviewedRoute i.input i.review i.reviewedGrant == .allow &&
    ToolPolicy.OperatorPaths.commandGuardAllows true sandboxEnforced true true

/-- A host that enforces the workspace-write sandbox (macOS `sandbox-exec`). -/
@[conformance_family]
def sandboxedBackground : Family RoutingInput Bool where
  inputs := backgroundCases
  model := backgroundAllowed true

/-- A host without the workspace-write sandbox: the command guard refuses
every required command. -/
@[conformance_family]
def unsandboxedBackground : Family RoutingInput Bool where
  inputs := backgroundCases
  model := backgroundAllowed false

structure SelectionInput where
  previous : Option Preference
  expected : Nat
  mode : Mode
  requestedTask : Option Nat
  deriving ToJson

def SelectionInput.ofCase (c : WriteCase) : SelectionInput :=
  { previous := c.previous, expected := c.expected, mode := c.mode, requestedTask := c.requestedTask }

@[conformance_family]
def selection : Family SelectionInput WriteResult where
  inputs := writeCases.map SelectionInput.ofCase
  model := fun i => selectMode i.previous i.expected i.mode i.requestedTask

@[conformance_family]
def persistedSelection : Family SelectionInput WriteResult where
  inputs := writeCases.map SelectionInput.ofCase
  model := fun i => selectMode i.previous i.expected i.mode i.requestedTask

end Conformance.WorkspacePermissions
