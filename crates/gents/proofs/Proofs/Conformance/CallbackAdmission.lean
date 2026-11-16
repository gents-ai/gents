import Proofs.Automation.EventDelivery.Durable
import Proofs.Conformance.Family

namespace Conformance.CallbackAdmission

open Lean EventDelivery.Durable

deriving instance ToJson for CallbackStep, CallbackObservation

structure TraceInput where
  steps : List CallbackStep
  deriving ToJson

@[conformance_family]
def trace : Family TraceInput (List CallbackObservation) where
  inputs :=
    [ [.admit, .edit, .restart]
    , [.disable, .admit]
    , [.restart]
    , [.admit, .restart]
    , [.edit, .restart]
    , [.disable, .restart]
    , [.admit, .disable, .restart]
    , [.restart, .edit, .restart]
    , [.admit, .admit]
    ].map fun steps => { steps }
  model := fun i => callbackRun {} i.steps

end Conformance.CallbackAdmission

namespace Conformance.TriggerCursor

open Lean EventDelivery.Durable

deriving instance ToJson for TriggerStep, TriggerAdmission, TriggerObservation

structure TraceInput where
  steps : List TriggerStep
  deriving ToJson

@[conformance_family]
def trace : Family TraceInput (List TriggerObservation) where
  inputs :=
    [ [.disable, .admit, .checkpoint, .enable, .admit, .disable, .admit, .checkpoint]
    , [.checkpoint]
    , [.admit, .checkpoint]
    , [.admit, .admit]
    , [.disable, .checkpoint]
    , [.disable, .admit, .enable, .admit, .checkpoint]
    ].map fun steps => { steps }
  model := fun i => triggerRun {} i.steps

end Conformance.TriggerCursor
