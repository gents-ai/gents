import Proofs.DurableLineage
import Proofs.Session.State

/-!
# Conversation continuation policy

Runtime-authored turns in an existing conversation share one vocabulary while
retaining the distinctions that matter for durability and projection.  This
module is the policy table consumed by conformance tests and mirrored by Rust.
-/

namespace ConversationContinuation

inductive Kind where
  | steering
  | backgroundCompletion
  | goal
  deriving DecidableEq, Repr

inductive ExecutionOrigin where
  | interactive
  | scheduled
  deriving DecidableEq, Repr

inductive Visibility where
  | visibleInput
  | runtimeControl
  deriving DecidableEq, Repr

inductive ParentStrategy where
  | generationOwner
  | previousRequest
  deriving DecidableEq, Repr

inductive ProviderInputStrategy where
  | promptOnce
  | historyThenControl
  | controlOnly
  deriving DecidableEq, Repr

def ExecutionOrigin.toContract : ExecutionOrigin → String
  | .interactive => "interactive"
  | .scheduled => "scheduled"

def Visibility.toContract : Visibility → String
  | .visibleInput => "visible_input"
  | .runtimeControl => "runtime_control"

def ParentStrategy.toContract : ParentStrategy → String
  | .generationOwner => "generation_owner"
  | .previousRequest => "previous_request"

def ProviderInputStrategy.toContract : ProviderInputStrategy → String
  | .promptOnce => "prompt_once"
  | .historyThenControl => "history_then_control"
  | .controlOnly => "control_only"

structure Policy where
  source : SessionQueue.QueueSource
  queuePolicy : SessionQueue.QueuePolicy
  origin : ExecutionOrigin
  inputVisibility : Option Visibility
  controlVisibility : Visibility
  parentStrategy : ParentStrategy
  requiresDurableInput : Bool
  providerInputStrategy : ProviderInputStrategy
  deriving DecidableEq, Repr

def version : Nat := 1

def Kind.policy : Kind → Policy
  | .steering =>
      { source := .steering
      , queuePolicy := .append
      , origin := .interactive
      , inputVisibility := some .visibleInput
      , controlVisibility := .runtimeControl
      , parentStrategy := .generationOwner
      , requiresDurableInput := true
      , providerInputStrategy := .promptOnce
      }
  | .backgroundCompletion =>
      { source := .backgroundCompletion
      , queuePolicy := .coalesce
      , origin := .scheduled
      , inputVisibility := some .runtimeControl
      , controlVisibility := .runtimeControl
      , parentStrategy := .generationOwner
      , requiresDurableInput := true
      , providerInputStrategy := .historyThenControl
      }
  | .goal =>
      { source := .goal
      , queuePolicy := .coalesce
      , origin := .scheduled
      , inputVisibility := none
      , controlVisibility := .runtimeControl
      , parentStrategy := .previousRequest
      , requiresDurableInput := false
      , providerInputStrategy := .controlOnly
      }

def Kind.lineage : Kind → Nat → DurableLineage.RawLineage
  | .steering, depth => DurableLineage.steeringContinuation depth
  | .backgroundCompletion, depth => DurableLineage.backgroundCompletionContinuation depth
  | .goal, _ => DurableLineage.goalContinuation

def Policy.toContract (policy : Policy) : String :=
  policy.source.toDefraDB ++ "|" ++ policy.queuePolicy.toDefraDB ++ "|" ++
    policy.origin.toContract ++ "|" ++
    (policy.inputVisibility.map Visibility.toContract).getD "none" ++ "|" ++
    policy.controlVisibility.toContract ++ "|" ++ policy.parentStrategy.toContract ++ "|" ++
    (if policy.requiresDurableInput then "durable_input" else "request_only") ++ "|" ++
    policy.providerInputStrategy.toContract

theorem every_control_prompt_is_internal (kind : Kind) :
    kind.policy.controlVisibility = .runtimeControl := by
  cases kind <;> rfl

theorem durable_input_exactly_for_message_backed_kinds (kind : Kind) :
    kind.policy.requiresDurableInput = kind.policy.inputVisibility.isSome := by
  cases kind <;> rfl

theorem every_continuation_lineage_is_admissible (kind : Kind) (depth : Nat) :
    DurableLineage.admissible (kind.lineage depth) = true := by
  cases kind
  · exact DurableLineage.steering_continuation_is_admissible depth
  · exact DurableLineage.background_completion_continuation_is_admissible depth
  · exact DurableLineage.goal_continuation_is_admissible

theorem steering_input_is_visible :
    Kind.steering.policy.inputVisibility = some .visibleInput := by
  rfl

inductive ProviderItem where
  | priorMessage
  | steeringInput
  | steeringControl
  deriving DecidableEq, Repr

def ProviderItem.toContract : ProviderItem → String
  | .priorMessage => "prior_message"
  | .steeringInput => "steering_input"
  | .steeringControl => "steering_control"

structure SequencedProviderItem where
  sequence : Nat
  item : ProviderItem
  deriving DecidableEq, Repr

def historyBefore (inputSequence : Nat) (rows : List SequencedProviderItem) :
    List ProviderItem :=
  (rows.filter fun row => row.sequence < inputSequence).map SequencedProviderItem.item

def assemblePromptOnce (inputSequence : Nat) (rows : List SequencedProviderItem)
    (currentPrompt : ProviderItem) : List ProviderItem :=
  historyBefore inputSequence rows ++ [currentPrompt]

def steeringProviderRows : List SequencedProviderItem :=
  [ { sequence := 1, item := .priorMessage }
  , { sequence := 2, item := .steeringInput }
  ]

def steeringProviderInput : List ProviderItem :=
  assemblePromptOnce 2 steeringProviderRows .steeringInput

def providerItemCount (needle : ProviderItem) (items : List ProviderItem) : Nat :=
  (items.filter (· = needle)).length

def steeringProviderInputContract : String :=
  String.intercalate "|" (steeringProviderInput.map ProviderItem.toContract)

theorem steering_visible_input_appears_exactly_once_at_provider_boundary :
    providerItemCount .steeringInput steeringProviderInput = 1 := by
  native_decide

theorem steering_control_prompt_is_absent_at_provider_boundary :
    providerItemCount .steeringControl steeringProviderInput = 0 := by
  native_decide

theorem background_input_is_internal :
    Kind.backgroundCompletion.policy.inputVisibility = some .runtimeControl := by
  rfl

theorem goal_has_no_separate_input :
    Kind.goal.policy.inputVisibility = none := by
  rfl

end ConversationContinuation
