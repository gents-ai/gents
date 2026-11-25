---- MODULE NetworkReturnRetry ----
\* lean: none
\* crates: none
\* models: ObserverResync.asksRetryNow, ObserverResync.retryNowRestarts
\* paths: apps/gents-desktop/src/hooks/clientObservation.ts
\* One node the desktop observes while the device's network drops and comes
\* back (`clientObservation.ts`, the observer's retry now in gents-observe).
\*
\* A dropped network ends the node's streams; the node is shown lost and its
\* source waits out its reconnect backoff (`Wait` ticks) before redialing. The
\* network coming back, or changing (Wi-Fi to mobile data stays online), is an
\* app event: when it asks for a retry now (Lean `asksRetryNow`), every node is
\* asked, and a node that is not connected restarts at once with its backoff
\* reset, while a connected node is left alone (Lean `retryNowRestarts`).
\*
\* Time advances only when nothing is due, so a due redial or connect happens
\* before the clock passes it.
\* REDs: `Triggers = FALSE` leaves a lost node waiting out its backoff after
\* the network is back; `SparesConnected = FALSE` restarts a connected node on
\* every network change, dropping its live subscriptions.
EXTENDS Naturals

CONSTANTS
  Wait,            \* backoff ticks left when the node is lost
  Horizon,         \* ticks checked
  Triggers,        \* a network event asks for a retry now
  SparesConnected  \* retry now leaves a connected node alone

ASSUME /\ Wait \in Nat \ {0} /\ Horizon \in Nat
       /\ Triggers \in BOOLEAN /\ SparesConnected \in BOOLEAN

VARIABLES
  now,
  up,        \* the device has a network
  status,    \* "connected" | "lost" | "connecting"
  retryIn,   \* backoff ticks before the lost node redials
  backFor,   \* ticks since the network came back while the node is lost
  dropped    \* a connected node was restarted by a network event

vars == <<now, up, status, retryIn, backFor, dropped>>

Init ==
  /\ now = 0 /\ up = TRUE /\ status = "connected" /\ retryIn = 0
  /\ backFor = 0 /\ dropped = FALSE

\* What a network event does to the node when it asks for a retry now.
RetryNow ==
  IF ~Triggers THEN UNCHANGED <<status, retryIn, dropped>>
  ELSE IF status = "connected"
    THEN IF SparesConnected THEN UNCHANGED <<status, retryIn, dropped>>
         ELSE /\ status' = "connecting" /\ retryIn' = 0 /\ dropped' = TRUE
    ELSE /\ status' = "connecting" /\ retryIn' = 0 /\ UNCHANGED dropped

Drop ==
  /\ up /\ now < Horizon
  /\ up' = FALSE
  /\ status' = IF status = "connected" THEN "lost" ELSE status
  /\ retryIn' = IF status = "connected" THEN Wait ELSE retryIn
  /\ UNCHANGED <<now, backFor, dropped>>

Return ==
  /\ ~up
  /\ up' = TRUE
  /\ RetryNow
  /\ backFor' = 0
  /\ UNCHANGED now

\* Wi-Fi to mobile data: still online, a new network.
Change ==
  /\ up /\ now < Horizon
  /\ RetryNow
  /\ UNCHANGED <<now, up, backFor>>

Redial ==
  /\ status = "lost" /\ retryIn = 0
  /\ status' = "connecting"
  /\ UNCHANGED <<now, up, retryIn, backFor, dropped>>

Connect ==
  /\ status = "connecting" /\ up
  /\ status' = "connected"
  /\ UNCHANGED <<now, up, retryIn, backFor, dropped>>

Due == (status = "lost" /\ retryIn = 0) \/ (status = "connecting" /\ up)

Tick ==
  /\ now < Horizon /\ ~Due
  /\ now' = now + 1
  /\ retryIn' = IF status = "lost" /\ retryIn > 0 THEN retryIn - 1 ELSE retryIn
  /\ backFor' = IF status = "lost" /\ up THEN backFor + 1 ELSE 0
  /\ UNCHANGED <<up, status, dropped>>

Next == Drop \/ Return \/ Change \/ Redial \/ Connect \/ Tick

Spec == Init /\ [][Next]_vars

\* A node lost to the network redials as soon as the network is back.
RedialsWhenBack == ~(status = "lost" /\ up /\ backFor > 0)

\* A network event never restarts a connected node.
ConnectedUntouched == ~dropped

====
