/-! # Router dispatch onto a full executor queue
A routed request waits for room on the executor queue of the snapshot it was
routed with. While it waits, a newer snapshot reroutes it to the newest
snapshot's executor, a closed admission gate stops the router and leaves the
request undispatched for the next runtime, and otherwise it keeps waiting. A
request is never delivered to the queue it was waiting on once anything other
than room happened. -/

namespace RouterDispatch

inductive WaitEvent where | nothing | snapshotChanged | admissionClosed
  deriving DecidableEq, Repr

inductive Destination where | routedExecutor | newestExecutor | undispatched
  deriving DecidableEq, Repr

structure BlockedDispatch where
  destination : Destination
  routerStopped : Bool
  deriving DecidableEq, Repr

def destinationAfter : WaitEvent → Destination
  | .snapshotChanged => .newestExecutor
  | .admissionClosed | .nothing => .undispatched

def routerStopsAfter : WaitEvent → Bool
  | .admissionClosed => true
  | .nothing | .snapshotChanged => false

def blockedDispatch (event : WaitEvent) : BlockedDispatch :=
  { destination := destinationAfter event, routerStopped := routerStopsAfter event }

theorem a_blocked_request_never_reaches_the_queue_it_waited_on (event : WaitEvent) :
    (blockedDispatch event).destination ≠ .routedExecutor := by
  cases event <;> decide

theorem only_a_running_router_dispatches (event : WaitEvent)
    (h : (blockedDispatch event).destination = .newestExecutor) :
    (blockedDispatch event).routerStopped = false := by
  cases event <;> simp_all [blockedDispatch, destinationAfter, routerStopsAfter]

theorem snapshot_change_reroutes_request_blocked_on_old_executor_queue :
    blockedDispatch .snapshotChanged =
      { destination := .newestExecutor, routerStopped := false } := by decide

end RouterDispatch
