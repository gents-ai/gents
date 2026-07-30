import Proofs.Fleet.State

namespace FleetState

inductive Transition : FleetState → FleetState → Prop where
  | materialize_scheduled {pre post : FleetState} (wid : Nat) (bid : BackendId) :
      wid ∉ pre.activeIds →
      post.activeIds = insert wid pre.activeIds →
      post.ctx = Function.update pre.ctx wid
        { state := .claimed
        , origin := .scheduled
        , backend := bid
        , admission := .waiting
        , deadline := 0
        , claimTime := 0
        , currentTime := 0
        , retryCount := 0
        , maxRetries := 3
        , progressSeq := 0
        , messageSeq := 0
        , isLatest := true
        , persistence := .uncommitted
        } →
      post.scheduler = pre.scheduler →
      Transition pre post
  | accept_existing {pre post : FleetState} (wid : Nat) :
      wid ∉ pre.activeIds →
      (pre.ctx wid).state = .claimed →
      (pre.ctx wid).admission = .waiting →
      post.activeIds = insert wid pre.activeIds →
      post.ctx = pre.ctx →
      post.scheduler = pre.scheduler →
      Transition pre post
  | acquire_slot {pre post : FleetState} (wid : Nat) (bid : BackendId) :
      CanAcquire pre wid bid →
      post.activeIds = pre.activeIds →
      post.ctx = Function.update pre.ctx wid { pre.ctx wid with admission := .acquired } →
      post.scheduler.backends = pre.scheduler.backends →
      post.scheduler.running =
        Function.update pre.scheduler.running bid (pre.scheduler.running bid + 1) →
      Transition pre post
  | begin_execution {pre post : FleetState} (wid : Nat) :
      CanBegin pre wid →
      post.activeIds = pre.activeIds →
      post.ctx =
        Function.update pre.ctx wid { pre.ctx wid with state := .processing, admission := .executing } →
      post.scheduler = pre.scheduler →
      Transition pre post
  | release_on_terminal {pre post : FleetState} (wid : Nat) (bid : BackendId)
      (terminal : RequestState) :
      CanRelease pre wid bid terminal →
      post.activeIds = pre.activeIds →
      post.ctx = Function.update pre.ctx wid (RequestContext.releaseToTerminal (pre.ctx wid) terminal) →
      post.scheduler.backends = pre.scheduler.backends →
      post.scheduler.running =
        Function.update pre.scheduler.running bid (pre.scheduler.running bid - 1) →
      Transition pre post

inductive Trace : FleetState → FleetState → Prop where
  | refl {s : FleetState} : Trace s s
  | step {s₁ s₂ s₃ : FleetState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

end FleetState
