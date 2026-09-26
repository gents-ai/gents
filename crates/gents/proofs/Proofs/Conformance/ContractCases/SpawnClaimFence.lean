import Proofs.SpawnClaimFence

namespace Conformance.ContractCases

open SpawnClaimFence

def routeString : Route → String
  | .samePrincipal => "same_principal"
  | .crossPrincipal => "cross_principal"

def fenceAwaitModeString : AwaitMode → String
  | .foreground => "foreground"
  | .background => "background"

def bridgeString : Bridge → String
  | .awaiting => "awaiting"
  | .linked => "linked"
  | .abandoned => "abandoned"
  | .expired => "expired"
  | .settledObserved => "settled_observed"

def childString : Child → String
  | .absent => "absent"
  | .pending => "pending"
  | .running => "running"
  | .interrupted => "interrupted"
  | .finished => "finished"

def actionString : Action → String
  | .expire => "expire"
  | .deadline => "deadline"
  | .materialize => "materialize"
  | .publishChild => "publish_child"
  | .replicateBridge => "replicate_bridge"
  | .mirror => "mirror"
  | .claim => "claim"
  | .stop => "stop"
  | .observeAck => "observe_ack"

/-- One node shows each side the other's current rows, so a step replays there
    only when the acting side's view agrees with the durable facts: the
    parent's expiries see exactly the child rows that exist, and the host's
    gates see exactly the bridge's intent. A host materializing on a stale view
    is replayed through the child creation owner it calls. -/
def singleNodeFaithful (w : World) : Action → Bool
  | .expire | .deadline => w.childVisible == (w.child != .absent)
  | .claim | .mirror => w.hostSeesIntent == w.cancelIntent
  | .materialize | .publishChild | .replicateBridge | .stop | .observeAck => true

/-- One observable step of a fence trace: the action, whether the route
    enabled it, whether the host acted on a stale view of the bridge, and the
    world after it. -/
structure SpawnFenceStep where
  action : String
  enabled : Bool
  staleHostView : Bool
  bridge : String
  child : String
  cancelIntent : Bool
  hostSeesIntent : Bool
  interruptLatched : Bool
  ackPending : Bool
  deriving Repr

structure SpawnFenceCase where
  name : String
  route : String
  awaitMode : String
  unclaimedDeadlineSet : Bool
  singleNodeReplayable : Bool
  steps : List SpawnFenceStep
  deriving Repr

def traceSteps (route : Route) (mode : AwaitMode) : World → List Action → List SpawnFenceStep
  | _, [] => []
  | w, action :: rest =>
      let on := enabled route mode action
      let next := if on then step w action else w
      { action := actionString action, enabled := on
      , staleHostView := action == .materialize && w.cancelIntent && !w.hostSeesIntent
      , bridge := bridgeString next.bridge, child := childString next.child
      , cancelIntent := next.cancelIntent, hostSeesIntent := next.hostSeesIntent
      , interruptLatched := next.interruptLatched, ackPending := next.ackPending }
        :: traceSteps route mode next rest

def traceFaithful (route : Route) (mode : AwaitMode) : World → List Action → Bool
  | _, [] => true
  | w, action :: rest =>
      let on := enabled route mode action
      (!on || singleNodeFaithful w action) &&
        traceFaithful route mode (if on then step w action else w) rest

def spawnFenceCase (name : String) (route : Route) (actions : List Action)
    (mode : AwaitMode := .background) : SpawnFenceCase :=
  { name, route := routeString route, awaitMode := fenceAwaitModeString mode
  , unclaimedDeadlineSet := unclaimedDeadlineApplies route mode
  , singleNodeReplayable := traceFaithful route mode World.initial actions
  , steps := traceSteps route mode World.initial actions }

def spawnFenceCases : List SpawnFenceCase :=
  [ spawnFenceCase "local_late_claim_runs_attached" .samePrincipal
      [.expire, .materialize, .publishChild, .claim]
  , spawnFenceCase "cross_expiry_refuses_materialization" .crossPrincipal
      [.expire, .replicateBridge, .materialize]
  , spawnFenceCase "cross_expiry_then_late_claim_refused" .crossPrincipal
      [.expire, .materialize, .publishChild, .replicateBridge, .claim, .observeAck]
  , spawnFenceCase "cross_mirror_latches_stale_child" .crossPrincipal
      [.expire, .materialize, .publishChild, .replicateBridge, .mirror, .observeAck,
        .claim, .observeAck]
  , spawnFenceCase "cross_late_claim_won_race_stays_unsettled" .crossPrincipal
      [.expire, .materialize, .claim, .replicateBridge, .mirror, .observeAck, .stop,
        .observeAck]
  , spawnFenceCase "cross_claim_then_expiry_links_child" .crossPrincipal
      [.materialize, .publishChild, .claim, .expire]
  , spawnFenceCase "cross_repeated_expiry_is_idempotent" .crossPrincipal
      [.expire, .replicateBridge, .expire]
  , spawnFenceCase "cross_deadline_first_fences_late_claim" .crossPrincipal
      [.deadline, .materialize, .publishChild, .replicateBridge, .claim, .observeAck]
  , spawnFenceCase "cross_both_expired_at_restart" .crossPrincipal
      [.deadline, .expire, .replicateBridge, .materialize]
  , spawnFenceCase "local_deadline_fences_late_child" .samePrincipal
      [.deadline, .materialize, .publishChild, .replicateBridge, .claim, .observeAck]
  , spawnFenceCase "local_foreground_unconfirmed_child_released" .samePrincipal
      [.expire, .replicateBridge, .materialize] (mode := .foreground)
  , spawnFenceCase "local_foreground_confirmed_child_links" .samePrincipal
      [.materialize, .publishChild, .claim, .expire] (mode := .foreground)
  ]

/-- Only the race whose claim beats the intent's replication needs two nodes;
    every other trace replays natively. -/
theorem spawnFenceCases_replayable :
    (spawnFenceCases.filter (·.singleNodeReplayable)).map (·.name) =
      (spawnFenceCases.map (·.name)).filter
        (· != "cross_late_claim_won_race_stays_unsettled") := by
  native_decide

end Conformance.ContractCases
