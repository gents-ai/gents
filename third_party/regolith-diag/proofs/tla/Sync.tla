---- MODULE Sync ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A lock lets one task at a time into a piece of code. regolith has its own
\* locks for async code (src/sync/): Mutex, Semaphore (a lock with several
\* "permits") and ReentrantMutex. A task that finds the lock taken does not
\* block its thread: it joins a queue of waiters and is woken later.
\*
\* The simplest fair rule would be: when the holder lets go, hand the lock to
\* the waiter that has waited longest. That is fair but slow: the lock then
\* sits idle until that sleeping task gets scheduled, and everyone else
\* waits for it. So these locks allow "barging": whoever asks while the lock
\* is free gets it, even past the queue. A release only "nudges" the oldest
\* waiters: it wakes them so they can try again.
\*
\* Waiting is costly too: a task that queues and sleeps must be woken by the
\* release, and the thread that runs it may have to be woken by the kernel. So a
\* task that finds the lock held first spins a little (E48): it looks at the lock
\* now and then for a short while and takes it if it is let go meanwhile; only
\* then does it queue. And a queued waiter that has just lost a retry spins on its
\* own waiter node before it sleeps: a release that nudges it then sets the mark
\* without waking anybody, because the waiter is awake and watching its node.
\* Tiny example: tasks A and B take turns at a lock on two cores. A holds; B spins;
\* A lets go; B sees it and takes it; nobody queued, nobody slept, nobody was woken.
\*
\* Barging alone is unfair. Tiny example: task A waits. Each time the lock is
\* released A is nudged, but before A gets to run, task B, which is already
\* running, grabs the lock again. A loses the race, and loses it again, for
\* ever. So A counts the races it loses. The retry that would be its
\* Bound-th loss does two things in one indivisible step: it finds the lock
\* taken, and it raises a flag on the lock that says "a handoff is owed".
\* While the flag is up nobody may barge: what a release frees stays put,
\* and the queue is served strictly oldest first until A has been served.
\*
\* What can go wrong, each a RED configuration below: a waiter starves; a
\* release frees the lock and tells nobody; somebody barges past an owed
\* handoff (a spinner too); a waiter that stops spinning goes to sleep
\* without looking, though a release nudged it while it spun and woke nobody;
\* a waiter that gave up (its future was dropped) keeps the lock it had just
\* been handed; a change made while another thread was serving the
\* queue is never looked at; a thread serving the queue is kept at it by
\* releases that have nobody to tell; a reentrant lock is freed while a
\* guard still exists.
\*
\* What this model shows: over every interleaving of a few tasks acquiring,
\* releasing, cancelling and passing guards around, one machine instruction
\* at a time, there is never more than one holder (never more permits out
\* than exist), nobody loses more than Bound races, an owed handoff cannot
\* be barged, no waiter sleeps beside a lock it could take, every waiter is
\* served, and a thread serving the queue runs one pass plus at most one
\* for each thing another thread does to the queue meanwhile.
\*
\* What it does NOT promise: that a thread serving the queue is done after
\* a fixed number of steps whatever the others do. Each waiter that queues
\* or withdraws while it serves costs it one more pass. These locks are
\* lock-free, not wait-free: somebody always gets on, and every extra pass
\* is another thread's own operation getting done.
\*
\* The primitives covered: Mutex, Semaphore (owned guards included) and
\* ReentrantMutex after D49. (Notify keeps FIFO handoff: SyncNotify.tla.
\* RwLock: SyncRwLock.tla.)
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/Sync.lean (the Mutex,
\* one permit, every interleaving of the same steps taken whole, any number
\* of requests, any Bound of at least 1):
\*   step_inv, reachable_inv      the protocol's invariant holds in every
\*                                reachable state
\*   mutual_exclusion             at most one holder, and none while free
\*                                (MutualExclusion here)
\*   bounded_bypass, lose_below_bound, bypass_counts_losses,
\*   owed_at_bound, at_bound_owed a waiter's count of lost races never
\*                                passes Bound, only a loss moves the count,
\*                                the loss that reaches Bound raises the
\*                                owed flag in the same step, and a waiter
\*                                at Bound is owed (BoundedBypass here)
\*   release_hands_off_at_bound   once a waiter is owed, the release that
\*                                follows is handed to it
\*   owed_exclusive,              while a handoff is owed the lock is not
\*   owed_blocks_barging          free and nobody competes, and no step
\*                                gives the lock to anyone but the owed
\*                                waiter (HandoffExclusive here)
\*   no_stranded_head,            a free lock with waiters has nudged its
\*   handed_woken                 head; a waiter handed the lock is woken
\*                                (NoLostWakeup here)
\*   no_lost_wakeup_spinning,     a waiter asleep with its waker armed never
\*   mark_spinning_wakes_nobody   has an unread mark, spins included, and a
\*                                release that finds it spinning calls no
\*                                waker (NoLostWakeup here; the model has no
\*                                ARMED bit: a spinning task is simply not
\*                                "parked", so a release puts it on no wake
\*                                list)
\*   unbounded_barging_breaks_bound, release_without_wake_strands,
\*   spin_without_look_strands    the RED cases, as counterexamples
\* The Lean machine has no drain role, so DrainBounded and the two liveness
\* promises are checked here only.
\*
\* CHECKED ON THE REAL CODE by loom (tests/loom_sync.rs), which runs the Rust
\* itself under every schedule within a preemption bound:
\*   mutex_excludes_and_loses_no_wakeup               MutualExclusion,
\*                                                    NoLostWakeup
\*   a_waiter_barged_past_max_bypass_times_is_handed_the_mutex
\*                                                    BoundedBypass,
\*                                                    HandoffExclusive
\*   a_cancelled_mutex_waiter_racing_the_handoff_loses_nothing
\*                                                    CancelPassesOn
\*   a_semaphore_never_hands_out_more_permits_than_it_has
\*                                                    MutualExclusion,
\*                                                    PermitsConserved
\*   a_large_request_behind_small_callers_is_owed_its_permits
\*                                                    BoundedBypass for a
\*                                                    request of several
\*                                                    permits
\*   a_reentrant_mutex_unwinds_its_depth_before_another_owner_enters
\*                                                    ReentrancyDepth
\*   a_waiter_serving_the_queue_is_served_while_the_holder_barges
\*                                                    EveryWaiterServed
\*                                                    (it counts the
\*                                                    passes of the
\*                                                    waiter's own drain)
\*   a_drain_that_found_nobody_queued_ends_while_another_thread_releases
\*                                                    DrainBounded
\*   the_first_attempt_refuses_a_lock_owed_to_a_waiter  HandoffExclusive,
\*                                                    BoundedBypass, for the
\*                                                    Mutex's inline attempt
\*   a_nudge_while_the_waiter_spins_is_never_lost      NoLostWakeup for a
\*                                                    waiter spinning on its
\*                                                    node (with floors on
\*                                                    the spins it reaches)
\* Each of the last four has a calibration: a copy of the run with the one
\* step written the old way (for the two E48 models: with its guard taken
\* away), which must fail. Under loom a spin has one round, so every model
\* above also runs the spin before queueing; the last one also makes sure the
\* spin on the node happens, and counts it.
\*
\* DrainBounded is also checked by two unit tests of src/sync/contend.rs,
\* which race a release against a drain at a chosen instant:
\*   a_drain_that_found_nobody_queued_does_not_pass_again_for_releases
\*   a_release_during_a_pass_that_found_a_waiter_reaches_it
\*
\* THE CODE MODELLED, one step here per atomic instruction there (the few
\* reads that share a step are listed under WHAT THE MODEL LEAVES OUT).
\*   - The state word (WaitQueue::state, src/sync/queue.rs) holds the free
\*     permits above six flag bits: DRAINING, DIRTY, QUEUED, NUDGE_OUT,
\*     HANDOFF and WAKE_FRONT. Each is a variable below.
\*   - Acquire (Contend::poll, src/sync/contend.rs; a Mutex makes this first
\*     attempt inline, RawSemaphore::take_one). One compare-and-swap takes the
\*     permits when they are free and HANDOFF is down, whoever waits: barging
\*     (contend::take). When it fails the out-of-line path tries once more
\*     from the state the first attempt saw (Retake, contend::take_from),
\*     then spins: it reads the state word now and then and takes the permits
\*     if a read shows them free, until it runs out of rounds or sees a
\*     handoff owed or waiters queued with nobody changing the queue
\*     (PreSpin, PreSpinOut; contend::spin_take). Then it pushes a waiter node
\*     on the arrival stack, raises QUEUED, and looks at its node once more
\*     before sleeping.
\*   - Release (RawSemaphore::release, src/sync/raw_semaphore.rs). One add
\*     puts the permits back in the state word. Only if waiters are queued
\*     and no nudge is already on its way (or a handoff is owed) does it go
\*     on to ask for a drain pass (Policy::released, a Step::Wake).
\*   - The drain role (Policy::transition and Policy::drain). Only one
\*     thread at a time sorts arrivals, grants and nudges: the one that
\*     raised DRAINING. Nobody waits for it. A waiter that queues or
\*     withdraws and finds the role taken raises DIRTY and leaves
\*     (Step::Drain); a release that finds it taken leaves only its request,
\*     WAKE_FRONT (Step::Wake). The holder gives the role up with a
\*     compare-and-swap that needs DIRTY down, and WAKE_FRONT down unless its
\*     pass found nobody queued; otherwise it passes again.
\*   - One pass (RawSemaphore::pass). While HANDOFF is up it takes permits
\*     for the front waiter and grants them, oldest first, for as long as
\*     they last. While no waiter is owed it nudges the waiters the free
\*     permits could serve, oldest first, stopping at the first that does
\*     not fit: more than the front, when the permits reach further. After
\*     a release it nudges the front whatever it asks for.
\*   - A nudged waiter, polled, takes back the NUDGE_OUT marker and tries
\*     the compare-and-swap again. A failed try counts one bypass in the
\*     waiting future (Contend::bypass), unless HANDOFF was up; the count
\*     stops at Bound. That holds in the poll that queued the waiter too:
\*     only its very first attempt, before it queued, is no bypass. The try
\*     that would be its Bound-th counted loss is Lock::take_or_owe: it
\*     counts itself owed, marks its node, and in one compare-and-swap
\*     either takes the permits or raises HANDOFF.
\*   - After a failed retry the future may spin on its own node (SpinStart,
\*     SpinEnd; Wait::spin_park): Waiter::begin_spin takes the node's ARMED
\*     bit down, so a release that nudges or grants it finds no waker to claim
\*     and only sets the mark; the spin ends when it sees a mark or runs out,
\*     and the look that follows arms the waker and reads the marks in one
\*     compare-and-swap.
\*   - Cancellation (Contend::cancel). A dropped future withdraws its node;
\*     a nudge it carried unused passes to the next front (WAKE_FRONT), and
\*     permits it had been granted are released like any others.
\*
\* WHY THE MARKER STAYS UP FOR A WAITER NUDGED ALREADY (defect E32, which
\* this model found). A waiter that queues takes the drain role in the same
\* poll when nobody holds it, so it can be the thread that serves the queue
\* while its own node sits in it. Tiny example: task W waits, task H only
\* tries. H holds; W queues and drains. H lets go while W's first pass is
\* under way, so that pass nudges W's own node, and DIRTY makes W pass
\* again. The second pass nudges the front again and finds W's node nudged
\* already: W cannot take that nudge, it is busy draining. If the pass took
\* that for "no nudge is on its way" and lowered NUDGE_OUT, every release
\* by H would find the marker down, ask for a pass and raise DIRTY, and W's
\* drain would pass again, for as long as H kept taking the lock and
\* letting it go: W would never retry, never count a bypass, never raise
\* HANDOFF. So a nudge that finds its waiter nudged already counts as a
\* nudge on its way (WakeList::nudge): the marker stays up, H's releases
\* stay one instruction, the drain ends and W retries. And the retry W
\* loses right after its own drain counts as a bypass, though it is still
\* in the poll that queued it. The promise that holds this down is
\* EveryWaiterServed; MC_Sync_Red_LowerPendingMarker plants the old
\* behaviour and breaks it.
\*
\* WHAT A DRAIN COSTS ITS THREAD (the second half of E32). The thread that
\* holds the drain role must not be kept at it by releases that have nobody
\* to tell. Tiny example: task A drops its guard and takes the role to tell
\* the queue, and its pass finds nobody queued. Task B, meanwhile, takes the
\* lock and lets it go, again and again; QUEUED is still up (A has not left
\* yet), so each of B's releases asks for a pass. If asking raised DIRTY, A
\* would pass once more for every one of them and never return, though
\* there is nobody to wake. So a release that finds the role taken leaves
\* only WAKE_FRONT, and A's exit drops that request when its pass found
\* nobody queued. Nothing is lost by dropping it: a waiter that queued
\* after A's pass took the arrivals raises DIRTY itself, or finds the role
\* free and passes itself, and reads what B released from the state word.
\*
\* The promise is DrainBounded, and it is exact. A drain runs one pass. It
\* runs another only if, since that pass began, one of these landed on it:
\*   - a waiter queued, withdrew, or claimed the handoff it is owed (each
\*     raises DIRTY);
\*   - a release found a node queued (it leaves WAKE_FRONT). That pass is
\*     needed: the front waiter must be nudged for what the release freed,
\*     and MC_Sync_Red_ExitIgnoresRequest, which drops the request, breaks
\*     NoLostWakeup. The pass leaves NUDGE_OUT up, so the releases after it
\*     ask for nothing at all until a waiter has taken its nudge.
\* A release over an empty queue earns no pass. MC_Sync_Red_ReleaseDirties
\* plants the old behaviour and breaks DrainBounded.
\*
\* What is not promised is a fixed number of passes. Waiters that keep
\* queueing or withdrawing while one thread holds the role keep that thread
\* passing, one pass each: MC_Sync_Green_PassesGrow is the witness, a drain
\* of three passes in the code as built. A design that caps the passes and
\* hands the role on was modelled and set aside: handing the role to the
\* front waiter by a nudge leaves a window in which that waiter takes the
\* nudge back and the holder must nudge again, for ever; handing it over by
\* a mark on the waiter's node closes the window but changes every
\* primitive's poll path, and still cannot bound a holder that finds only
\* futures dropped while they were queued. So the liveness promise is
\* conditional and says so: DrainsEndWhenWaitersSettle.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - The waker slots of a node (src/sync/waiter.rs: registering, armed,
\*     taking). Their job is that a grant or nudge either finds a sleeping
\*     future and wakes it, or is seen by a future that is in the middle of
\*     a poll. Here that is the rule itself: a mark on a sleeping task puts
\*     it on the pass's wake list, a mark on a running task is read by that
\*     task's next look at its node.
\*   - Polls with no wake. An executor may poll a future nobody woke. Such a
\*     poll reads the same two marks; with both clear it changes nothing.
\*   - A task has one waiter node at a time, and does not queue again while
\*     a node it withdrew is still linked; two nodes of one task act as two
\*     tasks.
\*   - When a pass sweeps withdrawn nodes out of the middle of its list is
\*     a cost rule (List::tidy counts cancellations). The model lets every
\*     pass either sweep or not, so both are checked.
\*   - Steps that share one step here. The swap that takes the arrival
\*     stack goes with the fetch-and before it (an arrival always announces
\*     itself afterwards, with DRAINING or DIRTY). A pass's two reads of the
\*     HANDOFF bit (before its handoff loop, and in WaitQueue::hands_off) go
\*     with that same fetch-and: only a waiter raises the bit, in a
\*     transition that also raises DIRTY, so a raise that lands later makes
\*     the drain pass again; and only the holder of the drain role lowers
\*     it. The calls of the wakers go with the instruction that gives the
\*     drain role up (a called waker only makes a later poll possible). A
\*     pass reads "has this node withdrawn" in the same step as it acts on
\*     the node (the grant and the nudge check again, atomically).
\*   - Memory ordering is loom's (plan 7.1).
\*   - How long a spin lasts. The code counts rounds and adapts the count per
\*     lock (spin::Budget: doubled when a spin saw what it waited for, halved
\*     when one ran out without seeing room, raised to one by a won retry;
\*     spin::Gaps spaces the reads). Here a spin may end after any number of
\*     rounds, none included, so every count the code can reach is covered;
\*     the count only sets how fast the code is, never what it does.
\*   - Reads that see the lock taken. A spin round that reads the word and
\*     finds no room changes nothing, and a round whose compare-and-swap
\*     fails changes nothing either: neither is a step here.
\*
\* SHORT SCENARIOS. Three tasks that each ask as often as they like, at one
\* step per instruction, are over four million states: far more than a row
\* may take. So the three-task configurations are scenarios, the way a loom
\* model is: Turns says how often each task asks, Start who holds the lock
\* at the beginning, Follows fixes the order two waiters arrive in. The
\* two-task configurations run free.
\*
\* CONFIGURATIONS.
\*   MC_Sync_Green_Mutex          Mutex, three tasks: two waiters queued at
\*                                once behind a starting holder, one of
\*                                which may cancel: every safety invariant.
\*   MC_Sync_Green_MutexBarger    Mutex, a waiter that may cancel against a
\*                                task that only tries, Bound 2, as often as
\*                                they like: every safety invariant.
\*   MC_Sync_Green_MutexBoth      Mutex, two tasks that both wait, Bound 1,
\*                                as often as they like: every safety
\*                                invariant.
\*   MC_Sync_Green_MutexLive      Mutex, two tasks: every waiter is served,
\*                                and once the waiters settle every drain
\*                                ends.
\*   MC_Sync_Green_PassesGrow     the witness: the same two tasks as
\*                                MutexBoth, and a drain of three passes.
\*                                FewPasses, which is no promise, fails.
\*   MC_Sync_Green_Semaphore      two permits given back in one release to
\*                                two waiters of one permit each: the pass
\*                                nudges both.
\*   MC_Sync_Green_BigRequest     two permits, task 1 asks for both and
\*                                waits while two tasks take one at a time.
\*   MC_Sync_Green_SemaphoreLive  two permits, two tasks: every waiter is
\*                                served, and once the waiters settle every
\*                                drain ends.
\*   MC_Sync_Green_Reentrant      ReentrantMutex, tasks 1 and 2 of one owner,
\*                                nesting two guards: every safety invariant.
\*   MC_Sync_Green_ReentrantLive  one task of each owner, one guard each: if
\*                                no owner holds forever, every waiter is
\*                                served; and once the waiters settle every
\*                                drain ends.
\*   MC_Sync_Red_UnboundedBarging the waiter never raises HANDOFF:
\*                                BoundedBypass.
\*   MC_Sync_Red_LoseQueuePosition  a waiter that loses starts over at the
\*                                tail with a fresh count: BoundedBypass.
\*   MC_Sync_Red_ReleaseNoWake    a release frees the lock and tells nobody:
\*                                NoLostWakeup.
\*   MC_Sync_Red_NoRecheck        a waiter sleeps without looking at its node
\*                                after queueing: NoLostWakeup.
\*   MC_Sync_Red_NoDirty          a thread that finds the drain role taken
\*                                leaves without raising DIRTY: NoLostWakeup.
\*   MC_Sync_Red_LowerPendingMarker  a nudge that finds its waiter nudged
\*                                already lowers NUDGE_OUT: the waiter's own
\*                                drain never ends, EveryWaiterServed.
\*   MC_Sync_Red_ReleaseDirties   a release that finds the drain role taken
\*                                raises DIRTY: a drain passes again over an
\*                                empty queue, DrainBounded.
\*   MC_Sync_Red_ExitIgnoresRequest  the drain gives its role up with a
\*                                release's request pending and a waiter
\*                                queued: NoLostWakeup.
\*   MC_Sync_Red_CancelNoPassOn   a waiter dropped after a grant keeps the
\*                                permits: CancelPassesOn.
\*   MC_Sync_Red_IgnoreOwed       the fast path ignores HANDOFF:
\*                                HandoffExclusive.
\*   MC_Sync_Red_NoDepth          a reentrant guard drop frees the lock at
\*                                any depth: ReentrancyDepth.
\*   MC_Sync_Red_SpinNoRecheck    a waiter whose spin on its node runs out
\*                                sleeps without reading the node's marks;
\*                                a release that nudged it while it spun
\*                                woke nobody: NoLostWakeup.
\*   MC_Sync_Red_SpinIgnoresOwed  the spin before queueing does not stop for
\*                                an owed handoff and takes the lock the
\*                                release freed for the owed waiter:
\*                                HandoffExclusive.
\* Every row above runs the spins: the GREEN rows check every safety promise
\* with tasks that spin before queueing and on their nodes.

\* ===========================================================================
\* WHERE EACH PIECE OF THIS MODEL LIVES IN THE RUST CODE (E48)
\* ===========================================================================
\* Read it as: "this word of the model" is "this place in the program", and
\* what it means in plain words. Paths are under src/sync/; line numbers are
\* those of the commit that last updated this table. A step of the model is one
\* atomic instruction of the program unless the line says otherwise.
\*
\* THE STATE (what the program remembers):
\*   avail       raw_semaphore.rs:126 and queue.rs:182 (WaitQueue::state, the
\*               bits above FIRST_BIT): how many permits are free.
\*   drainOn     queue.rs:91 DRAINING: somebody is serving the queue.
\*   dirty       queue.rs:93 DIRTY: the queue changed while it was served.
\*   queued      queue.rs:95 QUEUED: waiters may be queued.
\*   nudgeOut    queue.rs:97 NUDGE_OUT: a nudge is on its way to a waiter.
\*   handoff     queue.rs:99 HANDOFF: a waiter is owed the lock; no barging.
\*   wakeFront   queue.rs:102 WAKE_FRONT: a release asks to wake the front.
\*   owedCount   queue.rs:186 WaitQueue::owed: how many waiters are owed.
\*   arrivals    queue.rs:183 WaitQueue::arrivals: the stack new waiters join.
\*   waiting     raw_semaphore.rs:37 RawSemaphore::waiting: the sorted queue.
\*   phase       waiter.rs:54 PHASE bits: waiting, granted, cancelled.
\*   nudged      waiter.rs:63 NUDGED: told to try again, not looked yet.
\*   owedMark    waiter.rs:64 OWED: this waiter is counted owed.
\*   wake, wl    waiter.rs:651 WakeList and its calls in Drop (waiter.rs:741):
\*               whose waker is (to be) called.
\*   got, held   the permits a future took (raw_semaphore.rs:279 poll_one's
\*               Ready) and the guard that holds them (mutex.rs:149 its drop).
\*   owner,depth reentrant_mutex.rs:117 enter, 103 reenter, 124 unlock.
\*   bypass      contend.rs:172 Contend::bypass: races this waiter lost.
\*   marked, took, seenH   locals of Policy::owe_or_take (queue.rs:408).
\*   passOn      the nudge flag Wait::cancel (waiter.rs:912) returns in
\*               Cancel::Withdrawn, which Contend::cancel (contend.rs:281) passes on.
\*   spinning    no field of its own: the stretch of Wait::spin_park
\*               (waiter.rs:844) after Waiter::begin_spin (waiter.rs:283)
\*               cleared ARMED (waiter.rs:294) and before the look re-arms it.
\*               While it is TRUE the node has no waker to claim
\*               (waiter.rs:115 claimable), so a release only marks it.
\*   dWF ... dClearH   locals of Policy::drain (queue.rs:347) and
\*               RawSemaphore::pass (raw_semaphore.rs:188).
\*   pc, ret     where each task is in the code; not a field.
\*   lost, bargedOwed, dPasses, dEarned, overran, turns   bookkeeping the
\*               model keeps to state its promises; no field.
\*
\* THE STEPS (what the program does), each with the instruction it is:
\*   Acquire     the first attempt: mutex.rs:224 poll_one ->
\*               raw_semaphore.rs:89 take_one (a load at :91, then the swap at
\*               :94 from the free word or at :99 from the state loaded), or
\*               for a Semaphore contend.rs:213 try_take -> contend.rs:88 take.
\*   Retake      contend.rs:212 take_from(seen): the out-of-line path's first
\*               swap, from the state the inline attempt saw (contend.rs:100).
\*   PreSpin     one round of contend.rs:134 spin_take: the stop test at :143
\*               (queue.rs:119 stops_spin), the load at :146, and the swap at
\*               :153 when the load showed room.
\*   PreSpinOut  the spin's rounds run out (spin.rs:68 Gaps, spin.rs:109
\*               Budget::rounds); contend.rs:164 halves the budget.
\*   Reenter     reentrant_mutex.rs:103 reenter.
\*   Push        waiter.rs:808 Wait::enqueue -> queue.rs:236 WaitQueue::push.
\*   Publish     queue.rs:379 Policy::enqueue's transition (queue.rs:313).
\*   Park0       contend.rs:224 park(queue, None) -> waiter.rs:213 Waiter::park.
\*   Repark      contend.rs:267 spin_park with no spin (budget 0, or the node
\*               marked: begin_spin false) -> waiter.rs:820 Wait::park.
\*   SpinStart   waiter.rs:844 spin_park -> waiter.rs:283 begin_spin: a load,
\*               and a swap clearing ARMED (waiter.rs:294) if it was armed.
\*   SpinEnd     the spin loop's last waiter.rs:306 marked() read, then the
\*               look waiter.rs:820 park(Some(waker)), whose last swap arms the
\*               waker and reads the marks together (waiter.rs:213).
\*   Poll        the executor polls a woken future: poll_one -> poll_contended
\*               (raw_semaphore.rs:293) -> contend.rs:197 poll_from -> park.
\*   GrantCons   contend.rs:228 Park::Granted -> queue.rs:232 consume_nudge.
\*   NudgeCons   contend.rs:239 Park::Nudged -> queue.rs:232 consume_nudge.
\*   Try         contend.rs:244 try_take for a retry; a loss counts at :262.
\*   Owe1..Owe7  contend.rs:242 take_or_owe -> queue.rs:408 owe_or_take:
\*               the owed count, Waiter::owe (waiter.rs:313), the swap,
\*               Waiter::unowe (waiter.rs:334), forgive (queue.rs:226), the
\*               fetch-or, and the last transition.
\*   Wd1, Wd2    contend.rs:278 Contend::cancel -> waiter.rs:912 Wait::cancel
\*               -> waiter.rs:355 withdraw, then queue.rs:388 withdrawn.
\*   Cancel      the future is dropped: mutex.rs Lock's drop ->
\*               raw_semaphore.rs:306 PermitWait::cancel -> Contend::cancel.
\*   Gb1..Gb3    Cancel::Granted in Contend::cancel: consume_nudge, give_back
\*               (raw_semaphore.rs:170) = release, released.
\*   Release     raw_semaphore.rs:122 release: the add at :126, the test
\*               queue.rs:110 wants_drain.
\*   Released    raw_semaphore.rs:136 release_wakes -> queue.rs:396 released.
\*   Give        moving an owned guard (OwnedMutexGuard): no instruction.
\*   D0          queue.rs:347 drain's fetch-and, queue.rs:254 take, and the
\*               list's absorb, tidy and front (list.rs:60, :117, :85).
\*   DH, DG, DGB the handoff loop of RawSemaphore::pass: take_for_waiter
\*               (raw_semaphore.rs:198), WakeList::grant (waiter.rs:669),
\*               and the add that puts back a refused grant.
\*   DHO         queue.rs:221 hands_off.
\*   DNB..DNF    the nudging walk of pass (raw_semaphore.rs:215-221):
\*               Nudges::nudge -> WakeList::nudge (waiter.rs:686) ->
\*               Waiter::touch (waiter.rs:184), and Nudges::finish
\*               (queue.rs:159).
\*   DEnd, DEx   the end of drain: the exit swap, then the wakers called as
\*               the WakeList drops (waiter.rs:741).
\*
\* THE PROMISES (what must always hold), and what in the code keeps them:
\*   MutualExclusion, PermitsConserved   every take is a swap that checks
\*               the permits (take_one, take_from, spin_take, admit).
\*   BoundedBypass, HandoffExclusive    the bypass count and owe_or_take;
\*               every barging swap refuses HANDOFF (take_one :92/:97,
\*               take_from, stops_spin for the spin).
\*   NoLostWakeup (MarkedWoken, Stranded)  park's swaps read the marks as
\*               they arm the waker; a spin ends in that same look.
\*   (No promise here says a spinner is unarmed: the model has no ARMED
\*   bit. That a release finds a spinner unarmed is begin_spin clearing
\*   ARMED, checked by the waiter.rs unit tests and the loom model above.)
\*   CancelPassesOn    Contend::cancel gives a grant back.
\*   DrainBounded, DrainsEndWhenWaitersSettle   queue.rs:313-375.
\*   EveryWaiterServed   the bypass bound plus the handoff.
\*
\* WHAT IS IN THE CODE BUT NOT A STEP HERE, and why that is safe:
\*   - The budget's own reads and writes (spin.rs Budget::won, ran_out):
\*     a hint that only sizes spins; the model lets a spin be any length.
\*   - The pause between spin reads (spin.rs:49 pause): no memory access.
\*   - A spin round that sees no room, and a swap that fails: they change
\*     no memory, so they are stutters.
\*   - The waker slots (registering, taking): see WHAT THE MODEL LEAVES OUT.
\* WHAT IS A STEP HERE BUT NOT IN THE CODE: Retake for a Semaphore or a
\* ReentrantMutex, whose acquire has no inline attempt: one barging attempt
\* more than the code makes, which only lets the model do more.
\*
\* The same table holds for SyncRwLock.tla's acquire, retry and spin steps,
\* with RawRwLock's admit (raw_rwlock.rs) in place of the semaphore's; the
\* RwLock has no Retake (it has no inline attempt).

\* Borrow the standard TLA+ books: counting numbers, lists, and sizes of finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs a configuration chooses.
CONSTANTS
  \* The tasks that use the lock, named by positive numbers.
  Tasks,
  \* Which primitive is modelled: "Semaphore" (a Mutex is a semaphore with one permit,
  \* src/sync/mutex.rs) or "Reentrant" (ReentrantMutex, src/sync/reentrant_mutex.rs).
  Kind,
  \* How many permits there are. A Mutex and a ReentrantMutex have 1.
  Permits,
  \* How many permits each task asks for, as a map from task to number.
  Need,
  \* Each task's owner token, as a map from task to a positive number (ReentrantMutex only; Owner,
  \* src/sync/owner.rs). Tasks with the same token count as one owner.
  Owner,
  \* The largest number of nested guards one task holds (ReentrantMutex only).
  MaxDepth,
  \* TRUE when a guard may move from one task to another (the owned guards of lock_owned and
  \* acquire_owned).
  Owned,
  \* After how many lost races a waiter is owed a handoff: MAX_BYPASS in src/sync/contend.rs (16 in
  \* the engine, 2 under loom).
  Bound,
  \* The tasks that wait when the lock is taken (lock, acquire). The others only try (try_lock,
  \* try_acquire): when their one attempt fails they go away.
  Waits,
  \* The tasks whose waiting future may be dropped before it is served.
  Drops,
  \* How many times each task may ask for the lock, as a map from task to number; 0 means as often
  \* as it likes. A small number makes a run a short scenario, the way a test has a few calls.
  Turns,
  \* The tasks that hold a guard when the run starts, the way a test takes the lock before it
  \* spawns its threads. Their first hold counts as one of their turns.
  Start,
  \* For each task, a task that must have queued (or got the lock) before this one may ask, or 0
  \* for none: the way a test waits for a "queued" flag before it starts the next thread
  \* (flag_when_queued in tests/loom_sync.rs). It fixes the order two waiters arrive in.
  Follows,
  \* The tasks whose "is eventually served" promise is checked.
  Watched,
  \* Where the ghost count of a drain's passes stops. 0 in most configurations, which do not need
  \* the count; 3 in the witness MC_Sync_Green_PassesGrow, enough to tell "three passes" from fewer.
  PassMax,
  \* "none" for the code as built, or the name of one planted bug.
  Mutant

\* The value "no owner holds the lock".
NoOwner == 0
\* Every Owner token some task locks with.
Owners == {Owner[t] : t \in Tasks}

\* The kind is one of the two.
ASSUME Kind \in {"Semaphore", "Reentrant"}
\* There is at least one permit, at least one level of nesting, and the bound is at least 1.
ASSUME Permits \in Nat \ {0} /\ MaxDepth \in Nat \ {0} /\ Bound \in Nat \ {0}
\* Owned is a yes or a no, and the three sets of tasks are sets of tasks.
ASSUME Owned \in BOOLEAN /\ Waits \subseteq Tasks /\ Drops \subseteq Tasks /\ Watched \subseteq Tasks
\* Every task asks for at least one permit and no more than exist, and has a number of turns.
ASSUME \A t \in Tasks : Need[t] \in 1..Permits /\ Turns[t] \in Nat
\* Tasks and owner tokens are positive numbers (0 means "none").
ASSUME Tasks \subseteq Nat \ {0} /\ Owners \subseteq Nat \ {0}
\* A reentrant lock has one permit, and at most one task holds it at the start.
ASSUME Kind = "Reentrant" => Permits = 1 /\ Cardinality(Start) <= 1
\* The tasks that start with a guard are tasks, and a task comes after a task or after nobody.
ASSUME Start \subseteq Tasks /\ \A t \in Tasks : Follows[t] \in Tasks \cup {0}
\* The mutant is "none" or one of the thirteen planted bugs.
ASSUME Mutant \in {"none", "UnboundedBarging", "LoseQueuePosition", "ReleaseNoWake", "NoRecheck",
                   "NoDirty", "LowerPendingMarker", "ReleaseDirties", "ExitIgnoresRequest",
                   "CancelNoPassOn", "IgnoreOwed", "NoDepth", "SpinNoRecheck",
                   "SpinIgnoresOwed"}

\* Configuration helpers, named in the MC_Sync_*.cfg files.
\* Every task asks for one permit.
NeedOne == [t \in Tasks |-> 1]
\* Task 1 asks for every permit, the others for one.
NeedFirstAll == [t \in Tasks |-> IF t = 1 THEN Permits ELSE 1]
\* Every task is its own Owner.
OwnerEach == [t \in Tasks |-> t]
\* Tasks 1 and 2 share Owner 1 (two tasks pinned to one thread, say); every other task is Owner 2.
OwnerPair == [t \in Tasks |-> IF t \in {1, 2} THEN 1 ELSE 2]
\* No task has to come after another.
FollowsNobody == [t \in Tasks |-> 0]
\* Task 2 asks only after task 1 has queued or got the lock.
SecondFollowsFirst == [t \in Tasks |-> IF t = 2 THEN 1 ELSE 0]
\* Task 3 asks only after task 2 has queued or got the lock.
ThirdFollowsSecond == [t \in Tasks |-> IF t = 3 THEN 2 ELSE 0]
\* Task 2 asks only after task 1, and task 3 only after task 2.
EachFollowsThePrevious == [t \in Tasks |-> IF t = 1 THEN 0 ELSE t - 1]
\* Every task asks as often as it likes.
TurnsAny == [t \in Tasks |-> 0]
\* Every task asks once.
TurnsOnce == [t \in Tasks |-> 1]
\* A task that waits asks once; a task that only tries asks twice.
TurnsTryTwice == [t \in Tasks |-> IF t \in Waits THEN 1 ELSE 2]

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The permits that are free to take: the count kept in the state word above its flag bits
  \* (WaitQueue::state in src/sync/queue.rs; RawSemaphore::available in src/sync/raw_semaphore.rs
  \* reads it). For a ReentrantMutex: 1 when unlocked.
  avail,
  \* The DRAINING bit of the state word: some thread holds the drain role, the only one allowed to
  \* sort arrivals, grant and nudge.
  drainOn,
  \* The DIRTY bit: something changed while the drain role was held, so its holder must pass again
  \* before it lets go.
  dirty,
  \* The QUEUED bit: waiters may be queued, so a release has somebody to tell.
  queued,
  \* The NUDGE_OUT bit: a nudge is on its way to a waiter that has not polled yet, so a release need
  \* not run another pass.
  nudgeOut,
  \* The HANDOFF bit: a waiter is owed a handoff. While it is up nobody may barge.
  handoff,
  \* The WAKE_FRONT bit: a release (or a nudged waiter that withdrew) asks the next pass to nudge
  \* the front waiter whatever it asks for.
  wakeFront,
  \* How many queued waiters count themselves owed a handoff (WaitQueue::owed, an atomic counter
  \* beside the state word).
  owedCount,
  \* The arrival stack: nodes pushed by waiters and not yet taken by a pass, oldest first
  \* (WaitQueue::arrivals; a pass takes them all with one swap and reverses them).
  arrivals,
  \* The waiting list a pass keeps between passes, oldest first (RawSemaphore::waiting, a List of
  \* src/sync/list.rs). Only the holder of the drain role touches it. A withdrawn node stays linked
  \* until a pass lets go of it.
  waiting,
  \* The phase of each task's waiter node: "none" (no node), "waiting", "granted" (a pass handed it
  \* what it asked for) or "cancelled" (its future withdrew). The two phase bits of Waiter::state in
  \* src/sync/waiter.rs.
  phase,
  \* Each node's NUDGED bit: a pass asked the waiter to try again and it has not looked yet.
  nudged,
  \* Each node's OWED bit: the waiter reached the bypass bound and is counted in owedCount.
  owedMark,
  \* Whether each task's waker was called since the task last polled: the executor will poll it.
  wake,
  \* The wake list of the pass under way: the sleeping tasks it granted or nudged, whose wakers it
  \* calls when it gives the drain role up (WakeList, src/sync/waiter.rs).
  wl,
  \* The permits taken out of the state word for each task's pending acquire, by its own
  \* compare-and-swap or by a grant, that are not in a guard yet.
  got,
  \* The permits each task holds in a guard. For a ReentrantMutex: how many guards it holds.
  held,
  \* ReentrantMutex only: the owner token that holds the lock, or NoOwner (ReentrantMutex::holder).
  owner,
  \* ReentrantMutex only: how many guards are out on the lock (ReentrantMutex::depth).
  depth,
  \* Each waiting future's count of lost races (Contend::bypass, src/sync/contend.rs).
  bypass,
  \* A local of Policy::owe_or_take: whether the waiter's node took the OWED mark.
  marked,
  \* A local of Policy::owe_or_take: whether the compare-and-swap took the permits.
  took,
  \* A local of Policy::owe_or_take: whether the state it saw had HANDOFF up.
  seenH,
  \* A local of Contend::cancel: whether the withdrawn node carried a nudge nobody used.
  passOn,
  \* Whether the future spins on its own node after a lost try (Wait::spin_park in
  \* src/sync/waiter.rs). In the code this is the stretch between Waiter::begin_spin, which takes
  \* the node's ARMED bit down, and the look that arms it again: while it is TRUE the node has no
  \* waker a release could claim, so a release that nudges or grants it only sets the mark, and
  \* the spin itself sees the mark. It is the future's own state, so it sits with its locals.
  spinning,
  \* A local of the pass under way: whether WAKE_FRONT was up when the pass began (`wake_front`).
  dWF,
  \* A local of the pass under way: the free permits not yet promised to a nudged waiter (`budget`).
  dBudget,
  \* A local of the pass under way: the place in the waiting list its nudging has reached.
  dPos,
  \* A local of the pass under way: TRUE until its nudging has passed the first live waiter.
  dFirst,
  \* A local of the pass under way: whether it raised NUDGE_OUT (Nudges::raised).
  dRaised,
  \* A local of the pass under way: whether one of its nudges reached a waiter (Nudges::sent).
  dSent,
  \* A local of the pass under way: permits it took for the front waiter and has not granted yet.
  dHand,
  \* A local of the drain: whether the pass found no waiter left, so QUEUED may come down.
  dClearQ,
  \* A local of the drain: whether no waiter was counted owed, so HANDOFF may come down.
  dClearH,
  \* Where each task is in its operation: its "program counter" (see PCs below).
  pc,
  \* Where a task goes on once the drain it runs is over, per task.
  ret,
  \* Model bookkeeping (a "ghost"): how many counted tries of each task's current acquire failed
  \* while its node still waited, that is, how often it was really passed over, whatever the
  \* program's own counter says. The promise BoundedBypass reads it.
  lost,
  \* Model bookkeeping: TRUE once somebody barged while a handoff was owed. The promise
  \* HandoffExclusive reads it.
  bargedOwed,
  \* Model bookkeeping (a "ghost"): how many passes the drain under way has run, counted up to
  \* PassMax. 0 while nobody holds the drain role.
  dPasses,
  \* Model bookkeeping (a "ghost"): whether the drain under way (or the next one) has a pass to
  \* run that something earned. A drain's first pass is earned by whoever took the role. After
  \* that, a pass is earned by a waiter operation that lands on the drain, or by a release that
  \* lands on it and finds a node queued. Beginning a pass uses it up.
  dEarned,
  \* Model bookkeeping (a "ghost"): TRUE once some drain began a pass nothing had earned. The
  \* promise DrainBounded reads it.
  overran,
  \* Model bookkeeping: how many times each task with a limited number of turns has asked for the
  \* lock so far.
  turns

\* The flag bits and the permit count: the state word.
word == <<avail, drainOn, dirty, queued, nudgeOut, handoff, wakeFront>>
\* The fields of every waiter node.
nodes == <<phase, nudged, owedMark>>
\* The locals of a waiting future.
futs == <<bypass, marked, took, seenH, passOn, spinning>>
\* The locals of the pass under way.
dloc == <<dWF, dBudget, dPos, dFirst, dRaised, dSent, dHand, dClearQ, dClearH>>
\* What the tasks hold.
guards == <<got, held, owner, depth>>
\* The ghosts that watch a drain's passes.
tally == <<dPasses, dEarned, overran>>
\* Every variable, so a step that changes none of them is a stutter.
vars == <<word, owedCount, arrivals, waiting, nodes, wake, wl, guards, futs, dloc, pc, ret, lost,
          bargedOwed, turns, tally>>

\* The steps of the drain, where a task holds the drain role.
DrainPCs == {"d0", "dh", "dg", "dgb", "dho", "dnb", "dni", "dnt", "dnf", "dend", "dex"}

\* The places a task can be. "idle": between operations. "holding": it has a guard. "owned": it has
\* a guard another task moved to it. "parked": its future returned Pending and sleeps. The others
\* are instructions inside one poll, one release, one cancellation or one drain, named where they
\* are used.
PCs == {"idle", "holding", "owned", "parked", "retake", "pspin", "push", "enqt", "park0", "repark",
        "nspin", "gcons", "ncons", "try", "owe1", "owe2", "owe3", "owe4", "owe5", "owe6", "owe7",
        "wd1", "wd2", "gb1", "gb2", "gb3", "relt"} \cup DrainPCs

----------------------------------------------------------------------------
\* Helpers.

\* The sum of f[x] over a finite set S.
RECURSIVE SumOf(_, _)
\* The sum over the empty set is 0.
SumOf(f, S) == IF S = {} THEN 0
               \* Otherwise pick any element x, and add f[x] to the sum over the rest.
               ELSE LET x == CHOOSE y \in S : TRUE IN f[x] + SumOf(f, S \ {x})

\* The elements of a sequence, as a set.
Range(s) == {s[i] : i \in 1..Len(s)}

\* Sequence s without element x.
Remove(s, x) == SelectSeq(s, LAMBDA y : y # x)

\* Sequence s without the nodes that have withdrawn.
Alive(s) == SelectSeq(s, LAMBDA u : phase[u] # "cancelled")

\* The nodes of sequence s that have withdrawn.
Dead(s) == {u \in Range(s) : phase[u] = "cancelled"}

\* How many withdrawn nodes sequence s starts with.
RECURSIVE DeadRun(_)
\* None if s is empty or its first node has not withdrawn; else one more than for the rest.
DeadRun(s) == IF s # <<>> /\ phase[Head(s)] = "cancelled" THEN 1 + DeadRun(Tail(s)) ELSE 0

\* List s after List::front let go of every withdrawn node ahead of the first live one.
FrontOf(s) == SubSeq(s, DeadRun(s) + 1, Len(s))

\* The nodes List::front lets go of, for list s.
FrontOff(s) == {s[i] : i \in 1..DeadRun(s)}

\* The waiting list after List::front.
Pruned == FrontOf(waiting)

\* The nodes List::front lets go of, for the waiting list.
PrunedOff == FrontOff(waiting)

\* The state word has room for task t's request (Lock::admit for RawSemaphore: enough permits are
\* free). The HANDOFF bit is not looked at here.
Admit(t) == avail >= Need[t]

\* The compare-and-swap a fresh caller makes succeeds (contend::take with for_waiter false): the
\* permits are free and no handoff is owed. Waiters do not matter: barging. (Mutant IgnoreOwed: the
\* HANDOFF bit does not matter either.)
CanBarge(t) == Admit(t) /\ (~handoff \/ Mutant = "IgnoreOwed")

\* A release that saw this state must run a drain pass (wants_drain, src/sync/queue.rs): waiters
\* are queued, and no nudge is already on its way or a handoff is owed. (Mutant ReleaseNoWake:
\* never.)
WantsDrain == queued /\ (~nudgeOut \/ handoff) /\ Mutant # "ReleaseNoWake"

\* Task t has lost Bound counted races and still waits: it is owed a handoff.
AtBound(t) == phase[t] = "waiting" /\ lost[t] >= Bound

\* Task t may ask for the lock once more: it has a turn left, and the task it comes after, if any,
\* has asked and is past pushing its node.
HasTurn(t) ==
  \* It has a turn left,
  /\ Turns[t] = 0 \/ turns[t] < Turns[t]
  \* and it comes after nobody, or that task has asked and has queued or got the lock.
  /\ IF Follows[t] = 0 THEN TRUE
     \* (the task it follows: one turn used, and past the two steps that queue its node)
     ELSE turns[Follows[t]] >= 1 /\ pc[Follows[t]] \notin {"push", "enqt"}

\* Task t uses one of its turns (nothing is counted for a task with no limit).
TakeTurn(t) == turns' = [turns EXCEPT ![t] = IF Turns[t] = 0 THEN 0 ELSE @ + 1]

\* Reentrant: t's Owner holds the lock, so t enters again at once.
CanReenter(t) == Kind = "Reentrant" /\ owner = Owner[t] /\ held[t] < MaxDepth

\* Task t's acquire is complete: the permits it has in hand become its guard. A Semaphore's guard
\* holds them; a ReentrantMutex notes its owner and a depth of 1 (ReentrantMutex::enter).
\* `inHand` is how many permits it has for this acquire.
Guard(t, inHand) ==
  \* The task has nothing pending any more.
  /\ got' = [got EXCEPT ![t] = 0]
  \* A semaphore's guard holds the permits; a reentrant lock's task holds one more guard.
  /\ held' = [held EXCEPT ![t] = IF Kind = "Semaphore" THEN inHand ELSE @ + 1]
  \* A reentrant lock notes whose it is now,
  /\ owner' = IF Kind = "Reentrant" THEN Owner[t] ELSE owner
  \* with one guard out.
  /\ depth' = IF Kind = "Reentrant" THEN 1 ELSE depth

\* A state change that must be seen by a drain pass (Step::Drain in Policy::transition): in the
\* same compare-and-swap the task claims the drain role, or, when another thread holds it, raises
\* DIRTY and goes on to `r`. A task that claimed the role runs the drain, then goes on to `r`.
\* (Mutant NoDirty: a task that finds the role taken just goes on.)
Transit(t, r) ==
  \* Is the drain role taken?
  IF drainOn
    \* Yes: raise DIRTY, so the holder passes again,
    THEN /\ dirty' = (dirty \/ Mutant # "NoDirty")
         \* and go on.
         /\ pc' = [pc EXCEPT ![t] = r]
         \* The role stays where it is, and the task remembers nothing.
         /\ drainOn' = drainOn /\ ret' = [ret EXCEPT ![t] = "idle"]
         \* The ghost: this waiter operation landed on the drain under way, which has earned one
         \* more pass.
         /\ dEarned' = TRUE
    \* No: take the role,
    ELSE /\ drainOn' = TRUE
         \* start a pass,
         /\ pc' = [pc EXCEPT ![t] = "d0"]
         \* and remember where to go on afterwards.
         /\ ret' = [ret EXCEPT ![t] = r]
         \* DIRTY is as it was.
         /\ dirty' = dirty
         \* The ghost: the task's own drain has its first pass to run.
         /\ dEarned' = dEarned

\* A release's request for a pass (Step::Wake in Policy::transition): WAKE_FRONT goes up, and in
\* the same compare-and-swap the task claims the drain role, or, when another thread holds it,
\* leaves the request with that thread and goes on to `r`, raising no DIRTY. A release changes
\* what a waiter could take, not who waits, so the holder needs to pass again for it only if its
\* pass found somebody queued (see DEx). (Mutant ReleaseDirties: it raises DIRTY too, as the code
\* did before the drain was bounded.)
Asks(t, r) ==
  \* WAKE_FRONT goes up: a pass nudges the front whatever it asks for.
  /\ wakeFront' = TRUE
  \* Is the drain role taken?
  /\ IF drainOn
       \* Yes: the request stays with the holder (the bug: and DIRTY goes up),
       THEN /\ dirty' = (dirty \/ Mutant = "ReleaseDirties")
            \* and the task goes on.
            /\ pc' = [pc EXCEPT ![t] = r]
            \* The role stays where it is, and the task remembers nothing.
            /\ drainOn' = drainOn /\ ret' = [ret EXCEPT ![t] = "idle"]
            \* The ghost: if a node is queued (on the arrival stack or in the list), this release
            \* has somebody to tell, and the drain under way has earned one more pass. Over an
            \* empty queue it has earned nothing.
            /\ dEarned' = (dEarned \/ arrivals \o waiting # <<>>)
       \* No: take the role,
       ELSE /\ drainOn' = TRUE
            \* start a pass,
            /\ pc' = [pc EXCEPT ![t] = "d0"]
            \* and remember where to go on afterwards.
            /\ ret' = [ret EXCEPT ![t] = r]
            \* DIRTY is as it was.
            /\ dirty' = dirty
            \* The ghost: the task's own drain has its first pass to run.
            /\ dEarned' = dEarned

\* Where a task whose poll or cancellation is over goes: it holds a guard if it has permits in
\* hand, else it is idle.
After(inHand) == IF inHand > 0 THEN "holding" ELSE "idle"

\* The task's acquire is over, with `inHand` permits: a guard if it has some, else nothing.
Settle(t, inHand) ==
  \* With permits in hand they become its guard;
  IF inHand > 0 THEN Guard(t, inHand)
  \* with none, it has nothing pending and its guards are as they were.
  ELSE got' = [got EXCEPT ![t] = 0] /\ UNCHANGED <<held, owner, depth>>

\* The future of task t is done with this acquire: its locals are forgotten.
Forget(t) ==
  \* Its count of lost races,
  /\ bypass' = [bypass EXCEPT ![t] = 0]
  \* and the ghost count.
  /\ lost' = [lost EXCEPT ![t] = 0]

----------------------------------------------------------------------------
\* Acquiring: Contend::poll, src/sync/contend.rs.

\* lock() / acquire(n) / lock(owner), first poll, and try_lock / try_acquire: one compare-and-swap
\* takes the permits if they are free and no handoff is owed, barging past any waiters
\* (Lock::try_take). If it fails, a task that waits goes on to queue; a task that only tries stays
\* where it is.
Acquire(t) ==
  \* The task is between operations.
  /\ pc[t] = "idle"
  \* No node of an earlier wait of this task is still linked.
  /\ phase[t] = "none"
  \* It is not simply entering a reentrant lock its owner already holds.
  /\ ~CanReenter(t)
  \* It has a turn left, and this one is counted.
  /\ HasTurn(t) /\ TakeTurn(t)
  \* Are the permits it asks for free, with no handoff owed?
  /\ IF CanBarge(t)
       \* Yes: they leave the state word,
       THEN /\ avail' = avail - Need[t]
            \* and are its guard at once.
            /\ Guard(t, Need[t])
            \* If a handoff was owed at this moment, that is recorded.
            /\ bargedOwed' = (bargedOwed \/ handoff)
            \* It holds.
            /\ pc' = [pc EXCEPT ![t] = "holding"]
       \* No: only a task that waits goes on,
       ELSE /\ t \in Waits
            \* to the attempt its out-of-line path makes next, then the spin, and only then to
            \* pushing its node. (Losing this first attempt is no bypass: nobody has passed the
            \* caller yet, the holder was simply there first.)
            /\ pc' = [pc EXCEPT ![t] = "retake"]
            \* Nothing is taken.
            /\ UNCHANGED <<avail, guards, bargedOwed>>
  \* Nothing else changes.
  /\ UNCHANGED <<drainOn, dirty, queued, nudgeOut, handoff, wakeFront, owedCount, arrivals, waiting,
                 nodes, wake, wl, bypass, marked, took, seenH, passOn, spinning, dloc, ret, lost, tally>>

\* Reentrant: a task whose Owner holds the lock enters again at once, idle or holding
\* (ReentrantMutex::reenter). Nothing barges: the owner is already inside.
Reenter(t) ==
  \* The task is idle or holding.
  /\ pc[t] \in {"idle", "holding"}
  \* Its owner holds the lock, and it may nest one more guard.
  /\ CanReenter(t)
  \* It has a turn left, and this one is counted.
  /\ HasTurn(t) /\ TakeTurn(t)
  \* It holds one more guard.
  /\ held' = [held EXCEPT ![t] = @ + 1]
  \* One more guard is out.
  /\ depth' = depth + 1
  \* It holds.
  /\ pc' = [pc EXCEPT ![t] = "holding"]
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, nodes, wake, wl, got, owner, futs, dloc, ret,
                 lost, bargedOwed, tally>>

\* The attempt the out-of-line path makes first (Contend::poll_from, contend::take_from): the same
\* compare-and-swap as Acquire's, from the state the inline attempt saw. A Mutex's lock() made its
\* first attempt inline (RawSemaphore::take_one); this is its second, just after, which a free
\* permit with a queue bit up can still win. A Semaphore or ReentrantMutex acquire has no inline
\* attempt, so for them this step is one attempt more than the code makes: the model lets them
\* barge in one more way than they can, which only widens what is checked.
Retake(t) ==
  \* The task's first attempt failed.
  /\ pc[t] = "retake"
  \* Are the permits free, with no handoff owed?
  /\ IF CanBarge(t)
       \* Yes: they leave the state word,
       THEN /\ avail' = avail - Need[t]
            \* and are its guard at once.
            /\ Guard(t, Need[t])
            \* If a handoff was owed at this moment, that is recorded.
            /\ bargedOwed' = (bargedOwed \/ handoff)
            \* It holds.
            /\ pc' = [pc EXCEPT ![t] = "holding"]
       \* No: it goes on to spin.
       ELSE /\ pc' = [pc EXCEPT ![t] = "pspin"]
            \* Nothing is taken.
            /\ UNCHANGED <<avail, guards, bargedOwed>>
  \* Nothing else changes.
  /\ UNCHANGED <<drainOn, dirty, queued, nudgeOut, handoff, wakeFront, owedCount, arrivals, waiting,
                 nodes, wake, wl, futs, dloc, ret, lost, turns, tally>>

\* Whether a spinner that reads this state word stops and queues (queue::stops_spin): a handoff is
\* owed, or waiters are queued and nobody holds the drain role. A queue whose role is taken may be
\* emptying (a waiter that won its retry is leaving it), so the spinner looks again then.
\* (Mutant SpinIgnoresOwed: the spin does not stop for an owed handoff.)
StopsSpin == (handoff /\ Mutant # "SpinIgnoresOwed") \/ (queued /\ ~drainOn)

\* One round of the spin before queueing (contend::spin_take): after a pause, one load of the state
\* word; when it shows the permits free and the spin need not stop, one compare-and-swap from exactly
\* the state loaded. That swap succeeds only if nothing changed since the load, so the load and a
\* swap that succeeds are one step here, at the instant of the swap; a swap that fails is a round
\* that saw nothing, and changes nothing. A round that sees the lock taken changes nothing either,
\* so it is no step at all. (Mutant SpinIgnoresOwed: the spin takes the permits past an owed
\* handoff, as a swap that never looked at HANDOFF would.)
PreSpin(t) ==
  \* The task is spinning before it queues.
  /\ pc[t] = "pspin"
  \* Must it stop?
  /\ IF StopsSpin
       \* Yes: it goes on to push its node,
       THEN /\ pc' = [pc EXCEPT ![t] = "push"]
            \* taking nothing.
            /\ UNCHANGED <<avail, guards, bargedOwed>>
       \* No: the round takes the permits if they are free.
       ELSE /\ Admit(t)
            \* They leave the state word,
            /\ avail' = avail - Need[t]
            \* and are its guard at once.
            /\ Guard(t, Need[t])
            \* If a handoff was owed at this moment (only under the bug), that is recorded.
            /\ bargedOwed' = (bargedOwed \/ handoff)
            \* It holds.
            /\ pc' = [pc EXCEPT ![t] = "holding"]
  \* Nothing else changes.
  /\ UNCHANGED <<drainOn, dirty, queued, nudgeOut, handoff, wakeFront, owedCount, arrivals, waiting,
                 nodes, wake, wl, futs, dloc, ret, lost, turns, tally>>

\* The spin before queueing runs out of rounds (the lock's budget, spin::Budget): the task goes on
\* to push its node. How many rounds a spin has is a count the lock adapts, so here the spin may
\* end after any number of rounds, none included.
PreSpinOut(t) ==
  \* The task is spinning before it queues.
  /\ pc[t] = "pspin"
  \* It goes on to push its node.
  /\ pc' = [pc EXCEPT ![t] = "push"]
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, nodes, wake, wl, guards, futs, dloc, ret, lost,
                 bargedOwed, turns, tally>>

\* The waiter arms a fresh node and pushes it on the arrival stack (Wait::enqueue, then
\* WaitQueue::push: one compare-and-swap on the stack).
Push(t) ==
  \* The task's first attempt failed and it is about to push.
  /\ pc[t] = "push"
  \* Its node joins the arrivals, behind the ones already there.
  /\ arrivals' = Append(arrivals, t)
  \* The node is waiting.
  /\ phase' = [phase EXCEPT ![t] = "waiting"]
  \* Nobody has nudged it.
  /\ nudged' = [nudged EXCEPT ![t] = FALSE]
  \* It is not marked owed.
  /\ owedMark' = [owedMark EXCEPT ![t] = FALSE]
  \* Next it publishes the node.
  /\ pc' = [pc EXCEPT ![t] = "enqt"]
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, waiting, wake, wl, guards, futs, dloc, ret, lost, bargedOwed, turns, tally>>

\* The waiter publishes its node: it raises QUEUED and makes sure a pass sees it (Policy::enqueue:
\* a transition with Step::Drain).
Publish(t) ==
  \* The task has pushed its node.
  /\ pc[t] = "enqt"
  \* QUEUED is up: a release now knows somebody waits.
  /\ queued' = TRUE
  \* A pass will see the node: this task's own, or the one under way. Then it looks at its node.
  /\ Transit(t, "park0")
  \* Nothing else changes.
  /\ UNCHANGED <<avail, nudgeOut, handoff, wakeFront, owedCount, arrivals, waiting, nodes, wake, wl,
                 guards, futs, dloc, lost, bargedOwed, turns, dPasses, overran>>

\* The future looks at its node (Waiter::park: one compare-and-swap that reads the two marks).
\* Granted: it lets the node go and goes on to finish. Nudged: it takes the nudge and goes on to
\* try again. Neither: it sleeps, and a later mark will wake it.
Look(t) ==
  \* A pass granted it what it asked for:
  IF phase[t] = "granted"
    \* the future lets its node go,
    THEN /\ phase' = [phase EXCEPT ![t] = "none"]
         \* with whatever nudge was on it,
         /\ nudged' = [nudged EXCEPT ![t] = FALSE]
         \* and goes on to take back the nudge marker.
         /\ pc' = [pc EXCEPT ![t] = "gcons"]
    \* A pass nudged it:
    ELSE IF nudged[t]
      \* the nudge is taken,
      THEN /\ nudged' = [nudged EXCEPT ![t] = FALSE]
           \* the node stays queued,
           /\ phase' = phase
           \* and the future goes on to take back the nudge marker.
           /\ pc' = [pc EXCEPT ![t] = "ncons"]
      \* Neither: the future returns Pending,
      ELSE /\ pc' = [pc EXCEPT ![t] = "parked"]
           \* with its node as it was.
           /\ UNCHANGED <<phase, nudged>>

\* Right after publishing, the future settles its node (Wait::park with no waker): a grant or a
\* nudge that came while it was queueing set only a mark, and this look reads it. Mutant NoRecheck:
\* it sleeps without looking.
Park0(t) ==
  \* The task has published its node.
  /\ pc[t] = "park0"
  \* Under the bug NoRecheck
  /\ IF Mutant = "NoRecheck"
       \* it goes straight to sleep,
       THEN /\ pc' = [pc EXCEPT ![t] = "parked"]
            \* reading nothing.
            /\ UNCHANGED <<phase, nudged>>
       \* As built, it looks at its node.
       ELSE Look(t)
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, owedMark, wake, wl, guards, futs, dloc, ret,
                 lost, bargedOwed, turns, tally>>

\* After a failed try the future looks at its node at once (Wait::spin_park with a budget of zero,
\* or a node marked already, which Waiter::begin_spin reports: then Wait::park with the waker, which
\* registers the waker and reads the marks in one compare-and-swap): a mark set meanwhile is read
\* here.
Repark(t) ==
  \* The task's try failed.
  /\ pc[t] = "repark"
  \* It looks at its node.
  /\ Look(t)
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, owedMark, wake, wl, guards, futs, dloc, ret,
                 lost, bargedOwed, turns, tally>>

\* After a failed try the future spins on its own node instead (Wait::spin_park): Waiter::begin_spin
\* found nothing marked and took the node's ARMED bit down, so from now on a release that nudges or
\* grants this node finds no waker to claim and only sets the mark (in Nudge and DG below, a task
\* that is not "parked" goes on no wake list). The future stays inside its poll.
SpinStart(t) ==
  \* The task's try failed.
  /\ pc[t] = "repark"
  \* Nothing has been granted or nudged yet (begin_spin's load).
  /\ phase[t] = "waiting" /\ ~nudged[t]
  \* The future spins: its node is unarmed.
  /\ spinning' = [spinning EXCEPT ![t] = TRUE]
  \* It goes on spinning.
  /\ pc' = [pc EXCEPT ![t] = "nspin"]
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, nodes, wake, wl, guards, bypass, marked, took,
                 seenH, passOn, dloc, ret, lost, bargedOwed, turns, tally>>

\* The spin on the node ends: it saw a mark (Waiter::marked), or its rounds ran out. Either way the
\* future then looks at its node the way Repark does (Wait::park with the waker): it arms the waker
\* and reads the marks in the same compare-and-swap, so a nudge that came while it spun, even one
\* that landed just as it gave up, is read now, and the future retries instead of sleeping beside
\* it. (Mutant SpinNoRecheck: a spin that runs out arms the waker and sleeps without reading the
\* marks; a release that nudged it while it spun called no waker, because it was spinning, so the
\* waiter now sleeps beside a nudge nobody will ever wake it for.)
SpinEnd(t) ==
  \* The task is spinning on its node.
  /\ pc[t] = "nspin"
  \* The spin is over: the node is armed again in this step.
  /\ spinning' = [spinning EXCEPT ![t] = FALSE]
  \* As built it looks at its node; under the bug it may also go to sleep without looking.
  /\ \/ Look(t)
     \* (only under the bug, the other way out:)
     \/ /\ Mutant = "SpinNoRecheck"
        \* (the bug: straight to sleep,)
        /\ pc' = [pc EXCEPT ![t] = "parked"]
        \* (reading nothing.)
        /\ UNCHANGED <<phase, nudged>>
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, owedMark, wake, wl, guards, bypass, marked,
                 took, seenH, passOn, dloc, ret, lost, bargedOwed, turns, tally>>

\* The executor polls a future whose waker was called. The future looks at its node.
Poll(t) ==
  \* The task sleeps.
  /\ pc[t] = "parked"
  \* Its waker was called.
  /\ wake[t]
  \* That call is used up.
  /\ wake' = [wake EXCEPT ![t] = FALSE]
  \* It looks at its node.
  /\ Look(t)
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, owedMark, wl, guards, bypass, marked, took,
                 seenH, passOn, spinning, dloc, ret, lost, bargedOwed, turns, tally>>

\* A granted future takes back the nudge marker (WaitQueue::consume_nudge: one fetch-and), and its
\* acquire is complete: the granted permits are its guard.
GrantCons(t) ==
  \* The task found its node granted.
  /\ pc[t] = "gcons"
  \* NUDGE_OUT comes down: a nudge may have been on its way when the grant came.
  /\ nudgeOut' = FALSE
  \* The permits the pass took for it become its guard.
  /\ Guard(t, got[t])
  \* Its locals are forgotten.
  /\ Forget(t)
  \* It holds.
  /\ pc' = [pc EXCEPT ![t] = "holding"]
  \* Nothing else changes.
  /\ UNCHANGED <<avail, drainOn, dirty, queued, handoff, wakeFront, owedCount, arrivals, waiting,
                 nodes, wake, wl, marked, took, seenH, passOn, spinning, dloc, ret, bargedOwed, turns, tally>>

\* A nudged future takes back the nudge marker (one fetch-and), then chooses its try: the one that
\* would be its Bound-th counted loss is Lock::take_or_owe, every other is Lock::try_take. That
\* holds in the poll that queued the waiter too: a nudge means something was freed after it
\* queued, and the nudge may come from the waiter's own drain. (Mutant UnboundedBarging: always
\* try_take, so HANDOFF is never raised.)
NudgeCons(t) ==
  \* The task found its node nudged.
  /\ pc[t] = "ncons"
  \* NUDGE_OUT comes down: this waiter has polled.
  /\ nudgeOut' = FALSE
  \* Is this the try that would bring the count to Bound?
  /\ pc' = [pc EXCEPT ![t] = IF bypass[t] + 1 = Bound /\ Mutant # "UnboundedBarging"
                               \* Then it is take_or_owe; otherwise a plain try.
                               THEN "owe1" ELSE "try"]
  \* Nothing else changes.
  /\ UNCHANGED <<avail, drainOn, dirty, queued, handoff, wakeFront, owedCount, arrivals, waiting,
                 nodes, wake, wl, guards, futs, dloc, ret, lost, bargedOwed, turns, tally>>

\* The plain try (Lock::try_take): the same compare-and-swap as a fresh caller's. Winning, the
\* future goes on to withdraw its node. Losing counts one bypass, unless HANDOFF was up, and never
\* past Bound; then the future looks at its node again. Mutant LoseQueuePosition: a waiter that
\* loses starts over at the tail with a fresh count.
Try(t) ==
  \* The task is about to try.
  /\ pc[t] = "try"
  \* Are the permits free, with no handoff owed?
  /\ IF CanBarge(t)
       \* Yes: they leave the state word
       THEN /\ avail' = avail - Need[t]
            \* into the future's hand.
            /\ got' = [got EXCEPT ![t] = @ + Need[t]]
            \* If a handoff was owed at this moment, that is recorded.
            /\ bargedOwed' = (bargedOwed \/ handoff)
            \* Next it withdraws its node.
            /\ pc' = [pc EXCEPT ![t] = "wd1"]
            \* Nothing is counted.
            /\ UNCHANGED <<bypass, lost, waiting>>
       \* No: is it a counted loss (no handoff seen)?
       ELSE /\ LET counted == ~handoff
                   \* Under the bug LoseQueuePosition a counted loss makes the waiter start over.
                   startsOver == counted /\ Mutant = "LoseQueuePosition"
               \* The ghost counts it if the node still waits (a waiter that a pass granted
               \* meanwhile only lost to the permits taken for itself).
               IN /\ lost' = [lost EXCEPT ![t] = IF counted /\ phase[t] = "waiting" THEN @ + 1 ELSE @]
                  \* The future's own count goes up by one, but never past Bound (the bug: back to
                  \* zero). At Bound it stays: a waiter that is owed, was granted, and retries on a
                  \* nudge left over from before loses to the permits taken for itself.
                  /\ bypass' = [bypass EXCEPT ![t] = IF startsOver THEN 0
                                                    \* (a counted loss below the bound)
                                                    ELSE IF counted /\ @ < Bound THEN @ + 1 ELSE @]
                  \* The node keeps its place (the bug: it goes to the tail).
                  /\ waiting' = IF startsOver /\ t \in Range(waiting)
                                  \* Under the bug: out of its place, and back in at the end.
                                  THEN Append(Remove(waiting, t), t)
                                  \* As built: the list is untouched.
                                  ELSE waiting
            \* Next it looks at its node again.
            /\ pc' = [pc EXCEPT ![t] = "repark"]
            \* Nothing is taken.
            /\ UNCHANGED <<avail, got, bargedOwed, turns, tally>>
  \* Nothing else changes.
  /\ UNCHANGED <<drainOn, dirty, queued, nudgeOut, handoff, wakeFront, owedCount, arrivals, nodes,
                 wake, wl, held, owner, depth, marked, took, seenH, passOn, spinning, dloc, ret, turns, tally>>

\* take_or_owe, first instruction (Policy::owe_or_take): the waiter counts itself owed, before its
\* node is marked and before the bit is raised, so a pass that sees the bit sees the count too.
Owe1(t) ==
  \* The task is about to make the try that would be its Bound-th loss.
  /\ pc[t] = "owe1"
  \* One more waiter is counted owed.
  /\ owedCount' = owedCount + 1
  \* Next it marks its node.
  /\ pc' = [pc EXCEPT ![t] = "owe2"]
  \* Nothing else changes.
  /\ UNCHANGED <<word, arrivals, waiting, nodes, wake, wl, guards, futs, dloc, ret, lost, bargedOwed, turns, tally>>

\* take_or_owe, second instruction (Waiter::owe): mark the node owed, if it still waits. A node a
\* pass granted meanwhile takes no mark.
Owe2(t) ==
  \* The task has counted itself.
  /\ pc[t] = "owe2"
  \* Does its node still wait?
  /\ IF phase[t] = "waiting"
       \* Yes: the node is marked,
       THEN /\ owedMark' = [owedMark EXCEPT ![t] = TRUE]
            \* and the future remembers that.
            /\ marked' = [marked EXCEPT ![t] = TRUE]
       \* No, it was granted: no mark,
       ELSE /\ marked' = [marked EXCEPT ![t] = FALSE]
            \* on the node either.
            /\ UNCHANGED owedMark
  \* Next comes the compare-and-swap.
  /\ pc' = [pc EXCEPT ![t] = "owe3"]
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, phase, nudged, wake, wl, guards, bypass,
                 took, seenH, passOn, spinning, dloc, ret, lost, bargedOwed, turns, tally>>

\* take_or_owe, the compare-and-swap. A marked waiter, in one indivisible step: leaves a state
\* that already has HANDOFF up alone (the loss will not count); or takes the permits if they fit;
\* or raises HANDOFF and makes sure a pass sees it. So no release can fall between "the waiter
\* found the lock taken for the Bound-th time" and "barging is shut". A waiter whose node took no
\* mark (it is granted already) only reads the state.
Owe3(t) ==
  \* The task has tried to mark its node.
  /\ pc[t] = "owe3"
  \* The node took no mark (it was granted already):
  /\ IF ~marked[t]
       \* the future remembers whether HANDOFF was up,
       THEN /\ seenH' = [seenH EXCEPT ![t] = handoff]
            \* and goes on to take its count back.
            /\ pc' = [pc EXCEPT ![t] = "owe5"]
            \* Nothing else of the try changes.
            /\ UNCHANGED <<avail, drainOn, dirty, handoff, got, took, bypass, lost, ret, bargedOwed, turns, dEarned>>
       \* HANDOFF is already up, for another waiter:
       ELSE IF handoff
         \* the future remembers that,
         THEN /\ seenH' = [seenH EXCEPT ![t] = TRUE]
              \* and goes on to take its mark back.
              /\ pc' = [pc EXCEPT ![t] = "owe4"]
              \* Nothing else of the try changes.
              /\ UNCHANGED <<avail, drainOn, dirty, handoff, got, took, bypass, lost, ret, bargedOwed, turns, dEarned>>
       \* The permits are free:
       ELSE IF Admit(t)
         \* they leave the state word
         THEN /\ avail' = avail - Need[t]
              \* into the future's hand,
              /\ got' = [got EXCEPT ![t] = @ + Need[t]]
              \* which remembers that it took them.
              /\ took' = [took EXCEPT ![t] = TRUE]
              \* It goes on to take its mark back.
              /\ pc' = [pc EXCEPT ![t] = "owe4"]
              \* Nothing else of the try changes (HANDOFF was down, so nothing was barged).
              /\ UNCHANGED <<drainOn, dirty, handoff, seenH, bypass, lost, ret, bargedOwed, turns, dEarned>>
         \* The lock is taken: HANDOFF goes up, in this same compare-and-swap.
         ELSE /\ handoff' = TRUE
              \* This is the waiter's Bound-th counted loss (the ghost counts it if the node still
              \* waits),
              /\ lost' = [lost EXCEPT ![t] = IF phase[t] = "waiting" THEN @ + 1 ELSE @]
              \* and its own count says so too.
              /\ bypass' = [bypass EXCEPT ![t] = @ + 1]
              \* A pass will see the bit: this task's own, or the one under way. Then it looks at
              \* its node again.
              /\ Transit(t, "repark")
              \* The mark stays on the node; nothing is taken.
              /\ UNCHANGED <<avail, got, took, seenH, bargedOwed, turns>>
  \* Nothing else changes.
  /\ UNCHANGED <<queued, nudgeOut, wakeFront, owedCount, arrivals, waiting, nodes, wake, wl, held,
                 owner, depth, marked, passOn, spinning, dloc, turns, dPasses, overran>>

\* take_or_owe, taking the mark back (Waiter::unowe): the waiter took the permits itself, or found
\* a handoff already owed to another, so it is not owed after all. If a pass granted the node
\* meanwhile, the pass settled the count as it let the node go, and the future must not settle it
\* again.
Owe4(t) ==
  \* The task is about to take its mark back.
  /\ pc[t] = "owe4"
  \* Does the node still wait with its mark?
  /\ IF phase[t] = "waiting" /\ owedMark[t]
       \* Yes: the mark comes off,
       THEN /\ owedMark' = [owedMark EXCEPT ![t] = FALSE]
            \* and the future goes on to take its count back.
            /\ pc' = [pc EXCEPT ![t] = "owe5"]
       \* No: the queue settled it,
       ELSE /\ pc' = [pc EXCEPT ![t] = "owe6"]
            \* and the node is left alone.
            /\ UNCHANGED owedMark
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, phase, nudged, wake, wl, guards, futs, dloc,
                 ret, lost, bargedOwed, turns, tally>>

\* take_or_owe, taking the count back (WaitQueue::forgive: one fetch-sub).
Owe5(t) ==
  \* The task is about to take its count back.
  /\ pc[t] = "owe5"
  \* One waiter fewer is counted owed.
  /\ owedCount' = owedCount - 1
  \* Next it reads the state once more.
  /\ pc' = [pc EXCEPT ![t] = "owe6"]
  \* Nothing else changes.
  /\ UNCHANGED <<word, arrivals, waiting, nodes, wake, wl, guards, futs, dloc, ret, lost, bargedOwed, turns, tally>>

\* How take_or_owe ends for task t: with the permits, it goes on to withdraw its node; without,
\* the loss counts unless HANDOFF was up, and it looks at its node again. The values are the
\* future's own, so they change in the same step as its last shared instruction.
OweEnds(t) ==
  \* A loss that saw no handoff is counted by the future. (Only a waiter whose node was granted
  \* already ends here with such a loss, so the ghost, which counts real bypasses, does not move.)
  /\ bypass' = [bypass EXCEPT ![t] = IF ~took[t] /\ ~seenH[t] /\ @ < Bound THEN @ + 1 ELSE @]
  \* The three locals of the try are forgotten.
  /\ marked' = [marked EXCEPT ![t] = FALSE] /\ took' = [took EXCEPT ![t] = FALSE]
  \* (the third)
  /\ seenH' = [seenH EXCEPT ![t] = FALSE]

\* Where take_or_owe goes on: to withdraw the node if it took the permits, else to look at it.
OweNext(t) == IF took[t] THEN "wd1" ELSE "repark"

\* take_or_owe, the last read (a fetch-or of nothing, so it is ordered against a drain's exit): a
\* pass that counted this waiter while it was deciding kept HANDOFF up for it. If the bit is up,
\* one more pass must decide whether it stays.
Owe6(t) ==
  \* The task has settled its mark and its count.
  /\ pc[t] = "owe6"
  \* Is HANDOFF up?
  /\ IF handoff
       \* Yes: it goes on to ask for a pass,
       THEN /\ pc' = [pc EXCEPT ![t] = "owe7"]
            \* keeping its locals for one more step.
            /\ UNCHANGED <<bypass, marked, took, seenH>>
       \* No: the try is over.
       ELSE /\ OweEnds(t)
            \* It goes on.
            /\ pc' = [pc EXCEPT ![t] = OweNext(t)]
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, nodes, wake, wl, guards, passOn, spinning, dloc,
                 ret, lost, bargedOwed, turns, tally>>

\* take_or_owe, asking for one more pass (a transition with Step::Drain that changes nothing
\* else).
Owe7(t) ==
  \* The task saw HANDOFF up after settling.
  /\ pc[t] = "owe7"
  \* The try is over.
  /\ OweEnds(t)
  \* A pass will look: this task's own, or the one under way. Then it goes on.
  /\ Transit(t, OweNext(t))
  \* Nothing else changes.
  /\ UNCHANGED <<avail, queued, nudgeOut, handoff, wakeFront, owedCount, arrivals, waiting, nodes,
                 wake, wl, guards, passOn, spinning, dloc, lost, bargedOwed, turns, dPasses, overran>>

----------------------------------------------------------------------------
\* Leaving the queue: Contend::cancel, src/sync/contend.rs.

\* The future leaves its node (Wait::cancel, Waiter::withdraw: one compare-and-swap). Still
\* waiting: the node becomes "cancelled" and the future notes whether a nudge sat on it unused.
\* Granted first: the future lets the node go and will give the granted permits back.
\* `dropped` says the future is being dropped, not leaving because it took the permits itself.
\* Mutant CancelNoPassOn: a dropped future that was granted keeps the permits.
Withdraw(t, dropped) ==
  \* A pass granted the node first:
  IF phase[t] = "granted"
    \* the future lets the node go,
    THEN /\ phase' = [phase EXCEPT ![t] = "none"]
         \* with whatever nudge was on it.
         /\ nudged' = [nudged EXCEPT ![t] = FALSE]
         \* As built it goes on to give the permits back; under the bug a dropped future just stops.
         /\ pc' = [pc EXCEPT ![t] = IF dropped /\ Mutant = "CancelNoPassOn" THEN "idle" ELSE "gb1"]
         \* It carries no nudge to pass on.
         /\ UNCHANGED passOn
    \* The node still waits: it becomes "cancelled",
    ELSE /\ phase' = [phase EXCEPT ![t] = "cancelled"]
         \* the future notes whether it carried an unused nudge,
         /\ passOn' = [passOn EXCEPT ![t] = nudged[t]]
         \* which is gone from the node,
         /\ nudged' = [nudged EXCEPT ![t] = FALSE]
         \* and it goes on to tell the queue.
         /\ pc' = [pc EXCEPT ![t] = "wd2"]

\* A future that took the permits by its own try leaves the queue.
Wd1(t) ==
  \* The task's try succeeded.
  /\ pc[t] = "wd1"
  \* It leaves its node.
  /\ Withdraw(t, FALSE)
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, owedMark, wake, wl, guards, bypass,
                 marked, took, seenH, spinning, dloc, ret, lost, bargedOwed, turns, tally>>

\* The waiting future is dropped (the caller gave up): it leaves its node. A wake on its way to it
\* now has nobody to wake.
Cancel(t) ==
  \* The task's future may be dropped,
  /\ t \in Drops
  \* and it sleeps.
  /\ pc[t] = "parked"
  \* A call of its waker, made or about to be made, is void.
  /\ wake' = [wake EXCEPT ![t] = FALSE] /\ wl' = wl \ {t}
  \* Its locals are forgotten.
  /\ Forget(t)
  \* It leaves its node.
  /\ Withdraw(t, TRUE)
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, owedMark, guards, marked, took, seenH, spinning, dloc,
                 ret, bargedOwed, turns, tally>>

\* The withdrawn future tells the queue (Policy::withdrawn: a transition with Step::Drain): a
\* nudge marker that was for it is void, so NUDGE_OUT comes down; a nudge it carried unused passes
\* to the next front, through WAKE_FRONT; and a pass must look, to prune the node and reconsider
\* what it was blocking. With that its poll, or its drop, is over.
Wd2(t) ==
  \* The task withdrew its node.
  /\ pc[t] = "wd2"
  \* NUDGE_OUT comes down.
  /\ nudgeOut' = FALSE
  \* WAKE_FRONT goes up if the node carried an unused nudge.
  /\ wakeFront' = (wakeFront \/ passOn[t])
  \* That local is forgotten.
  /\ passOn' = [passOn EXCEPT ![t] = FALSE]
  \* Permits it took itself become its guard.
  /\ Settle(t, got[t])
  \* Its other locals are forgotten.
  /\ Forget(t)
  \* A pass will look: this task's own, or the one under way. Then it holds, or is idle.
  /\ Transit(t, After(got[t]))
  \* Nothing else changes.
  /\ UNCHANGED <<avail, queued, handoff, owedCount, arrivals, waiting, nodes, wake, wl, marked, took,
                 seenH, spinning, dloc, bargedOwed, turns, dPasses, overran>>

\* A future that leaves after a grant takes back the nudge marker (one fetch-and) before it gives
\* the granted permits back.
Gb1(t) ==
  \* The task found its node granted as it left.
  /\ pc[t] = "gb1"
  \* NUDGE_OUT comes down.
  /\ nudgeOut' = FALSE
  \* Next it gives the permits back.
  /\ pc' = [pc EXCEPT ![t] = "gb2"]
  \* Nothing else changes.
  /\ UNCHANGED <<avail, drainOn, dirty, queued, handoff, wakeFront, owedCount, arrivals, waiting,
                 nodes, wake, wl, guards, futs, dloc, ret, lost, bargedOwed, turns, tally>>

\* It gives the granted permits back (Lock::give_back, which is RawSemaphore::release: one add).
\* If waiters need telling it goes on to ask for a pass; otherwise its poll, or its drop, is over.
Gb2(t) ==
  \* The task took back the marker.
  /\ pc[t] = "gb2"
  \* The granted permits return to the state word.
  /\ avail' = avail + Need[t]
  \* Must a pass run?
  /\ IF WantsDrain
       \* Yes: the permits leave its hand, and it goes on to ask for the pass,
       THEN /\ got' = [got EXCEPT ![t] = @ - Need[t]]
            \* next.
            /\ pc' = [pc EXCEPT ![t] = "gb3"]
            \* Its guard and its locals wait for that step.
            /\ UNCHANGED <<held, owner, depth, bypass, lost>>
       \* No: permits it took itself, if any, become its guard,
       ELSE /\ Settle(t, got[t] - Need[t])
            \* its locals are forgotten,
            /\ Forget(t)
            \* and it holds, or is idle.
            /\ pc' = [pc EXCEPT ![t] = After(got[t] - Need[t])]
  \* Nothing else changes.
  /\ UNCHANGED <<drainOn, dirty, queued, nudgeOut, handoff, wakeFront, owedCount, arrivals, waiting,
                 nodes, wake, wl, marked, took, seenH, passOn, spinning, dloc, ret, bargedOwed, turns, tally>>

\* It asks for the pass (Policy::released: a transition with Step::Wake that raises WAKE_FRONT).
Gb3(t) ==
  \* The task gave permits back while waiters needed telling.
  /\ pc[t] = "gb3"
  \* Permits it took itself, if any, become its guard.
  /\ Settle(t, got[t])
  \* Its locals are forgotten.
  /\ Forget(t)
  \* It asks: its own pass, or a request left with the holder of the role. Then it holds, or is
  \* idle.
  /\ Asks(t, After(got[t]))
  \* Nothing else changes.
  /\ UNCHANGED <<avail, queued, nudgeOut, handoff, owedCount, arrivals, waiting, nodes, wake, wl,
                 marked, took, seenH, passOn, spinning, dloc, bargedOwed, turns, dPasses, overran>>

----------------------------------------------------------------------------
\* Releasing and moving owned guards.

\* A guard is dropped (RawSemaphore::release: one add to the state word). A Semaphore gives back
\* all the guard's permits; a ReentrantMutex drops one guard and gives its permit back when none
\* is left (ReentrantMutex::unlock; mutant NoDepth: at any depth). If waiters need telling, the
\* task goes on to ask for a pass.
Release(t) ==
  \* The task holds a guard.
  /\ pc[t] \in {"holding", "owned"}
  \* n: the guards it drops: one of a reentrant lock, the whole guard of a semaphore.
  /\ LET n    == IF Kind = "Reentrant" THEN 1 ELSE held[t]
         \* back: the permits that return to the state word.
         back == IF Kind = "Semaphore" THEN n
                 \* A reentrant lock returns its one permit with the last guard (the bug: always).
                 ELSE IF depth = 1 \/ Mutant = "NoDepth" THEN 1 ELSE 0
         \* next: it keeps holding if it has a guard left, else it is idle.
         next == IF held[t] - n > 0 THEN "holding" ELSE "idle"
     \* The task holds that much less.
     IN /\ held' = [held EXCEPT ![t] = @ - n]
        \* A reentrant lock has one guard fewer out.
        /\ depth' = IF Kind = "Reentrant" THEN depth - 1 ELSE depth
        \* A reentrant lock that returned its permit has no owner.
        /\ owner' = IF Kind = "Reentrant" /\ back = 1 THEN NoOwner ELSE owner
        \* The permits are back in the state word.
        /\ avail' = avail + back
        \* Did something return while waiters need telling?
        /\ IF back > 0 /\ WantsDrain
             \* Yes: it goes on to ask for a pass,
             THEN /\ pc' = [pc EXCEPT ![t] = "relt"]
                  \* and remembers where to go on after that.
                  /\ ret' = [ret EXCEPT ![t] = next]
             \* No: it goes on at once.
             ELSE /\ pc' = [pc EXCEPT ![t] = next]
                  \* It remembers nothing.
                  /\ UNCHANGED ret
  \* Nothing else changes.
  /\ UNCHANGED <<drainOn, dirty, queued, nudgeOut, handoff, wakeFront, owedCount, arrivals, waiting,
                 nodes, wake, wl, got, futs, dloc, lost, bargedOwed, turns, tally>>

\* The releasing task asks for the pass (Policy::released: a transition with Step::Wake that
\* raises WAKE_FRONT).
Released(t) ==
  \* The task put permits back while waiters needed telling.
  /\ pc[t] = "relt"
  \* It asks: its own pass, or a request left with the holder of the role. Then it goes on.
  /\ Asks(t, ret[t])
  \* Nothing else changes.
  /\ UNCHANGED <<avail, queued, nudgeOut, handoff, owedCount, arrivals, waiting, nodes, wake, wl,
                 guards, futs, dloc, lost, bargedOwed, turns, dPasses, overran>>

\* An owned guard (`lock_owned`, `acquire_owned`) moves from task t to an idle task u, which only
\* drops it.
Give(t, u) ==
  \* Only a semaphore with owned guards.
  /\ Kind = "Semaphore" /\ Owned
  \* Two different tasks.
  /\ t # u
  \* The giver holds.
  /\ pc[t] = "holding"
  \* The receiver is idle.
  /\ pc[u] = "idle"
  \* The permits move from the giver to the receiver.
  /\ held' = [held EXCEPT ![t] = 0, ![u] = held[t]]
  \* The giver is idle; the receiver holds an owned guard.
  /\ pc' = [pc EXCEPT ![t] = "idle", ![u] = "owned"]
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, nodes, wake, wl, got, owner, depth, futs, dloc,
                 ret, lost, bargedOwed, turns, tally>>

----------------------------------------------------------------------------
\* The drain: Policy::drain in src/sync/queue.rs, running RawSemaphore::pass. Only the task that
\* holds the drain role takes these steps.

\* The queue lets go of the nodes in G (release_queued, src/sync/waiter.rs): each is free for its
\* task to use again, and each one that was marked owed is taken out of the owed count
\* (WaitQueue::forgive).
LetGo(G) ==
  \* The nodes are free.
  /\ phase' = [u \in Tasks |-> IF u \in G THEN "none" ELSE phase[u]]
  \* Their owed marks are gone,
  /\ owedMark' = [u \in Tasks |-> IF u \in G THEN FALSE ELSE owedMark[u]]
  \* and so is their share of the owed count.
  /\ owedCount' = owedCount - Cardinality({u \in G : owedMark[u]})

\* A pass begins. One fetch-and brings DIRTY and WAKE_FRONT down, remembering whether WAKE_FRONT
\* was up. One swap takes every arrival; the pass appends the ones still waiting to its list and
\* lets go of the withdrawn ones (List::absorb). Then it may sweep the withdrawn nodes out of the
\* whole list (List::tidy), or not, and it lets go of the withdrawn nodes at the front
\* (List::front). With HANDOFF up it goes on to hand permits out in queue order; with HANDOFF down
\* the queue competes, and the pass goes on to read its nudging budget if a waiter is there.
D0(d) ==
  \* The task holds the drain role and starts a pass.
  /\ pc[d] = "d0"
  \* DIRTY comes down: what changed before this instant, this pass sees.
  /\ dirty' = FALSE
  \* WAKE_FRONT comes down,
  /\ wakeFront' = FALSE
  \* and the pass remembers whether it was up.
  /\ dWF' = wakeFront
  \* The arrival stack is empty.
  /\ arrivals' = <<>>
  \* The ghost counts the pass,
  /\ dPasses' = IF dPasses < PassMax THEN dPasses + 1 ELSE dPasses
  \* uses up what earned it,
  /\ dEarned' = FALSE
  \* and notes a pass that nothing earned.
  /\ overran' = (overran \/ ~dEarned)
  \* whole: the list with the live arrivals appended.
  /\ LET whole == waiting \o Alive(arrivals)
     \* The pass sweeps, or not; there is a choice only if the list holds a withdrawn node.
     IN \E sweep \in IF Dead(whole) = {} THEN {FALSE} ELSE BOOLEAN :
          \* kept: the list with every node, or only the live ones.
          LET kept == IF sweep THEN Alive(whole) ELSE whole
          \* The list is that, without the withdrawn nodes at its front.
          IN /\ waiting' = FrontOf(kept)
             \* The queue lets go of the withdrawn arrivals, of the swept nodes and of those at the
             \* front.
             /\ LetGo(Dead(arrivals) \cup (IF sweep THEN Dead(whole) ELSE {}) \cup FrontOff(kept))
             \* HANDOFF up: the handoff loop. Down: the budget if a waiter is there, else the end.
             /\ pc' = [pc EXCEPT ![d] = IF handoff THEN "dh" ELSE IF FrontOf(kept) # <<>> THEN "dnb"
                                                             \* (no waiter)
                                                             ELSE "dend"]
  \* Nothing else changes.
  /\ UNCHANGED <<avail, drainOn, queued, nudgeOut, handoff, nudged, wake, wl, guards, futs, dBudget,
                 dPos, dFirst, dRaised, dSent, dHand, dClearQ, dClearH, ret, lost, bargedOwed, turns>>

\* The handoff loop, one round: let go of withdrawn nodes at the front (List::front), then try to
\* take the front waiter's permits out of the state word (Lock::take_for_waiter: a compare-and-swap
\* that may pass the HANDOFF bit, because it serves the queue). No waiter, or not enough permits:
\* the loop ends.
DH(d) ==
  \* The pass is in the handoff loop.
  /\ pc[d] = "dh"
  \* The withdrawn nodes at the front leave the list,
  /\ waiting' = Pruned
  \* and the queue lets go of them.
  /\ LetGo(PrunedOff)
  \* Is a live waiter at the front, with its permits free?
  /\ IF Pruned # <<>> /\ avail >= Need[Head(Pruned)]
       \* Yes: its permits leave the state word
       THEN /\ avail' = avail - Need[Head(Pruned)]
            \* into the pass's hand,
            /\ dHand' = Need[Head(Pruned)]
            \* and the pass goes on to grant them.
            /\ pc' = [pc EXCEPT ![d] = "dg"]
       \* No: the loop ends, and the pass goes on to read the owed count,
       ELSE /\ pc' = [pc EXCEPT ![d] = "dho"]
            \* with nothing taken.
            /\ UNCHANGED <<avail, dHand>>
  \* Nothing else changes.
  /\ UNCHANGED <<drainOn, dirty, queued, nudgeOut, handoff, wakeFront, arrivals, nudged, wake, wl,
                 guards, futs, dWF, dBudget, dPos, dFirst, dRaised, dSent, dClearQ, dClearH, ret,
                 lost, bargedOwed, turns, tally>>

\* The grant (WakeList::grant: one compare-and-swap on the node). The front node leaves the list.
\* If it still waits it becomes "granted": the permits in the pass's hand are its future's, a
\* sleeping future goes on the wake list, and the queue lets go of the node, settling its owed
\* mark. If its future withdrew meanwhile, the node is let go and the permits go back.
DG(d) ==
  \* The pass holds permits for the front waiter.
  /\ pc[d] = "dg"
  \* h: that waiter.
  /\ LET h == Head(waiting) IN
       \* Its node leaves the list.
       /\ waiting' = Tail(waiting)
       \* Whatever happens, the queue settles its owed mark,
       /\ owedMark' = [owedMark EXCEPT ![h] = FALSE]
       \* and its share of the owed count.
       /\ owedCount' = owedCount - IF owedMark[h] THEN 1 ELSE 0
       \* Does the node still wait?
       /\ IF phase[h] = "waiting"
            \* Yes: it is granted.
            THEN /\ phase' = [phase EXCEPT ![h] = "granted"]
                 \* The permits are its future's now,
                 /\ got' = [got EXCEPT ![h] = @ + dHand]
                 \* and the pass's hand is empty.
                 /\ dHand' = 0
                 \* If the future is asleep (it returned Pending and waits for its waker), it goes
                 \* on the wake list. If it is awake (busy in a poll, or spinning on its node), it
                 \* is not: it will read the grant from its node itself.
                 /\ wl' = IF pc[h] = "parked" THEN wl \cup {h} ELSE wl
                 \* The loop goes round again.
                 /\ pc' = [pc EXCEPT ![d] = "dh"]
            \* No, its future withdrew: the node is free,
            ELSE /\ phase' = [phase EXCEPT ![h] = "none"]
                 \* and the pass goes on to put the permits back.
                 /\ pc' = [pc EXCEPT ![d] = "dgb"]
                 \* Nobody gets them and nobody is woken.
                 /\ UNCHANGED <<got, dHand, wl>>
  \* Nothing else changes.
  /\ UNCHANGED <<word, arrivals, nudged, wake, held, owner, depth, futs, dWF, dBudget, dPos, dFirst,
                 dRaised, dSent, dClearQ, dClearH, ret, lost, bargedOwed, turns, tally>>

\* The permits taken for a waiter that had withdrawn go back to the state word (one add).
DGB(d) ==
  \* The pass holds permits nobody took.
  /\ pc[d] = "dgb"
  \* They are free again,
  /\ avail' = avail + dHand
  \* and the pass's hand is empty.
  /\ dHand' = 0
  \* The loop goes round again.
  /\ pc' = [pc EXCEPT ![d] = "dh"]
  \* Nothing else changes.
  /\ UNCHANGED <<drainOn, dirty, queued, nudgeOut, handoff, wakeFront, owedCount, arrivals, waiting,
                 nodes, wake, wl, guards, futs, dWF, dBudget, dPos, dFirst, dRaised, dSent, dClearQ,
                 dClearH, ret, lost, bargedOwed, turns, tally>>

\* The pass found that the queue does not serve by handoff alone: it lets go of withdrawn nodes at
\* the front, and if a live waiter is left it goes on to read the nudging budget.
Compete(d) ==
  \* The withdrawn nodes at the front leave the list,
  /\ waiting' = Pruned
  \* and the queue lets go of them.
  /\ LetGo(PrunedOff)
  \* With a waiter left the pass reads the budget, else it goes to its end.
  /\ pc' = [pc EXCEPT ![d] = IF Pruned # <<>> THEN "dnb" ELSE "dend"]

\* After the handoff loop (WaitQueue::hands_off). HANDOFF is still up: only the holder of the
\* drain role ever brings it down. So what decides is the owed count (one load): is a queued
\* waiter still counted owed? Then the queue serves by handoff alone and this pass nudges nobody.
\* The count alone would not be enough, which is why the code reads the bit first: a waiter counts
\* itself just before its last try, and may take the permits instead.
DHO(d) ==
  \* The pass is past the handoff loop.
  /\ pc[d] = "dho"
  \* Is a waiter counted owed?
  /\ IF owedCount # 0
       \* Yes: no nudges; the pass goes to its end,
       THEN /\ pc' = [pc EXCEPT ![d] = "dend"]
            \* touching nothing.
            /\ UNCHANGED <<waiting, nodes, owedCount>>
       \* No: the queue competes after all.
       ELSE /\ Compete(d)
            \* No nudge is touched.
            /\ UNCHANGED nudged
  \* Nothing else changes.
  /\ UNCHANGED <<word, arrivals, wake, wl, guards, futs, dloc, ret, lost, bargedOwed, turns, tally>>

\* The pass reads the free permits (one load): that is its budget for nudges.
DNB(d) ==
  \* The pass is about to nudge.
  /\ pc[d] = "dnb"
  \* The budget is what is free now.
  /\ dBudget' = avail
  \* It starts at the front of the list,
  /\ dPos' = 1
  \* with the first live waiter still to come,
  /\ dFirst' = TRUE
  \* no marker raised
  /\ dRaised' = FALSE
  \* and no nudge sent.
  /\ dSent' = FALSE
  \* Next it walks the list.
  /\ pc' = [pc EXCEPT ![d] = "dni"]
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, nodes, wake, wl, guards, futs, dWF, dHand,
                 dClearQ, dClearH, ret, lost, bargedOwed, turns, tally>>

\* The place of the next live node at or after place i of the waiting list, or 0 if there is none
\* (List::iter skips withdrawn nodes).
NextLive(i) ==
  \* Is some node at or after place i live?
  IF \E j \in i..Len(waiting) : phase[waiting[j]] # "cancelled"
    \* Then the first such place;
    THEN CHOOSE j \in i..Len(waiting) :
           \* it is live,
           /\ phase[waiting[j]] # "cancelled"
           \* and nothing live comes before it.
           /\ \A k \in i..(j - 1) : phase[waiting[k]] = "cancelled"
    \* else none.
    ELSE 0

\* The nudge itself (WakeList::nudge: one compare-and-swap on the node): a node that still waits
\* and carries no nudge gets the NUDGED mark, and a sleeping future goes on the wake list. A node
\* that is nudged already is left alone, but a nudge is on its way to it all the same (the earlier
\* one, which its future has not looked at yet), so the marker must stay up. A node that withdrew
\* is skipped. Either way its request is taken off the budget and the walk moves on. `i` is the
\* node's place. (Mutant LowerPendingMarker: a node nudged already counts as "no nudge", as the
\* code did before defect E32 was fixed.)
Nudge(i) ==
  \* n: the node at that place.
  LET n == waiting[i]
      \* hit: the nudge reaches it.
      hit == phase[n] = "waiting" /\ ~nudged[n]
      \* pending: an earlier nudge is still on its way to it.
      pending == phase[n] = "waiting" /\ nudged[n] /\ Mutant # "LowerPendingMarker"
  \* The node is marked, if the nudge reaches it.
  IN /\ nudged' = [nudged EXCEPT ![n] = @ \/ hit]
     \* The pass notes that a nudge is on its way: this one, or the earlier one.
     /\ dSent' = (dSent \/ hit \/ pending)
     \* If the future is asleep (it returned Pending and waits for its waker), it goes on the
     \* wake list. If it is awake (busy in a poll, or spinning on its node), the release calls no
     \* waker: the future will read the mark from its node itself.
     /\ wl' = IF hit /\ pc[n] = "parked" THEN wl \cup {n} ELSE wl
     \* The budget shrinks by the request (never below zero).
     /\ dBudget' = IF dBudget >= Need[n] THEN dBudget - Need[n] ELSE 0
     \* The walk moves past the node,
     /\ dPos' = i + 1
     \* which was the first live one at most once.
     /\ dFirst' = FALSE

\* Where the pass goes when its walk is over: to lower NUDGE_OUT if it raised it for nothing, else
\* to its end. `raised` and `sent` are the two notes of the walk.
WalkEnds(raised, sent) == IF raised /\ ~sent THEN "dnf" ELSE "dend"

\* One step of the walk. The walk stops at the end of the list, or at the first live waiter whose
\* request is over the budget, unless that is the front waiter and a release asked for it to be
\* nudged anyway. Otherwise the pass nudges the waiter; before its first nudge it raises NUDGE_OUT
\* (one fetch-or), so that a waiter that polls at once still finds the marker to take back.
DNI(d) ==
  \* The pass walks the list.
  /\ pc[d] = "dni"
  \* i: the place of the next live waiter, if any.
  /\ LET i == NextLive(dPos) IN
       \* Is the walk over: no waiter left, or one that does not fit and is not a front to be
       \* nudged anyway?
       IF i = 0 \/ (Need[waiting[i]] > dBudget /\ ~(dFirst /\ dWF))
         \* Yes: the pass goes on,
         THEN /\ pc' = [pc EXCEPT ![d] = WalkEnds(dRaised, dSent)]
              \* having touched nothing.
              /\ UNCHANGED <<nudgeOut, nudged, wl, dBudget, dPos, dFirst, dRaised, dSent>>
         \* No. Is this the pass's first nudge?
         ELSE IF ~dRaised
           \* Yes: NUDGE_OUT goes up first,
           THEN /\ nudgeOut' = TRUE
                \* the pass notes that it raised it,
                /\ dRaised' = TRUE
                \* stays at this waiter,
                /\ dPos' = i
                \* and nudges it next.
                /\ pc' = [pc EXCEPT ![d] = "dnt"]
                \* The node is not touched yet.
                /\ UNCHANGED <<nudged, wl, dBudget, dFirst, dSent>>
           \* No, and the marker is up already: the pass nudges the waiter,
           ELSE /\ Nudge(i)
                \* and walks on.
                /\ UNCHANGED <<nudgeOut, dRaised, pc>>
  \* Nothing else changes.
  /\ UNCHANGED <<avail, drainOn, dirty, queued, handoff, wakeFront, owedCount, arrivals, waiting,
                 phase, owedMark, wake, guards, futs, dWF, dHand, dClearQ, dClearH, ret, lost,
                 bargedOwed, turns, tally>>

\* The pass's first nudge, after it raised NUDGE_OUT.
DNT(d) ==
  \* The pass raised the marker and stands at a waiter.
  /\ pc[d] = "dnt"
  \* It nudges that waiter.
  /\ Nudge(dPos)
  \* The walk goes on.
  /\ pc' = [pc EXCEPT ![d] = "dni"]
  \* Nothing else changes.
  /\ UNCHANGED <<word, owedCount, arrivals, waiting, phase, owedMark, wake, guards, futs, dWF,
                 dRaised, dHand, dClearQ, dClearH, ret, lost, bargedOwed, turns, tally>>

\* Nudges::finish: the pass raised NUDGE_OUT but every nudge found its waiter gone, so no nudge is
\* on its way: the marker comes down again (one fetch-and).
DNF(d) ==
  \* The pass raised the marker for nothing.
  /\ pc[d] = "dnf"
  \* NUDGE_OUT comes down.
  /\ nudgeOut' = FALSE
  \* The pass goes to its end.
  /\ pc' = [pc EXCEPT ![d] = "dend"]
  \* Nothing else changes.
  /\ UNCHANGED <<avail, drainOn, dirty, queued, handoff, wakeFront, owedCount, arrivals, waiting,
                 nodes, wake, wl, guards, futs, dloc, ret, lost, bargedOwed, turns, tally>>

\* The end of a pass. It lets go of withdrawn nodes at the front and notes whether any waiter is
\* left: if none, QUEUED may come down. Then the drain reads the owed count (one load): if nobody
\* is counted owed, HANDOFF may come down.
DEnd(d) ==
  \* The pass is done granting and nudging.
  /\ pc[d] = "dend"
  \* The withdrawn nodes at the front leave the list,
  /\ waiting' = Pruned
  \* and the queue lets go of them.
  /\ LetGo(PrunedOff)
  \* QUEUED may come down if no waiter is left.
  /\ dClearQ' = (Pruned = <<>>)
  \* HANDOFF may come down if nobody is counted owed (the count after this pass's own letting go).
  /\ dClearH' = (owedCount - Cardinality({u \in PrunedOff : owedMark[u]}) = 0)
  \* Next the pass tries to give the role up.
  /\ pc' = [pc EXCEPT ![d] = "dex"]
  \* Nothing else changes.
  /\ UNCHANGED <<word, arrivals, nudged, wake, wl, guards, futs, dWF, dBudget, dPos, dFirst, dRaised,
                 dSent, dHand, ret, lost, bargedOwed, turns, tally>>

\* The drain tries to give the role up: one compare-and-swap. It needs DIRTY down, and it needs
\* WAKE_FRONT down unless the pass found nobody queued. With DIRTY up, somebody queued or withdrew
\* while the pass worked; with WAKE_FRONT up and a waiter queued, a release came that the waiter
\* must be nudged for: the drain passes again. With WAKE_FRONT up and nobody queued, the release
\* has no front to nudge: its request is dropped together with QUEUED, so that releases by other
\* threads cannot keep this one passing over an empty queue. Otherwise DRAINING comes down
\* together with the bits the pass may clear, the wakers on the wake list are called, and the task
\* goes on with what it was doing. (Mutant ExitIgnoresRequest: the exit never waits for
\* WAKE_FRONT.)
DEx(d) ==
  \* The pass is over.
  /\ pc[d] = "dex"
  \* Did somebody queue or withdraw meanwhile, or did a release ask while a waiter is queued?
  /\ IF dirty \/ (wakeFront /\ ~dClearQ /\ Mutant # "ExitIgnoresRequest")
       \* Yes: another pass,
       THEN /\ pc' = [pc EXCEPT ![d] = "d0"]
            \* with the role, the bits, the wake list and the ghosts kept.
            /\ UNCHANGED <<drainOn, queued, handoff, wakeFront, wake, wl, ret, dPasses, dEarned>>
       \* No: the role is free.
       ELSE /\ drainOn' = FALSE
            \* QUEUED comes down if the pass found no waiter,
            /\ queued' = (queued /\ ~dClearQ)
            \* and with it a release's request that had no front to nudge.
            /\ wakeFront' = (wakeFront /\ ~dClearQ)
            \* HANDOFF comes down if nobody was counted owed.
            /\ handoff' = (handoff /\ ~dClearH)
            \* Every waker on the wake list is called.
            /\ wake' = [u \in Tasks |-> wake[u] \/ u \in wl]
            \* The wake list is empty.
            /\ wl' = {}
            \* The task goes on where it meant to go,
            /\ pc' = [pc EXCEPT ![d] = ret[d]]
            \* and that memory is cleared.
            /\ ret' = [ret EXCEPT ![d] = "idle"]
            \* The ghosts start over: the next drain has run no pass and has its first one to run.
            /\ dPasses' = 0 /\ dEarned' = TRUE
  \* The pass's locals are forgotten.
  /\ dWF' = FALSE /\ dBudget' = 0 /\ dPos' = 0 /\ dFirst' = FALSE /\ dRaised' = FALSE
  \* (the rest of them)
  /\ dSent' = FALSE /\ dClearQ' = FALSE /\ dClearH' = FALSE
  \* Nothing else changes.
  /\ UNCHANGED <<avail, dirty, nudgeOut, owedCount, arrivals, waiting, nodes, guards, futs, dHand,
                 lost, bargedOwed, turns, overran>>

----------------------------------------------------------------------------
\* The specification.

\* The start: every permit free, nobody waiting.
Init ==
  \* Every permit is free, but the ones the starting holders have.
  /\ avail = Permits - SumOf([t \in Tasks |-> IF t \in Start THEN Need[t] ELSE 0], Tasks)
  \* Nobody holds the drain role, and nothing changed behind anybody's back.
  /\ drainOn = FALSE /\ dirty = FALSE
  \* No waiter is queued, no nudge is on its way, no handoff is owed, no front is to be nudged.
  /\ queued = FALSE /\ nudgeOut = FALSE /\ handoff = FALSE /\ wakeFront = FALSE
  \* Nobody is counted owed.
  /\ owedCount = 0
  \* No arrival, and an empty waiting list.
  /\ arrivals = <<>> /\ waiting = <<>>
  \* No task has a node.
  /\ phase = [t \in Tasks |-> "none"]
  \* No node is nudged or marked owed.
  /\ nudged = [t \in Tasks |-> FALSE] /\ owedMark = [t \in Tasks |-> FALSE]
  \* No waker was called, and no pass has anybody to wake.
  /\ wake = [t \in Tasks |-> FALSE] /\ wl = {}
  \* Nobody has permits pending.
  /\ got = [t \in Tasks |-> 0]
  \* Only a starting holder has a guard: its permits (a reentrant lock: one guard).
  /\ held = [t \in Tasks |-> IF t \notin Start THEN 0 ELSE IF Kind = "Semaphore" THEN Need[t] ELSE 1]
  \* A reentrant lock is owned by its starting holder, if there is one,
  /\ owner = IF Kind = "Reentrant" /\ Start # {} THEN Owner[CHOOSE t \in Start : TRUE] ELSE NoOwner
  \* with that one guard out.
  /\ depth = IF Kind = "Reentrant" THEN Cardinality(Start) ELSE 0
  \* No future has lost a race.
  /\ bypass = [t \in Tasks |-> 0]
  \* No try is under way.
  /\ marked = [t \in Tasks |-> FALSE] /\ took = [t \in Tasks |-> FALSE]
  \* (its other locals)
  /\ seenH = [t \in Tasks |-> FALSE] /\ passOn = [t \in Tasks |-> FALSE]
  \* Nobody spins on its node.
  /\ spinning = [t \in Tasks |-> FALSE]
  \* No pass is under way.
  /\ dWF = FALSE /\ dBudget = 0 /\ dPos = 0 /\ dFirst = FALSE /\ dRaised = FALSE /\ dSent = FALSE
  \* (its other locals)
  /\ dHand = 0 /\ dClearQ = FALSE /\ dClearH = FALSE
  \* Every task is idle, but a starting holder, which holds.
  /\ pc = [t \in Tasks |-> IF t \in Start THEN "holding" ELSE "idle"]
  \* Every task would go on to idle.
  /\ ret = [t \in Tasks |-> "idle"]
  \* No task has lost a race, and nobody barged past an owed handoff.
  /\ lost = [t \in Tasks |-> 0] /\ bargedOwed = FALSE
  \* Nobody has asked for the lock yet, but a starting holder, whose hold used a turn if its turns
  \* are counted.
  /\ turns = [t \in Tasks |-> IF t \in Start /\ Turns[t] > 0 THEN 1 ELSE 0]
  \* No drain has run a pass, the first drain has its first pass to run, and none overran.
  /\ dPasses = 0 /\ dEarned = TRUE /\ overran = FALSE

\* The steps a task takes on its own once started: they must happen.
Internal(t) ==
  \* The attempt and the spin before queueing,
  \/ Retake(t) \/ PreSpin(t) \/ PreSpinOut(t)
  \* the waiter's own steps up to its try,
  \/ Push(t) \/ Publish(t) \/ Park0(t) \/ Repark(t) \/ Poll(t) \/ GrantCons(t) \/ NudgeCons(t) \/ Try(t)
  \* the spin on its own node after a failed try,
  \/ SpinStart(t) \/ SpinEnd(t)
  \* the try that may raise HANDOFF,
  \/ Owe1(t) \/ Owe2(t) \/ Owe3(t) \/ Owe4(t) \/ Owe5(t) \/ Owe6(t) \/ Owe7(t)
  \* leaving the queue, and asking for a pass after a release,
  \/ Wd1(t) \/ Wd2(t) \/ Gb1(t) \/ Gb2(t) \/ Gb3(t) \/ Released(t)
  \* and the drain's steps.
  \/ D0(t) \/ DH(t) \/ DG(t) \/ DGB(t) \/ DHO(t) \/ DNB(t) \/ DNI(t) \/ DNT(t) \/ DNF(t) \/ DEnd(t)
  \* (the last one)
  \/ DEx(t)

\* Every step the system can take.
Next ==
  \* Some task
  \E t \in Tasks :
    \* acquires, re-enters, cancels, releases, or takes one of its own steps,
    \/ Acquire(t) \/ Reenter(t) \/ Cancel(t) \/ Release(t) \/ Internal(t)
    \* or gives its owned guard to another task.
    \/ \E u \in Tasks : Give(t, u)

\* Fairness: a started operation finishes, a woken future is polled, and a holder eventually drops
\* its guard. Acquiring, re-entering, cancelling and moving a guard are the caller's choices.
Fairness == \A t \in Tasks : WF_vars(Internal(t)) /\ WF_vars(Release(t))

\* Every behaviour: start in Init, take Next steps or stutter, fairly.
Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The free permits never exceed the permits.
  /\ avail \in 0..Permits
  \* The flag bits are each a yes or a no.
  /\ {drainOn, dirty, queued, nudgeOut, handoff, wakeFront} \subseteq BOOLEAN
  \* No more waiters are counted owed than there are tasks.
  /\ owedCount \in 0..Cardinality(Tasks)
  \* The arrival stack and the waiting list hold tasks.
  /\ Range(arrivals) \subseteq Tasks /\ Range(waiting) \subseteq Tasks
  \* Every node is in one of the four phases.
  /\ phase \in [Tasks -> {"none", "waiting", "granted", "cancelled"}]
  \* Nobody has more pending than the permits, or holds more than the permits times the depth.
  /\ got \in [Tasks -> 0..Permits] /\ held \in [Tasks -> 0..(Permits * MaxDepth)]
  \* The owner is an owner token or none.
  /\ owner \in Owners \cup {NoOwner}
  \* A pass's hand holds at most the permits.
  /\ dHand \in 0..Permits
  \* Every task is at one of the known places.
  /\ pc \in [Tasks -> PCs]
  \* The ghosts are a small count and two yes-or-no notes.
  /\ dPasses \in 0..PassMax /\ dEarned \in BOOLEAN /\ overran \in BOOLEAN
  \* Each future spins on its node or not.
  /\ spinning \in [Tasks -> BOOLEAN]

\* Only the holder of the drain role is inside the drain, and there is at most one.
\* Rules out two threads sorting the same waiting list at once, which would corrupt it: the list
\* has no lock of its own.
DrainExclusive ==
  \* The DRAINING bit is up exactly while some task is at a drain step.
  /\ drainOn <=> \E t \in Tasks : pc[t] \in DrainPCs
  \* No two tasks are at a drain step.
  /\ \A t, u \in Tasks : pc[t] \in DrainPCs /\ pc[u] \in DrainPCs => t = u

\* Every node is in at most one place: a node that waits or has withdrawn and is not let go yet is
\* on the arrival stack or in the waiting list, once.
\* Rules out a waiter that is linked nowhere, which no pass could ever grant or nudge.
NodesLinked ==
  \* No node is linked twice.
  /\ Len(arrivals \o waiting) = Cardinality(Range(arrivals \o waiting))
  \* A node that waits is linked, unless its task is just about to push it.
  /\ \A t \in Tasks : phase[t] = "waiting" => t \in Range(arrivals \o waiting)

\* Every permit is somewhere: free, pending for a task, in a guard, or in the hand of a pass.
\* Reentrant: the one permit is free, pending, with the owner, or in the hand of a pass.
PermitsConserved ==
  \* For a semaphore:
  /\ Kind = "Semaphore" =>
       \* the free, pending, held and in-hand permits add up to the permits there are. Rules out a
       \* permit that vanishes (the lock is lost for ever) or appears from nowhere (two holders).
       avail + SumOf(got, Tasks) + SumOf(held, Tasks) + dHand = Permits
  \* For a reentrant lock:
  /\ Kind = "Reentrant" =>
       \* its one permit is free, pending, with an owner, or in a pass's hand: exactly one of these.
       avail + SumOf(got, Tasks) + (IF owner # NoOwner THEN 1 ELSE 0) + dHand = 1

\* MUTUAL EXCLUSION. A Semaphore never has more permits out than it has (a
\* Mutex: at most one holder). A ReentrantMutex's guards are all held by
\* tasks of the one Owner holding it. Lean: mutual_exclusion.
MutualExclusion ==
  \* For a semaphore: no more permits are in guards than exist. Rules out, for a Mutex, two tasks
  \* inside the critical section at once.
  /\ Kind = "Semaphore" => SumOf(held, Tasks) <= Permits
  \* For a reentrant lock: every task holding a guard belongs to the owner that holds the lock.
  \* Rules out a second owner getting in.
  /\ Kind = "Reentrant" => \A t \in Tasks : held[t] > 0 => Owner[t] = owner

\* REENTRANCY DEPTH. The depth is exactly the guards out, and the lock has
\* an owner exactly when the depth is not 0.
ReentrancyDepth ==
  \* For a reentrant lock:
  Kind = "Reentrant" =>
    \* the depth equals the number of guards out,
    /\ depth = SumOf(held, Tasks)
    \* and the lock has no owner exactly when no guard is out. Rules out a lock that is free while a
    \* guard still exists (the bug NoDepth), and one that stays owned with no guard left.
    /\ (owner = NoOwner <=> depth = 0)

\* BOUNDED BYPASS. During one acquire a waiter loses at most Bound counted
\* races, and from the moment it has lost Bound of them the HANDOFF bit is
\* up for as long as it waits, so nobody can pass it again.
\* Lean: bounded_bypass, owed_at_bound.
\* Rules out, for example: a waiter that is nudged again and again and every time finds a faster
\* thread already took the lock, for ever.
BoundedBypass ==
  \* For every task:
  \A t \in Tasks :
    \* it is never passed over more than Bound times,
    /\ lost[t] <= Bound
    \* while it waits with Bound losses, barging is shut,
    /\ AtBound(t) => handoff
    \* and its own counter (Contend::bypass) never reads more than Bound.
    /\ bypass[t] <= Bound

\* HANDOFF EXCLUSIVE. Nobody ever barges while a handoff is owed: the
\* HANDOFF bit shuts the fast path until the queue has been served.
\* Lean: owed_exclusive, owed_blocks_barging.
\* Rules out, for example: a waiter has lost Bound races, the holder lets go, and a newcomer grabs
\* the lock before the queue's pass can hand it over.
HandoffExclusive == ~bargedOwed

\* A sleeping future whose node carries a mark (granted, or nudged) has had its waker called, or
\* is on the wake list of the pass under way. Lean: handed_woken.
MarkedWoken ==
  \* For every task
  \A t \in Tasks :
    \* that sleeps with its node granted or nudged:
    pc[t] = "parked" /\ (phase[t] = "granted" \/ nudged[t]) =>
      \* its waker was called, or the pass under way is about to call it.
      wake[t] \/ t \in wl

\* Every task is between operations and nobody holds the drain role.
Quiescent == ~drainOn /\ \A t \in Tasks : pc[t] \in {"idle", "holding", "owned", "parked"}

\* The live waiters, oldest first.
LiveQ == SelectSeq(waiting \o arrivals, LAMBDA u : phase[u] = "waiting")

\* Left behind with nobody to act, once everything is quiet.
Stranded ==
  \* An arrival no pass took in;
  \/ arrivals # <<>>
  \* or a waiter, with the QUEUED bit down, so that no release would look for it;
  \/ LiveQ # <<>> /\ ~queued
  \* or a front waiter whose permits are free, and either a handoff is owed (the pass should have
  \* granted them) or its waker was not called (nobody told it to try);
  \/ LiveQ # <<>> /\ avail >= Need[Head(LiveQ)] /\ (handoff \/ ~wake[Head(LiveQ)])
  \* or the NUDGE_OUT marker up with no woken waiter left to take it back, so that every later
  \* release would skip its pass;
  \/ nudgeOut /\ ~\E t \in Tasks : pc[t] = "parked" /\ wake[t]
  \* or a release's request to nudge the front still up, with no pass left to act on it.
  \/ wakeFront

\* NO LOST WAKEUP. A mark on a sleeping future comes with a wake; and once
\* every task is between operations, no waiter sleeps while it could proceed.
\* Lean: no_stranded_head, handed_woken.
\* Rules out, for example: a release makes the lock free and tells nobody; the waiter sleeps for
\* ever beside a free lock.
NoLostWakeup == MarkedWoken /\ (Quiescent => ~Stranded)

\* CANCELLATION PASSES ON. An idle task holds nothing: a waiter dropped
\* after a grant gave the permits back.
\* Rules out, for example: a waiter is handed the lock, its future is dropped before it runs, and
\* the lock stays held by nobody: everyone after it waits for ever.
CancelPassesOn == \A t \in Tasks : pc[t] = "idle" => held[t] = 0 /\ got[t] = 0

\* A DRAIN'S PASSES ARE EARNED. A drain runs one pass. It runs another only if, since that pass
\* began, an operation of another thread landed on it that has something to tell the queue: a
\* waiter that queues, withdraws, or claims the handoff it is owed; or a release that finds a node
\* queued. So a drain runs at most one pass plus one for each such operation, and a release over
\* an empty queue costs it none.
\* Rules out, for example: a thread drops its guard, takes the drain role to tell the queue, finds
\* nobody queued, and is kept passing by other threads that take the lock and let it go.
DrainBounded == ~overran

\* No drain runs three passes. This one is NOT a promise: MC_Sync_Green_PassesGrow breaks it on
\* purpose, to show that the passes of a drain do grow with what other threads do to the queue
\* while it runs.
FewPasses == dPasses < 3

----------------------------------------------------------------------------
\* Liveness. Checked for the tasks in Watched.

\* Every waiter stops waiting: it is granted the lock, takes it itself, or cancels.
\* Rules out a waiter that waits for ever although the lock keeps being released: because its
\* losses are never counted, or because it holds the drain role and every pass is followed by a
\* release that asks for another (defect E32).
\*
\* SCOPE: this row checks that the waiter gets the lock, not only that it is woken and retries.
\* It follows the request (phase[t], the phase of the waiter's node), not the future (pc[t]).
\* Only two steps move phase[t] off "waiting": a pass grants the node, or the future withdraws
\* it. In the live configurations no watched future is dropped (Drops = {}), so a withdraw there
\* comes only after the waiter's own try took the lock. A wake whose retry loses leaves phase[t]
\* at "waiting": the loss is counted and the future sleeps again. So a waiter that is woken and
\* loses for ever breaks this row.
\* A row written on the future instead, (pc[t] = "parked") ~> (pc[t] # "parked"), would promise
\* much less: a woken future's poll takes pc from "parked" to its retry whether or not the retry
\* wins, so that row would hold for a waiter that never gets the lock. It checks a wake and a
\* retry, not acquisition, and must not be cited for this promise.
EveryWaiterServed == \A t \in Watched : (phase[t] = "waiting") ~> (phase[t] # "waiting")

\* A waiter operation lands on a drain: task t queues, withdraws, or claims its handoff, and
\* finds the drain role taken, so the holder of the role passes once more for it.
Lands(t) ==
  \* The role is taken,
  /\ drainOn
  \* and the task publishes its node, or asks for a pass after its try, or tells the queue it
  \* withdrew,
  /\ \/ Publish(t) \/ Owe7(t) \/ Wd2(t)
     \* or raises HANDOFF.
     \/ (Owe3(t) /\ ~handoff /\ handoff')

\* From some point on, no waiter operation lands on a drain.
WaitersSettle == <>[][\A t \in Tasks : ~Lands(t)]_vars

\* Again and again, each task is outside the drain.
DrainsEnd == \A t \in Tasks : []<>(pc[t] \notin DrainPCs)

\* EVERY DRAIN ENDS, ONCE THE WAITERS SETTLE. If from some point on no waiter operation lands on
\* a drain, every thread that takes the drain role gives it up again, whatever the others
\* release. The condition is needed: each waiter that queues or withdraws while a drain runs
\* costs its holder one more pass (DrainBounded), so waiters that keep coming can keep one thread
\* passing. That is lock-free, not wait-free: every extra pass is another thread's own operation
\* getting done.
\* Rules out, for example: a pass that never finishes its walk, or a drain that goes on passing
\* when nothing asks it to.
DrainsEndWhenWaitersSettle == WaitersSettle => DrainsEnd

\* Reentrant: no Owner keeps the lock forever.
OwnersLetGo == \A o \in Owners : []<>(owner # o)

\* Reentrant: if no owner holds the lock forever, every waiter is served.
ReentrantServed == OwnersLetGo => EveryWaiterServed

====
