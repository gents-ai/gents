---- MODULE WindowIndependence ----
(***************************************************************************)
(* Several desktop windows, each on its own node (Lean `ClientWindowScope`). *)
(* One client core serves them all: one node-scoped observer source per     *)
(* runtime publishes into one ObservedStore, and its change notice is       *)
(* broadcast to every window (`desktop://client-updated`). Each window owns *)
(* its node selection (`selected_nodes_by_window`), its pack watches        *)
(* (`UxWatchRegistry`, PackUxWatch) and its store view: on a notice it      *)
(* reads its own node's slice through a bridge command and publishes the    *)
(* result only if the view's epoch is unchanged (a selection bumps it).     *)
(*                                                                         *)
(* A command names its node or falls back to the calling window's own       *)
(* selection. `SharedFallback` is the code before this model: the fallback *)
(* is the client core's one `selected_node_did`, which names a node only    *)
(* while exactly one window is open, so a second window leaves every        *)
(* window's node-scoped views (MCP services, backend health) without a      *)
(* node. `CancelOnSwitch = FALSE` keeps a window's pack watch on its old    *)
(* node.                                                                    *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
  Windows,
  Nodes,
  MaxWrites,       \* runtime writes per node
  MaxSwitch,       \* selection changes in a behavior
  SharedFallback,  \* RED: a window's read resolves the core's one shared selection
  CancelOnSwitch   \* FALSE: a selection change keeps the old node's watch

None == "none"
NoVer == MaxWrites + 1

VARIABLES
  sel,        \* window -> selected node
  latest,     \* node -> runtime document version
  observed,   \* node -> version the shared ObservedStore shows (node scoped)
  dirty,      \* window -> a change notice arrived since its last read began
  epoch,      \* window -> view epoch
  reading,    \* window -> in-flight read [node, epoch]
  view,       \* window -> [node, ver] the window shows
  watch,      \* window -> node of the window's pack watch
  switches,
  crossed     \* ghost: a watch delivered another node's event to a window

vars == <<sel, latest, observed, dirty, epoch, reading, view, watch,
          switches, crossed>>

Idle == [node |-> None, epoch |-> 0]

TypeOK ==
  /\ sel \in [Windows -> Nodes \cup {None}]
  /\ latest \in [Nodes -> 0..MaxWrites]
  /\ observed \in [Nodes -> 0..MaxWrites]
  /\ dirty \in [Windows -> BOOLEAN]
  /\ epoch \in [Windows -> 0..MaxSwitch]
  /\ \A w \in Windows : reading[w].node \in Nodes \cup {None}
  /\ \A w \in Windows : view[w].node \in Nodes \cup {None} /\ view[w].ver \in 0..NoVer
  /\ watch \in [Windows -> Nodes \cup {None}]
  /\ switches \in 0..MaxSwitch /\ crossed \in BOOLEAN

Init ==
  /\ sel = [w \in Windows |-> None]
  /\ latest = [n \in Nodes |-> 0] /\ observed = [n \in Nodes |-> 0]
  /\ dirty = [w \in Windows |-> FALSE] /\ epoch = [w \in Windows |-> 0]
  /\ reading = [w \in Windows |-> Idle]
  /\ view = [w \in Windows |-> [node |-> None, ver |-> NoVer]]
  /\ watch = [w \in Windows |-> None]
  /\ switches = 0 /\ crossed = FALSE

(* The runtime and the shared observer. *)

Write(n) ==
  /\ latest[n] < MaxWrites /\ latest' = [latest EXCEPT ![n] = @ + 1]
  /\ UNCHANGED <<sel, observed, dirty, epoch, reading, view, watch, switches, crossed>>

\* The node's source delivers the change and the store publishes it; the
\* notice reaches every window.
Observe(n) ==
  /\ observed[n] < latest[n] /\ observed' = [observed EXCEPT ![n] = latest[n]]
  /\ dirty' = [w \in Windows |-> TRUE]
  /\ UNCHANGED <<sel, latest, epoch, reading, view, watch, switches, crossed>>

(* A window. *)

\* `select_window_node`: the window's watches are cancelled before its
\* selection is written, under the bridge lock; its view restarts loading.
Select(w, n) ==
  /\ switches < MaxSwitch /\ sel[w] # n
  /\ sel' = [sel EXCEPT ![w] = n]
  /\ epoch' = [epoch EXCEPT ![w] = @ + 1]
  /\ view' = [view EXCEPT ![w] = [node |-> n, ver |-> NoVer]]
  /\ dirty' = [dirty EXCEPT ![w] = TRUE]
  /\ watch' = IF CancelOnSwitch THEN [watch EXCEPT ![w] = None] ELSE watch
  /\ switches' = switches + 1
  /\ UNCHANGED <<latest, observed, reading, crossed>>

\* The page watches its selected node.
Rewatch(w) ==
  /\ watch[w] = None /\ sel[w] # None /\ watch' = [watch EXCEPT ![w] = sel[w]]
  /\ UNCHANGED <<sel, latest, observed, dirty, epoch, reading, view, switches, crossed>>

\* A watch event of the watch's node reaches its window (the registry sends
\* only to a registered watch).
WatchDeliver(w) ==
  /\ watch[w] # None
  /\ crossed' = (crossed \/ watch[w] # sel[w])
  /\ UNCHANGED <<sel, latest, observed, dirty, epoch, reading, view, watch, switches>>

\* The core's shared selection names a node only while one window is open.
Shared == IF Cardinality(Windows) = 1 THEN sel[CHOOSE w \in Windows : TRUE] ELSE None

Scope(w) == IF SharedFallback THEN Shared ELSE sel[w]

StartRead(w) ==
  /\ dirty[w] /\ reading[w].node = None /\ Scope(w) # None
  /\ reading' = [reading EXCEPT ![w] = [node |-> Scope(w), epoch |-> epoch[w]]]
  /\ dirty' = [dirty EXCEPT ![w] = FALSE]
  /\ UNCHANGED <<sel, latest, observed, epoch, view, watch, switches, crossed>>

FinishRead(w) ==
  /\ reading[w].node # None
  /\ view' = IF reading[w].epoch = epoch[w]
               THEN [view EXCEPT ![w] = [node |-> reading[w].node,
                                         ver |-> observed[reading[w].node]]]
               ELSE view
  /\ reading' = [reading EXCEPT ![w] = Idle]
  /\ UNCHANGED <<sel, latest, observed, dirty, epoch, watch, switches, crossed>>

Next ==
  \/ \E n \in Nodes : Write(n) \/ Observe(n)
  \/ \E w \in Windows : \/ \E n \in Nodes : Select(w, n)
                        \/ Rewatch(w) \/ WatchDeliver(w) \/ StartRead(w) \/ FinishRead(w)

Fairness ==
  /\ \A n \in Nodes : WF_vars(Observe(n))
  /\ \A w \in Windows : WF_vars(Rewatch(w)) /\ WF_vars(StartRead(w)) /\ WF_vars(FinishRead(w))

Spec == Init /\ [][Next]_vars /\ Fairness

(* Properties. *)

\* No delivery crosses windows: a window shows only its own node, and its
\* pack watch delivers only its own node's events.
NoCrossDelivery ==
  /\ \A w \in Windows : view[w].node = sel[w]
  /\ ~crossed

\* Each window converges for its own node, whatever the other windows do.
Converges ==
  \A w \in Windows :
    <>[](sel[w] = None \/ (view[w].node = sel[w] /\ view[w].ver = latest[sel[w]]))

\* Every selected window eventually watches its own node.
WatchFollowsSelection ==
  \A w \in Windows : <>[](sel[w] = None \/ watch[w] = sel[w])

====
