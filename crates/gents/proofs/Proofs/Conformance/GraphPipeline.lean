import Proofs.GraphPipeline
import Proofs.GraphPipeline.Configuration
import Proofs.Conformance.ContractTypes

namespace Conformance.GraphPipelineContracts

open Conformance.Contracts

def boolValues : List Bool := [false, true]

def boolJson (value : Bool) : String :=
  if value then "true" else "false"

/-- The one revision every family starts from: whole-graph valid, so a case
varies only the inputs it names. -/
def fixtureRevision
    (status : GraphPipeline.RevisionStatus)
    (artifactsComplete : Bool := true) : GraphPipeline.Revision :=
  { graphId := 1
  , revisionId := 2
  , digest := 3
  , status := status
  , typesValid := true
  , topologyValid := true
  , capabilitiesAuthorized := true
  , withinBounds := true
  , terminalResultDeclared := true
  , artifactsComplete := artifactsComplete
  }

def transitionAllowed
    (state : GraphPipeline.State)
    (action : GraphPipeline.Action) : Bool :=
  (GraphPipeline.step? state action).isSome

/-- Which native compiler diagnostic realizes `topologyValid = false`. The
model keeps topology opaque (`GraphPipeline.Revision.topologyValid`);
acyclicity is decided by the native Kahn pass in `compile_graph`. Both faults
project to `topologyValid = false`; the selector only tells the consumer which
intent to build, so a rejection is checked through that diagnostic. -/
inductive TopologyFault where
  | valid
  | missingInputBinding
  | cycle
  deriving DecidableEq, Repr

def TopologyFault.topologyValid : TopologyFault → Bool
  | .valid => true
  | .missingInputBinding | .cycle => false

def TopologyFault.wireName : TopologyFault → String
  | .valid => "valid"
  | .missingInputBinding => "missing_input_binding"
  | .cycle => "cycle"

def topologyFaults : List TopologyFault := [.valid, .missingInputBinding, .cycle]

/-- Which native compiler diagnostic realizes `withinBounds = false`: an
intent that exceeds its own node limit (`node_limit_exceeded`), or one that
asks for more invocations than the platform ceiling
(`platform_limit_exceeded`). The model keeps `withinBounds` opaque and orders
no graph limits; the native compiler compares them. -/
inductive BoundsFault where
  | within
  | nodeLimit
  | invocationCeiling
  deriving DecidableEq, Repr

def BoundsFault.withinBounds : BoundsFault → Bool
  | .within => true
  | .nodeLimit | .invocationCeiling => false

def BoundsFault.wireName : BoundsFault → String
  | .within => "within"
  | .nodeLimit => "node_limit"
  | .invocationCeiling => "invocation_ceiling"

def boundsFaults : List BoundsFault := [.within, .nodeLimit, .invocationCeiling]

structure ValidationCase where
  name : String
  typesValid : Bool
  topology : TopologyFault
  capabilitiesAuthorized : Bool
  bounds : BoundsFault
  terminalResultDeclared : Bool
  deriving DecidableEq, Repr

def ValidationCase.revision (c : ValidationCase) : GraphPipeline.Revision :=
  { fixtureRevision .draft false with
    typesValid := c.typesValid,
    topologyValid := c.topology.topologyValid,
    capabilitiesAuthorized := c.capabilitiesAuthorized,
    withinBounds := c.bounds.withinBounds,
    terminalResultDeclared := c.terminalResultDeclared }

/-- The compile gate is the `validate` transition of a fresh proposal. -/
def ValidationCase.expectedValid (c : ValidationCase) : Bool :=
  transitionAllowed (GraphPipeline.initial c.revision) .validate

def validationCases : List ValidationCase :=
  boolValues.flatMap fun typesValid =>
    topologyFaults.flatMap fun topology =>
      boolValues.flatMap fun capabilitiesAuthorized =>
        boundsFaults.flatMap fun bounds =>
          boolValues.map fun terminalResultDeclared =>
            { name :=
                "types=" ++ toString typesValid ++
                ",topology=" ++ topology.wireName ++
                ",authorized=" ++ toString capabilitiesAuthorized ++
                ",bounds=" ++ bounds.wireName ++
                ",terminal_result=" ++ toString terminalResultDeclared
            , typesValid := typesValid
            , topology := topology
            , capabilitiesAuthorized := capabilitiesAuthorized
            , bounds := bounds
            , terminalResultDeclared := terminalResultDeclared
            }

theorem validationCases_count : validationCases.length = 72 := by native_decide

/-- The executable gate agrees with the Prop-level whole-graph predicate. -/
theorem validationCases_agree_with_wholeGraphValid :
    validationCases.all
      (fun c => c.expectedValid == decide c.revision.wholeGraphValid) = true := by
  native_decide

/-- A cyclic topology never validates, whatever the other inputs say. -/
theorem cycle_never_validates :
    validationCases.all (fun c => c.topology != .cycle || !c.expectedValid) = true := by
  native_decide

theorem every_topology_and_bounds_fault_is_covered :
    (topologyFaults.all (fun f => validationCases.any (·.topology == f)) &&
      boundsFaults.all (fun f => validationCases.any (·.bounds == f))) = true := by
  native_decide

def validationCaseJson (testCase : ValidationCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString testCase.name ++ ","
    ++ "\"types_valid\":" ++ boolJson testCase.typesValid ++ ","
    ++ "\"topology_fault\":" ++ jsonString testCase.topology.wireName ++ ","
    ++ "\"capabilities_authorized\":" ++ boolJson testCase.capabilitiesAuthorized ++ ","
    ++ "\"bounds_fault\":" ++ jsonString testCase.bounds.wireName ++ ","
    ++ "\"terminal_result_declared\":" ++
      boolJson testCase.terminalResultDeclared ++ ","
    ++ "\"expected_valid\":" ++ boolJson testCase.expectedValid
    ++ "}"

def validationCasesJson : String :=
  jsonArray (validationCases.map validationCaseJson)

/-- Where `GraphDefinition.active_revision_digest` points relative to the
candidate revision. `GraphPipeline.State.activeRevision` is one `Option`, so
an empty slot (the activation precondition) and a pointer at this revision
(the start precondition) exclude each other. `activate` requires an empty
slot; the native activation's replacement of a predecessor is not covered by
this family. -/
inductive ActivePointer where
  | empty
  | current
  | other
  deriving DecidableEq, Repr

def ActivePointer.wireName : ActivePointer → String
  | .empty => "empty"
  | .current => "current"
  | .other => "other"

def ActivePointer.target : ActivePointer → Option GraphPipeline.RevisionId
  | .empty => none
  | .current => some 2
  | .other => some 9

def activePointers : List ActivePointer := [.empty, .current, .other]

def revisionStatuses : List (GraphPipeline.RevisionStatus × String) :=
  [ (.draft, "draft")
  , (.validated, "validated")
  , (.active, "active")
  , (.retired, "retired")
  ]

structure RevisionGateCase where
  name : String
  status : GraphPipeline.RevisionStatus
  statusName : String
  artifactsComplete : Bool
  pointer : ActivePointer
  deriving DecidableEq, Repr

def gateState (c : RevisionGateCase) : GraphPipeline.State :=
  { revision := fixtureRevision c.status c.artifactsComplete
  , activeRevision := c.pointer.target
  , run := none
  }

def RevisionGateCase.expectedActivate (c : RevisionGateCase) : Bool :=
  transitionAllowed (gateState c) .activate

def RevisionGateCase.expectedStart (c : RevisionGateCase) : Bool :=
  transitionAllowed (gateState c) (.startRun 4 true)

/-- The native gate's two inputs, projected from the one pointer so the
adapter translates representation and decides nothing. -/
def RevisionGateCase.activationPreconditionMet (c : RevisionGateCase) : Bool :=
  c.pointer == .empty

def RevisionGateCase.pointerMatches (c : RevisionGateCase) : Bool :=
  c.pointer == .current

def revisionGateCases : List RevisionGateCase :=
  revisionStatuses.flatMap fun (status, statusName) =>
    boolValues.flatMap fun artifactsComplete =>
      activePointers.map fun pointer =>
        { name :=
            "status=" ++ statusName ++
              ",complete=" ++ toString artifactsComplete ++
              ",pointer=" ++ pointer.wireName
        , status := status
        , statusName := statusName
        , artifactsComplete := artifactsComplete
        , pointer := pointer
        }

theorem revisionGateCases_count : revisionGateCases.length = 24 := by native_decide

/-- Activation needs a validated, complete revision and an empty slot; a start
needs an active, complete revision that the pointer selects. -/
theorem revisionGate_agrees_with_ready_predicate :
    revisionGateCases.all (fun c =>
      c.expectedActivate ==
          (c.status == .validated && c.artifactsComplete && c.pointer == .empty) &&
        c.expectedStart ==
          (c.status == .active && c.artifactsComplete && c.pointer == .current)) = true := by
  native_decide

def revisionGateCaseJson (testCase : RevisionGateCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString testCase.name ++ ","
    ++ "\"status\":" ++ jsonString testCase.statusName ++ ","
    ++ "\"artifacts_complete\":" ++ boolJson testCase.artifactsComplete ++ ","
    ++ "\"active_pointer\":" ++ jsonString testCase.pointer.wireName ++ ","
    ++ "\"activation_precondition_met\":" ++
      boolJson testCase.activationPreconditionMet ++ ","
    ++ "\"pointer_matches\":" ++ boolJson testCase.pointerMatches ++ ","
    ++ "\"expected_activate\":" ++ boolJson testCase.expectedActivate ++ ","
    ++ "\"expected_start\":" ++ boolJson testCase.expectedStart
    ++ "}"

def revisionGateCasesJson : String :=
  jsonArray (revisionGateCases.map revisionGateCaseJson)

structure RunTerminalCase where
  name : String
  status : String
  cancellationRequested : Bool
  resultContractSatisfied : Bool
  activeWorkTerminal : Bool
  failureProven : Bool
  expectedSucceed : Bool
  expectedFail : Bool
  expectedCancel : Bool
  deriving DecidableEq, Repr

def runStatuses : List (GraphPipeline.RunStatus × String) :=
  [ (.running, "running")
  , (.succeeded, "succeeded")
  , (.failed, "failed")
  , (.cancelled, "cancelled")
  ]

def runState
    (status : GraphPipeline.RunStatus)
    (cancellationRequested : Bool) : GraphPipeline.State :=
  { revision := fixtureRevision .active
  , activeRevision := some 2
  , run := some
      { runId := 4
      , graphId := 1
      , revisionId := 2
      , revisionDigest := 3
      , status := status
      , seedCommitted := true
      , cancellationRequested := cancellationRequested
      , resultsCommitted := false
      }
  }

def runTerminalCases : List RunTerminalCase :=
  runStatuses.flatMap fun (status, statusName) =>
    boolValues.flatMap fun cancellationRequested =>
      boolValues.flatMap fun resultContractSatisfied =>
      boolValues.flatMap fun activeWorkTerminal =>
        boolValues.map fun failureProven =>
            let state := runState status cancellationRequested
            { name :=
                "status=" ++ statusName ++
                  ",cancel_requested=" ++ toString cancellationRequested ++
                  ",results_satisfied=" ++ toString resultContractSatisfied ++
                  ",work_terminal=" ++ toString activeWorkTerminal ++
                  ",failure_proven=" ++ toString failureProven
            , status := statusName
            , cancellationRequested := cancellationRequested
            , resultContractSatisfied := resultContractSatisfied
            , activeWorkTerminal := activeWorkTerminal
            , failureProven := failureProven
            , expectedSucceed :=
                transitionAllowed state
                  (.succeedRun resultContractSatisfied activeWorkTerminal)
            , expectedFail :=
                transitionAllowed state (.failRun failureProven activeWorkTerminal)
            , expectedCancel := transitionAllowed state (.cancelRun activeWorkTerminal)
            }

theorem runTerminalCases_count : runTerminalCases.length = 64 := by native_decide

def runTerminalCaseJson (testCase : RunTerminalCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString testCase.name ++ ","
    ++ "\"status\":" ++ jsonString testCase.status ++ ","
    ++ "\"cancellation_requested\":" ++
      boolJson testCase.cancellationRequested ++ ","
    ++ "\"result_contract_satisfied\":" ++
      boolJson testCase.resultContractSatisfied ++ ","
    ++ "\"active_work_terminal\":" ++ boolJson testCase.activeWorkTerminal ++ ","
    ++ "\"failure_proven\":" ++ boolJson testCase.failureProven ++ ","
    ++ "\"expected_succeed\":" ++ boolJson testCase.expectedSucceed ++ ","
    ++ "\"expected_fail\":" ++ boolJson testCase.expectedFail ++ ","
    ++ "\"expected_cancel\":" ++ boolJson testCase.expectedCancel
    ++ "}"

def runTerminalCasesJson : String :=
  jsonArray (runTerminalCases.map runTerminalCaseJson)

/-- Delivery shapes on both sides of the shared group validator, kept inside
what the native compiler also enforces: timeouts at most 86400 seconds,
source fields that are GraphQL names, and the 256-document bound shared by
`Triggers.Groups.maxGroupDocs` and `CompilerPolicy.max_group_size`. -/
def edgeDeliveryShapes : List (String × Option Triggers.Groups.GroupConfig) :=
  [ ("ungrouped", none)
  , ("fixed_1", some { expected := some (.fixed 1) })
  , ("fixed_2", some { expected := some (.fixed 2) })
  , ("fixed_256", some { expected := some (.fixed 256) })
  , ("fixed_257", some { expected := some (.fixed 257) })
  , ("source_field", some { expected := some (.sourceField "expected_total") })
  , ("timeout_only", some { timeoutSecs := some 60 })
  , ("fixed_3_minimum_4", some { expected := some (.fixed 3), minimumCount := some 4 })
  , ("fixed_3_timeout_0", some { expected := some (.fixed 3), timeoutSecs := some 0 })
  , ("fixed_3_timeout_86400_minimum_2",
      some { expected := some (.fixed 3), timeoutSecs := some 86400, minimumCount := some 2 })
  , ("fixed_3_minimum_0", some { expected := some (.fixed 3), minimumCount := some 0 })
  ]

def concurrencyModes : List (ConcurrencyMode × String) :=
  [ (.parallel, "parallel")
  , (.serial, "serial")
  , (.queuedSerial, "queued_serial")
  , (.latestOnly, "latest_only")
  ]

/-- The correlation field every port of the native edge fixture uses. -/
def edgeCorrelation : String := "graph_run_id"

structure EdgeDeliveryCase where
  name : String
  mode : ConcurrencyMode
  modeName : String
  delivery : Option Triggers.Groups.GroupConfig
  deriving DecidableEq, Repr

def EdgeDeliveryCase.expectedConcurrencyValid (c : EdgeDeliveryCase) : Bool :=
  GraphPipeline.graphEdgeConcurrencyValid c.mode

def EdgeDeliveryCase.expectedValid (c : EdgeDeliveryCase) : Bool :=
  GraphPipeline.graphEdgeValid c.delivery edgeCorrelation c.mode

def edgeDeliveryCases : List EdgeDeliveryCase :=
  concurrencyModes.flatMap fun (mode, modeName) =>
    edgeDeliveryShapes.map fun (shape, delivery) =>
      { name := "concurrency=" ++ modeName ++ ",delivery=" ++ shape
      , mode := mode
      , modeName := modeName
      , delivery := delivery
      }

theorem edgeDeliveryCases_count : edgeDeliveryCases.length = 44 := by native_decide

theorem edgeDeliveryCases_valid_count :
    (edgeDeliveryCases.filter (·.expectedValid)).length = 15 := by native_decide

theorem edgeDeliveryCases_latest_only_never_valid :
    edgeDeliveryCases.all (fun c => c.mode != .latestOnly || !c.expectedValid) = true := by
  native_decide

theorem edgeDeliveryCases_cover_both_verdicts :
    (edgeDeliveryCases.any (fun c => c.expectedValid) &&
      edgeDeliveryCases.any (fun c => !c.expectedValid) &&
      edgeDeliveryCases.any (fun c => !c.expectedConcurrencyValid)) = true := by
  native_decide

/-- The `EventGroup` wire shape (`expected_count`, `timeout_secs`, `min_count`). -/
def groupConfigJson (g : Triggers.Groups.GroupConfig) : String :=
  let expected : List String := match g.expected with
    | none => []
    | some (.fixed count) => ["\"expected_count\":" ++ toString count]
    | some (.sourceField field) =>
        ["\"expected_count\":{\"source_field\":" ++ jsonString field ++ "}"]
  let timeout : List String := match g.timeoutSecs with
    | none => []
    | some seconds => ["\"timeout_secs\":" ++ toString seconds]
  let minimum : List String := match g.minimumCount with
    | none => []
    | some count => ["\"min_count\":" ++ toString count]
  "{" ++ ",".intercalate (expected ++ timeout ++ minimum) ++ "}"

def edgeDeliveryCaseJson (c : EdgeDeliveryCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"concurrency\":" ++ jsonString c.modeName ++ ","
    ++ "\"delivery\":" ++ (match c.delivery with
        | none => "null"
        | some g => groupConfigJson g) ++ ","
    ++ "\"expected_concurrency_valid\":" ++ boolJson c.expectedConcurrencyValid ++ ","
    ++ "\"expected_valid\":" ++ boolJson c.expectedValid
    ++ "}"

def edgeDeliveryCasesJson : String :=
  jsonArray (edgeDeliveryCases.map edgeDeliveryCaseJson)

end Conformance.GraphPipelineContracts
