---- MODULE MCClientSubmissionContentionShortBudget_TTrace_1794118124 ----
EXTENDS Sequences, MCClientSubmissionContentionShortBudget, TLCExt, Toolbox, Naturals, TLC

_expression ==
    LET MCClientSubmissionContentionShortBudget_TEExpression == INSTANCE MCClientSubmissionContentionShortBudget_TEExpression
    IN MCClientSubmissionContentionShortBudget_TEExpression!expression
----

_trace ==
    LET MCClientSubmissionContentionShortBudget_TETrace == INSTANCE MCClientSubmissionContentionShortBudget_TETrace
    IN MCClientSubmissionContentionShortBudget_TETrace!trace
----

_inv ==
    ~(
        TLCGet("level") = Len(_TETrace)
        /\
        sessVer = (4)
        /\
        tries = (4)
        /\
        agentLeft = (0)
        /\
        created = (0)
        /\
        client = ("lost")
        /\
        snap = (3)
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
    \*         IN J!JsonSerialize("MCClientSubmissionContentionShortBudget_TTrace_1794118124.json", _TETrace)

=============================================================================

 Note that you can extract this module `MCClientSubmissionContentionShortBudget_TEExpression`
  to a dedicated file to reuse `expression` (the module in the 
  dedicated `MCClientSubmissionContentionShortBudget_TEExpression.tla` file takes precedence 
  over the module `MCClientSubmissionContentionShortBudget_TEExpression` below).

---- MODULE MCClientSubmissionContentionShortBudget_TEExpression ----
EXTENDS Sequences, MCClientSubmissionContentionShortBudget, TLCExt, Toolbox, Naturals, TLC

expression == 
    [
        \* To hide variables of the `MCClientSubmissionContentionShortBudget` spec from the error trace,
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
\*---- MODULE MCClientSubmissionContentionShortBudget_TETrace ----
\*EXTENDS IOUtils, MCClientSubmissionContentionShortBudget, TLC
\*
\*trace == IODeserialize("MCClientSubmissionContentionShortBudget_TTrace_1794118124.bin", TRUE)
\*
\*=============================================================================
\*

---- MODULE MCClientSubmissionContentionShortBudget_TETrace ----
EXTENDS MCClientSubmissionContentionShortBudget, TLC

trace == 
    <<
    ([sessVer |-> 0,tries |-> 0,agentLeft |-> 4,created |-> 0,client |-> "idle",snap |-> 0]),
    ([sessVer |-> 0,tries |-> 1,agentLeft |-> 4,created |-> 0,client |-> "reserving",snap |-> 0]),
    ([sessVer |-> 1,tries |-> 1,agentLeft |-> 3,created |-> 0,client |-> "reserving",snap |-> 0]),
    ([sessVer |-> 1,tries |-> 2,agentLeft |-> 3,created |-> 0,client |-> "reserving",snap |-> 1]),
    ([sessVer |-> 2,tries |-> 2,agentLeft |-> 2,created |-> 0,client |-> "reserving",snap |-> 1]),
    ([sessVer |-> 2,tries |-> 3,agentLeft |-> 2,created |-> 0,client |-> "reserving",snap |-> 2]),
    ([sessVer |-> 3,tries |-> 3,agentLeft |-> 1,created |-> 0,client |-> "reserving",snap |-> 2]),
    ([sessVer |-> 3,tries |-> 4,agentLeft |-> 1,created |-> 0,client |-> "reserving",snap |-> 3]),
    ([sessVer |-> 4,tries |-> 4,agentLeft |-> 0,created |-> 0,client |-> "reserving",snap |-> 3]),
    ([sessVer |-> 4,tries |-> 4,agentLeft |-> 0,created |-> 0,client |-> "lost",snap |-> 3])
    >>
----


=============================================================================

---- CONFIG MCClientSubmissionContentionShortBudget_TTrace_1794118124 ----
CONSTANTS
    Publications = 4
    Attempts = 4
    Redrive = TRUE
    ReserveRedrive = TRUE
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
\* Generated on Sat Nov 07 22:08:44 PST 2026