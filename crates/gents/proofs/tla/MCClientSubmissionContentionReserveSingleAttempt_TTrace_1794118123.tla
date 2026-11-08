---- MODULE MCClientSubmissionContentionReserveSingleAttempt_TTrace_1794118123 ----
EXTENDS Sequences, TLCExt, Toolbox, Naturals, TLC, MCClientSubmissionContentionReserveSingleAttempt

_expression ==
    LET MCClientSubmissionContentionReserveSingleAttempt_TEExpression == INSTANCE MCClientSubmissionContentionReserveSingleAttempt_TEExpression
    IN MCClientSubmissionContentionReserveSingleAttempt_TEExpression!expression
----

_trace ==
    LET MCClientSubmissionContentionReserveSingleAttempt_TETrace == INSTANCE MCClientSubmissionContentionReserveSingleAttempt_TETrace
    IN MCClientSubmissionContentionReserveSingleAttempt_TETrace!trace
----

_inv ==
    ~(
        TLCGet("level") = Len(_TETrace)
        /\
        sessVer = (1)
        /\
        tries = (1)
        /\
        agentLeft = (0)
        /\
        created = (0)
        /\
        client = ("lost")
        /\
        snap = (0)
    )
----

_init ==
    /\ sessVer = _TETrace[1].sessVer
    /\ tries = _TETrace[1].tries
    /\ agentLeft = _TETrace[1].agentLeft
    /\ snap = _TETrace[1].snap
    /\ client = _TETrace[1].client
    /\ created = _TETrace[1].created
----

_next ==
    /\ \E i,j \in DOMAIN _TETrace:
        /\ \/ /\ j = i + 1
              /\ i = TLCGet("level")
        /\ sessVer  = _TETrace[i].sessVer
        /\ sessVer' = _TETrace[j].sessVer
        /\ tries  = _TETrace[i].tries
        /\ tries' = _TETrace[j].tries
        /\ agentLeft  = _TETrace[i].agentLeft
        /\ agentLeft' = _TETrace[j].agentLeft
        /\ snap  = _TETrace[i].snap
        /\ snap' = _TETrace[j].snap
        /\ client  = _TETrace[i].client
        /\ client' = _TETrace[j].client
        /\ created  = _TETrace[i].created
        /\ created' = _TETrace[j].created

\* Uncomment the ASSUME below to write the states of the error trace
\* to the given file in Json format. Note that you can pass any tuple
\* to `JsonSerialize`. For example, a sub-sequence of _TETrace.
    \* ASSUME
    \*     LET J == INSTANCE Json
    \*         IN J!JsonSerialize("MCClientSubmissionContentionReserveSingleAttempt_TTrace_1794118123.json", _TETrace)

=============================================================================

 Note that you can extract this module `MCClientSubmissionContentionReserveSingleAttempt_TEExpression`
  to a dedicated file to reuse `expression` (the module in the 
  dedicated `MCClientSubmissionContentionReserveSingleAttempt_TEExpression.tla` file takes precedence 
  over the module `MCClientSubmissionContentionReserveSingleAttempt_TEExpression` below).

---- MODULE MCClientSubmissionContentionReserveSingleAttempt_TEExpression ----
EXTENDS Sequences, TLCExt, Toolbox, Naturals, TLC, MCClientSubmissionContentionReserveSingleAttempt

expression == 
    [
        \* To hide variables of the `MCClientSubmissionContentionReserveSingleAttempt` spec from the error trace,
        \* remove the variables below.  The trace will be written in the order
        \* of the fields of this record.
        sessVer |-> sessVer
        ,tries |-> tries
        ,agentLeft |-> agentLeft
        ,snap |-> snap
        ,client |-> client
        ,created |-> created
        
        \* Put additional constant-, state-, and action-level expressions here:
        \* ,_stateNumber |-> _TEPosition
        \* ,_sessVerUnchanged |-> sessVer = sessVer'
        
        \* Format the `sessVer` variable as Json value.
        \* ,_sessVerJson |->
        \*     LET J == INSTANCE Json
        \*     IN J!ToJson(sessVer)
        
        \* Lastly, you may build expressions over arbitrary sets of states by
        \* leveraging the _TETrace operator.  For example, this is how to
        \* count the number of times a spec variable changed up to the current
        \* state in the trace.
        \* ,_sessVerModCount |->
        \*     LET F[s \in DOMAIN _TETrace] ==
        \*         IF s = 1 THEN 0
        \*         ELSE IF _TETrace[s].sessVer # _TETrace[s-1].sessVer
        \*             THEN 1 + F[s-1] ELSE F[s-1]
        \*     IN F[_TEPosition - 1]
    ]

=============================================================================



Parsing and semantic processing can take forever if the trace below is long.
 In this case, it is advised to uncomment the module below to deserialize the
 trace from a generated binary file.

\*
\*---- MODULE MCClientSubmissionContentionReserveSingleAttempt_TETrace ----
\*EXTENDS IOUtils, TLC, MCClientSubmissionContentionReserveSingleAttempt
\*
\*trace == IODeserialize("MCClientSubmissionContentionReserveSingleAttempt_TTrace_1794118123.bin", TRUE)
\*
\*=============================================================================
\*

---- MODULE MCClientSubmissionContentionReserveSingleAttempt_TETrace ----
EXTENDS TLC, MCClientSubmissionContentionReserveSingleAttempt

trace == 
    <<
    ([sessVer |-> 0,tries |-> 0,agentLeft |-> 1,created |-> 0,client |-> "idle",snap |-> 0]),
    ([sessVer |-> 0,tries |-> 1,agentLeft |-> 1,created |-> 0,client |-> "reserving",snap |-> 0]),
    ([sessVer |-> 1,tries |-> 1,agentLeft |-> 0,created |-> 0,client |-> "reserving",snap |-> 0]),
    ([sessVer |-> 1,tries |-> 1,agentLeft |-> 0,created |-> 0,client |-> "lost",snap |-> 0])
    >>
----


=============================================================================

---- CONFIG MCClientSubmissionContentionReserveSingleAttempt_TTrace_1794118123 ----
CONSTANTS
    Publications = 1
    Attempts = 4
    Redrive = TRUE
    ReserveRedrive = FALSE
    LostResponse = FALSE

INVARIANT
    _inv

CHECK_DEADLOCK
    \* CHECK_DEADLOCK off because of PROPERTY or INVARIANT above.
    FALSE

INIT
    _init

NEXT
    _next

CONSTANT
    _TETrace <- _trace

ALIAS
    _expression
=============================================================================
\* Generated on Sat Nov 07 22:08:43 PST 2026