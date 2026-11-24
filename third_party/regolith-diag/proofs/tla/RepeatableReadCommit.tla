---- MODULE RepeatableReadCommit ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A transaction is a batch of reads and writes that must look as if it ran
\* alone. Regolith's optimistic transactions do not lock anything while they
\* run. Each one reads from a frozen picture of the database (its snapshot),
\* remembers what it read and what it wants to write, and at the very end asks
\* to commit. The commit then checks: did anybody else change, since my
\* snapshot, something I relied on? If so the transaction is refused (it
\* "aborts") and the caller runs it again. This check is "validation".
\*
\* How the checks are run. TLC, the model checker, tries every order in which
\* a handful of transactions can take their steps. A configuration file
\* (MC_*.cfg) picks the transactions and the level and names the promises to
\* check in every state. A GREEN configuration runs the rule as the engine
\* has it, and TLC must find no order that breaks a promise. A RED one plants
\* one bug (`Mutant`, the list is in RepeatableRead.tla) or runs a level that
\* does not make a promise, and TLC must find an order that breaks exactly the
\* promise it names. A RED shows that the GREEN's promise really rests on
\* the line of the rule its bug removes.
\*
\* Tiny example. Two people each read a counter that says 5 and each write
\* back 6. If both commits are let through, the counter says 6 and one
\* increment is lost. Validation refuses the second commit, because the key
\* it read has a newer write than its snapshot.
\*
\* What can go wrong, both ways:
\*   - validate too little, and a transaction commits on stale input (the
\*     lost increment above);
\*   - validate too much, and transactions that could never hurt each other
\*     are refused for nothing (two appends to different places, two writes of
\*     the very same bytes).
\*
\* How much is validated depends on the isolation level (IsolationLevel,
\* src/transaction.rs), and at DefraLevel also on what the caller declares
\* about each key (KeyClass and KeyClassifier, src/transaction/policy.rs).
\* This module writes that rule down exactly. RepeatableRead.tla, which
\* extends this one, runs workloads against it and checks that nothing stale
\* commits and nothing harmless is refused. Read its header first for the
\* workloads and the promises; this file holds what every workload shares:
\* the fixed inputs, the layout of the keys, the variables, the history of
\* committed writes, and the commit rule. The two files are one model, split
\* only to keep each readable.
\*
\* WHERE EACH RULE COMES FROM, with the code it mirrors (checked against the
\* code of this branch). Section numbers in brackets are sections of the
\* design, docs/plans/defralevel.md. A RED configuration per rule breaks it
\* on purpose (RepeatableRead.tla, MUTANTS).
\*
\*   Transaction::observe, Transaction::validation_set    src/transaction.rs
\*   TxnScanStream and its yield_cursor               src/transaction/cursor.rs
\*   scan_range::cover                            src/transaction/scan_range.rs
\*         Point reads, scan stretches, which reads a level validates, and
\*         the written keys a stretch covers.
\*   policy::run_is_commutative                       src/transaction/policy.rs
\*         A stretch inside one commutative prefix records nothing.
\*   policy::exempt_content_addressed (E17, D7)       src/transaction/policy.rs
\*         A put or merge of a content-addressed key is never validated,
\*         and neither is any read of a key the commit puts or merges. A
\*         delete of one is validated as for any key. A read of one that
\*         found it is validated for presence only; one that found nothing
\*         is validated in full.
\*   RegolithEngine::validate                         src/engine/commit/txn.rs
\*         The read loop, then the validated ranges, then the write loop.
\*         A presence-only read does not stand for a write of its key
\*         (read_stands_for, src/engine/commit/early.rs), so a deleted
\*         content-addressed key is checked by both loops.
\*   RegolithEngine::check_write              src/engine/commit/write_check.rs
\*   RegolithEngine::landed_above              src/engine/commit/terminator.rs
\*         One written key against the versions newer than the snapshot. A
\*         blind merge at DefraLevel aborts only on a newer replacement. An
\*         identical blind write is elided, except under a merge of its own
\*         batch (write_matches_committed, src/engine/mod.rs; its operand
\*         clause is left out, see Elided).
\*   ReadRule::Value, RegolithEngine::read_changed [3.5]
\*                              src/engine/read_rule.rs, commit/read_rules.rs
\*         Value-validated reads. At DefraLevel a read is current when the
\*         key's newest committed write is a put or a delete that leaves
\*         exactly the value the read returned. A newer operand on top is a
\*         change. Every other level compares sequences (ReadRule::Seq).
\*   Transaction::get_parts, ReadRule::Parts [3.3, 3.4]
\*                                                src/transaction/projection.rs
\*         Projected reads. Validated only against newer writes that change
\*         one of the named parts: a put, delete or range delete always does
\*         (unless it leaves exactly the bytes read), an operand does when
\*         MergeOperator::touches says so. In full when the read found
\*         nothing or the transaction puts the key; never with no part
\*         named. Like any read but a presence read, it stands for a write
\*         of its key, which is why the put fallback is needed.
\*   RegolithEngine::commit_write_free [3.8, E50]     src/engine/commit/mod.rs
\*   Transaction::drain_for_commit                              src/transaction.rs
\*         Write-free transactions. A commit with no put, delete, merge or
\*         append takes no part in the commit pipeline, at every level and
\*         in both flavours: it takes no sequence, checks what its level
\*         checks against the published state, commits on a database
\*         latched read-only, and its receipt names its snapshot. An
\*         optimistic DefraLevel one checks only its get_for_update reads; a
\*         pessimistic one validates as at RepeatableRead.
\*   KeyClass::Log, Transaction::append [3.2, 3.6]    src/transaction/append.rs
\*         Log keys. A read of a key of the log range is never validated at
\*         DefraLevel. Only `append` writes the log, in the ordered step
\*         (AppendOrder::number, src/engine/commit/append.rs): the position
\*         is assigned at commit, so the appending transaction never sees
\*         its own entry.
\*   RangeCheck, RegolithEngine::written_in_range_of [3.15]
\*                    src/transaction/validated_range.rs, commit/range_rule.rs
\*         Validated scans. Any newer write inside the range the scan
\*         covered conflicts, so a key that left the range (a revocation)
\*         and a key that appeared in it (a phantom) both abort the scan.
\*   KeyWrites::fold [3.9]                         src/transaction/write_buffer.rs
\*         Read-your-own-writes in operation order. A transaction's read
\*         applies its own puts, deletes and merges in the order it made
\*         them, and its commit stores the same fold. The 0.1.x engine
\*         applied a put before a merge whatever their order (MUTANT
\*         PutsBeforeMerges).
\*   E11 (#236). A merge made before a scan keeps the scan's stretch: the
\*         scan reads the operands-only key's base without recording it.
\*   Conflict [3.14]                                           src/conflict.rs
\*         Conflict reasons. An abort carries (key, mine, theirs,
\*         observed_seq, latest_seq): what this transaction did with the
\*         key, and the newer committed write that decided the race.
\*
\* THE HISTORY. `hist` holds every committed write as a record: the commit
\* sequence, the key, the kind (Put, Delete, RangeDelete, Merge: the
\* engine's WriteKind, src/conflict.rs), the value the key held right after
\* it, and for an operand the parts it touches. A commit takes one sequence
\* here where the engine gives each operation its own; a key that one commit
\* both puts and merges gets a Put and a Merge record at that sequence, the
\* Merge being the newer (Newer). Every rule below reads the history the way
\* the engine walks the versions newer than a snapshot, newest first. Each
\* step keeps only what an open snapshot can still read (Prune), as
\* compaction does.
\*
\* LEAN. proofs/lean/Regolith/Validation.lean proves, for every history
\* size, that these per-class rules accept only histories equivalent to the
\* serial one in commit order (all_classes_serial);
\* proofs/lean/Regolith/Relaxations.lean states each class's relaxation
\* exactly; proofs/lean/Regolith/MergeOperator.lean proves the merge-operator
\* laws the blind-merge and projected-read rules rest on. RepeatableRead.tla's
\* header names every theorem.

\* Borrow the standard TLA+ books: counting numbers, sizes of finite sets, lists, and TLC's helpers
\* for writing small maps (:> and @@).
EXTENDS Naturals, FiniteSets, Sequences, TLC

\* The fixed inputs a configuration (an MC_*.cfg file) chooses. Every transaction is named by a
\* number, its id.
CONSTANTS
  \* The transactions that each append one block to the Merkle DAG. An appender's id doubles as the
  \* id of its block.
  Appenders,
  \* The block that is already the DAG's only head before anything runs.
  Seed,
  \* The one transaction that sweeps away head keys a newer block superseded.
  Pruner,
  \* The one transaction that reads the collection definition and writes a document from it.
  Writer,
  \* The one transaction that replaces the definition with its next version.
  Patcher,
  \* The transactions that each apply the same block received from a peer (the remote block). May be
  \* empty.
  Mergers,
  \* The id of that remote block.
  Remote,
  \* The blocks the remote block names as its parents. Its bytes fix them, not what the local heads
  \* happen to be.
  RemoteParents,
  \* The transactions that add one to the counter with a merge and read nothing first (a blind
  \* merge). May be empty.
  Incrementers,
  \* The transactions that read the counter and then merge an increment into it. May be empty.
  ReadMergers,
  \* The transactions that put the counter to 1 and merge an increment on top, in one commit. May be
  \* empty.
  Rebasers,
  \* The transactions that delete the counter. May be empty.
  Resetters,
  \* The plain range deletes over the counter's range, made outside any transaction
  \* (Db::delete_range, src/lib.rs). May be empty.
  Clearers,
  \* The transactions that scan up to the definition and then rewrite it. May be empty.
  Migrators,
  \* The transactions of the later workloads, as one set of ids per role name (RoleNames below). A
  \* configuration picks one of the records RepeatableRead.tla defines, for example Extras <-
  \* NoExtras.
  Extras,
  \* The key ranges the caller's KeyClassifier (src/transaction/policy.rs) declares
  \* KeyClass::CommutativePrefix.
  CommutativeRanges,
  \* The key ranges it declares KeyClass::ContentAddressed: keys named after their own bytes, so one
  \* key never holds two different values.
  ContentAddressedRanges,
  \* The isolation level every transaction runs at, one of the names of IsolationLevel
  \* (src/transaction.rs): "ReadCommitted", "SnapshotIsolation", "RepeatableRead", "Serializable" or
  \* "DefraLevel". The two writers of the read-only anomaly run at SnapshotIsolation whatever it says
  \* (LevelOf below).
  Level,
  \* "none" for the real rules, or the name of the one rule a RED configuration breaks on purpose
  \* (the list is in RepeatableRead.tla, MUTANTS).
  Mutant

\* The roles of the later workloads, each a set of transaction ids.
RoleNames == {"Collectors", "Linkers", "Holders",
              "PartReaders", "PartRewriters", "PartWriters",
              "ValueReaders", "VersionWriters",
              "ReadOnlys", "Promoters", "SkewWriters", "ForUpdateReaders", "Latchers",
              "Withdrawers", "Depositors", "Reporters", "PessimisticReporters",
              "RangeReaders", "ScanReaders", "Bumpers", "Restorers",
              "LogAppenders", "LogReaders", "LogDeciders",
              "TallyMergers", "OwnWriters",
              "Checkers", "Revokers", "Granters",
              "Joiners", "Claimers", "Peekers", "NoPartDeciders"}
\* A configuration must give every role a set of transaction ids, possibly an empty one.
ASSUME Extras \in [RoleNames -> SUBSET Nat]
\* Two roles never share a transaction.
ASSUME \A r1, r2 \in RoleNames : r1 # r2 => Extras[r1] \cap Extras[r2] = {}

\* One name per role, so the rules read as the workloads are described.
\* Collectors: garbage collection. A collector deletes the shared block when nothing refers to it.
Collectors     == Extras["Collectors"]
\* Linkers: each puts the shared block again (or merges the block's own bytes into it) and writes a
\* pin that refers to it.
Linkers        == Extras["Linkers"]
\* Holders: each reads the shared block and writes down whether it was there.
Holders        == Extras["Holders"]
\* Part readers: each calls get_parts(part key, {1}) and decides on part 1 only.
PartReaders    == Extras["PartReaders"]
\* Part rewriters: each calls get_parts(part key, {1}) and then puts the whole key back.
PartRewriters  == Extras["PartRewriters"]
\* Part writers: each makes one blind write of the part key, picked when it begins.
PartWriters    == Extras["PartWriters"]
\* Value readers: each reads the version key and decides on the value it found.
ValueReaders   == Extras["ValueReaders"]
\* Version writers: each rewrites the version key, with the same bytes or with different ones.
VersionWriters == Extras["VersionWriters"]
\* Read-only transactions: optimistic, they read both skew keys and write nothing.
ReadOnlys      == Extras["ReadOnlys"]
\* Promoters: pessimistic and write-free; each reads one key later than its snapshot, with
\* get_for_update.
Promoters      == Extras["Promoters"]
\* Skew writers: each writes both skew keys in one commit.
SkewWriters    == Extras["SkewWriters"]
\* For-update readers: optimistic and write-free; each reads skew key A plainly and skew key B with
\* get_for_update, both at its snapshot (an optimistic transaction reads everything there).
ForUpdateReaders == Extras["ForUpdateReaders"]
\* Latchers: not transactions. Each is one failure (a log write that failed, a callback that
\* panicked) that latches the database read-only: from then on every commit that writes is refused.
Latchers       == Extras["Latchers"]
\* Withdrawers: the bank's withdrawal of the read-only anomaly. Each reads the checking and the
\* savings balance and takes 25 from checking, plus a fee of 1 when the total it saw is under 35.
\* Runs at SnapshotIsolation.
Withdrawers    == Extras["Withdrawers"]
\* Depositors: the bank's deposit. Each reads the savings balance and puts 20 more into it. Runs at
\* SnapshotIsolation.
Depositors     == Extras["Depositors"]
\* Reporters: the read-only report of the anomaly. Each reads both balances and writes nothing, at
\* the configuration's Level, optimistic.
Reporters      == Extras["Reporters"]
\* Pessimistic reporters: the same report made through a TransactionDb (pessimistic).
PessimisticReporters == Extras["PessimisticReporters"]
\* Range readers: each makes a validated scan (ScanCheck::Range) of the grant range and writes
\* nothing.
RangeReaders   == Extras["RangeReaders"]
\* Scan readers: each makes a plain scan of the grant range and writes nothing. At Serializable every
\* key the scan returned is a read the commit checks.
ScanReaders    == Extras["ScanReaders"]
\* Bumpers: each puts skew key B to 2, visible at once.
Bumpers        == Extras["Bumpers"]
\* Restorers: each puts skew key B back to 1, its value before anything ran; decided first and
\* visible later.
Restorers      == Extras["Restorers"]
\* Log appenders: each appends one entry to the log.
LogAppenders   == Extras["LogAppenders"]
\* Log readers: each reads the log's head, appends an entry, and stores the head it read as its
\* cursor.
LogReaders     == Extras["LogReaders"]
\* Log deciders: the misuse case. Each bases a decision on a read of the log.
LogDeciders    == Extras["LogDeciders"]
\* Tally mergers: each merges into a key of a commutative prefix and then scans that prefix.
TallyMergers   == Extras["TallyMergers"]
\* Own-writers: each does two operations on one key and then reads the key back.
OwnWriters     == Extras["OwnWriters"]
\* Checkers: each decides on everything a range holds, using a validated scan.
Checkers       == Extras["Checkers"]
\* Revokers: each deletes a key of that range.
Revokers       == Extras["Revokers"]
\* Granters: each puts a new key into that range.
Granters       == Extras["Granters"]
\* Joiners: each scans the roster, a commutative prefix, and adds a key of its own to it. It decides
\* nothing on what the scan returned: the use the prefix is declared for.
Joiners        == Extras["Joiners"]
\* Claimers: the misuse case. Each scans the roster and takes the one seat when the scan showed it
\* free: an exclusive outcome decided on a scan.
Claimers       == Extras["Claimers"]
\* Peekers: each calls get_parts(part key, {}), naming no part, and decides on nothing it read.
Peekers        == Extras["Peekers"]
\* No-part deciders: the misuse case. Each calls get_parts(part key, {}) and still decides on part 1.
NoPartDeciders == Extras["NoPartDeciders"]

\* Every transaction of the later workloads.
NewTxns == UNION {Extras[r] : r \in RoleNames}
\* The transactions that read the part key with get_parts and name no part.
NoPartTxns == Peekers \cup NoPartDeciders
\* The transactions that read the part key with get_parts.
PartTxns == PartReaders \cup PartRewriters \cup NoPartTxns

\* Every transaction that writes the counter.
CounterTxns == Incrementers \cup ReadMergers \cup Rebasers \cup Resetters \cup Clearers
\* The DefraLevel workloads of #237.
ExtraTxns   == Mergers \cup CounterTxns \cup Migrators

\* There is at least one appender, and ids are natural numbers.
ASSUME Appenders # {} /\ Appenders \subseteq Nat
\* The seed block is a number no appender uses.
ASSUME Seed \in Nat /\ Seed \notin Appenders
\* The pruner, the writer and the patcher are numbers too,
ASSUME {Pruner, Writer, Patcher} \subseteq Nat
\* three different ones,
ASSUME Cardinality({Pruner, Writer, Patcher}) = 3
\* and none of them is an appender.
ASSUME {Pruner, Writer, Patcher} \cap Appenders = {}
\* The level is one of the five the engine offers.
ASSUME Level \in {"ReadCommitted", "SnapshotIsolation", "RepeatableRead", "Serializable",
                  "DefraLevel"}
\* The mutant is "none" or one of the planted bugs, each described in RepeatableRead.tla under
\* MUTANTS.
ASSUME Mutant \in {"none", "ReadMergeBlind", "PutMergeBlind", "PutMergeElides",
                   "MergeIgnoresReplacement", "RangeDeleteNotReplacement",
                   "PolicyIgnoresRange",
                   "CaDeleteExempt", "PresenceReadFull", "PresenceUnchecked",
                   "PartsIgnorePut", "PartsIgnoreTouch", "PartsNoAbsentFallback",
                   "PartsNoPutFallback", "PartsNamesOther", "SeqOnlyValidation",
                   "WriteFreePessimistic", "WriteFreeUnchecked", "WriteFreeRefusedWhenLatched",
                   "WriteFreeTakesSequence", "ReceiptAtHorizon", "WriteFreeChecksPlainReads",
                   "WriteFreePublishedOnly", "ValueAboveHorizonRefuses",
                   "ReadCommittedChecksForUpdate", "WriteFreeIgnoresRanges",
                   "WriteFreeJudgedAtDecidedOnly",
                   "OwnAppendVisible", "MergeBeforeScanReadsBase",
                   "PutsBeforeMerges", "OwnMergesInvisible", "ReasonNewestWrite",
                   "PlainScanDecides"}
\* The remote block is a number of its own: not an appender's block and not the seed.
ASSUME Remote \in Nat /\ Remote \notin Appenders \cup {Seed}
\* Its parents are blocks that exist here.
ASSUME RemoteParents \subseteq Appenders \cup {Seed}
\* The transactions of the DefraLevel workloads of #237 are numbers,
ASSUME ExtraTxns \subseteq Nat
\* and none of them plays one of the four first roles.
ASSUME ExtraTxns \cap (Appenders \cup {Pruner, Writer, Patcher}) = {}
\* The extra workloads are pairwise disjoint.
ASSUME Cardinality(ExtraTxns) =
         Cardinality(Mergers) + Cardinality(Incrementers) + Cardinality(ReadMergers)
         + Cardinality(Rebasers) + Cardinality(Resetters) + Cardinality(Clearers)
         + Cardinality(Migrators)
\* The later workloads use ids of their own.
ASSUME NewTxns \cap (Appenders \cup {Pruner, Writer, Patcher} \cup ExtraTxns) = {}

\* Every block of the DAG: the seed, one per appender, and the remote one.
Blocks == Appenders \cup {Seed} \cup (IF Mergers = {} THEN {} ELSE {Remote})
\* The transactions of the first workloads (#237), and every transaction.
OldTxns == Appenders \cup {Pruner, Writer, Patcher} \cup ExtraTxns
\* Every transaction of the model: the first workloads' and the later ones'.
Txns    == OldTxns \cup NewTxns

----------------------------------------------------------------------------
\* THE KEY LAYOUT. Keys are <<range, p, c>>. Ranges sort by number, and
\* inside a range a key sorts by p, then c: the order a scan walks. A range
\* is also the prefix a KeyClassifier declares.
\* Range 1: the head keys of the DAG, one per block that is a head.
HeadRange    == 1
\* Range 2: markers. A marker <<2, p, c>> says block c names block p as a parent, so p is no longer
\* a head.
MarkerRange  == 2
\* Range 3: the collection definition (one key).
DefRange     == 3
\* Range 4: the document written from the definition (one key).
DocRange     == 4
\* Range 5: the counter (one key).
CounterRange == 5
\* Range 6: the blocks themselves, each stored under a key made from its content.
BlockRange   == 6
\* Range 7: a linker's pin, its reference to the shared block.
PinRange     == 7
\* Range 8: a holder's note of whether the shared block was present.
HoldRange    == 8
\* Range 9: the key that is read by parts.
PartRange    == 9
\* Range 10: a key rewritten on every save, often with the very same bytes.
VersionRange == 10
\* Range 11: two keys that are always written together.
SkewRange    == 11
\* Range 12: the commit-ordered log, its head and its entries.
LogRange     == 12
\* Range 13: a key that transactions merge into, inside a commutative prefix.
TallyRange   == 13
\* Range 14: the key the own-writes workload writes and reads back.
OwnRange     == 14
\* Range 15: grants. A decision rests on everything the range holds at once.
AclRange     == 15
\* Range 16: one output key per deciding transaction, where it writes its decision down.
OutRange     == 16
\* Range 17: the roster, a set transactions add their own keys to, with one seat in it.
RosterRange  == 17
\* Range 18: the bank of the read-only anomaly, a checking and a savings balance.
BankRange    == 18
\* Only the head, marker, tally and roster ranges may be declared commutative here.
ASSUME CommutativeRanges \subseteq {HeadRange, MarkerRange, TallyRange, RosterRange}
\* A key has one class.
\* Only the first six ranges may be declared content-addressed here,
ASSUME /\ ContentAddressedRanges \subseteq {HeadRange, MarkerRange, DefRange, DocRange,
                                             CounterRange, BlockRange}
       \* and no range is both content-addressed and commutative.
       /\ ContentAddressedRanges \cap CommutativeRanges = {}
\* The classifier always declares the log range Log: only append writes it.
LogRanges == {LogRange}

\* The key that says block b is a head.
HeadKey(b)      == <<HeadRange, b, b>>
\* The key that says block c names block p as a parent.
MarkerKey(p, c) == <<MarkerRange, p, c>>
\* The one key of the definition.
DefKey          == <<DefRange, 0, 0>>
\* The one key of the document.
DocKey          == <<DocRange, 0, 0>>
\* The one key of the counter.
CounterKey      == <<CounterRange, 0, 0>>
\* A block's content-addressed key: written once, identically by anyone.
BlockKey(b)     == <<BlockRange, b, b>>
\* The block the collector reclaims and linkers and holders refer to: the
\* seed's block, which no other workload writes.
Shared          == BlockKey(Seed)
\* Linker l's pin.
PinKey(l)       == <<PinRange, l, l>>
\* Holder h's note.
HoldKey(h)      == <<HoldRange, h, h>>
\* The one key read by parts.
PartKey         == <<PartRange, 0, 0>>
\* The one version key.
VersionKey      == <<VersionRange, 0, 0>>
\* The first of the two skew keys.
SkewA           == <<SkewRange, 0, 1>>
\* The second of the two skew keys.
SkewB           == <<SkewRange, 0, 2>>
\* The log's head holds the newest assigned position; entry p sorts after it.
LogHead         == <<LogRange, 0, 0>>
\* The log entry at position p.
LogEntry(p)     == <<LogRange, 1, p>>
\* The key the tally mergers merge into.
TallyKey        == <<TallyRange, 0, 0>>
\* The key the own-writers write and read back.
OwnKey          == <<OwnRange, 0, 0>>
\* Grant number i.
AclKey(i)       == <<AclRange, i, i>>
\* The output key of deciding transaction t.
OutKey(t)       == <<OutRange, t, t>>
\* The first key of the roster, stored from the start: a scan of the roster always yields it.
RosterLow       == <<RosterRange, 0, 0>>
\* The roster's one seat. It sorts between the first and the last key, so it lies inside the
\* stretch any scan of the roster walks.
SeatKey         == <<RosterRange, 1, 0>>
\* Joiner j's own key in the roster. It too sorts between the first and the last key.
MemberKey(j)    == <<RosterRange, 2, j>>
\* The last key of the roster, stored from the start.
RosterHigh      == <<RosterRange, 3, 0>>
\* The checking balance: 30 before anything runs.
CheckingKey     == <<BankRange, 0, 1>>
\* The savings balance: 0 (absent) before anything runs.
SavingsKey      == <<BankRange, 0, 2>>

\* Positions the log can reach: one append per appending transaction.
LogPositions == 1..Cardinality(LogAppenders \cup LogReaders)
\* The transactions that scan the roster.
RosterTxns == Joiners \cup Claimers
\* The transactions that write a decision into an output key of their own.
OutTxns == PartReaders \cup ValueReaders \cup LogReaders \cup LogDeciders \cup Checkers
           \cup Claimers \cup NoPartTxns

\* Every key any workload of this config reads or writes.
Keys == {HeadKey(b) : b \in Blocks}
        \cup {MarkerKey(p, c) : p \in Blocks, c \in Blocks}
        \cup {DefKey, DocKey, CounterKey}
        \cup {BlockKey(b) : b \in Blocks}
        \cup {PinKey(l) : l \in Linkers} \cup {HoldKey(h) : h \in Holders}
        \cup {PartKey, VersionKey, SkewA, SkewB, LogHead, TallyKey, OwnKey,
              AclKey(1), AclKey(2)}
        \cup {LogEntry(p) : p \in LogPositions}
        \cup {OutKey(t) : t \in OutTxns}
        \cup (IF RosterTxns = {} THEN {} ELSE {RosterLow, SeatKey, RosterHigh})
        \cup {MemberKey(j) : j \in Joiners}
        \cup {CheckingKey, SavingsKey}

\* Key a sorts at or before key b: by range, then p, then c.
KeyLeq(a, b) ==
  \* Its range comes first;
  \/ a[1] < b[1]
  \* or the ranges match and its second part is smaller;
  \/ a[1] = b[1] /\ a[2] < b[2]
  \* or both match and its third part is not larger.
  \/ a[1] = b[1] /\ a[2] = b[2] /\ a[3] <= b[3]

\* The first and the last key of a nonempty set, in scan order.
MinKey(S) == CHOOSE k \in S : \A j \in S : KeyLeq(k, j)
\* The last key of a nonempty set, in scan order: every other key sorts at or before it.
MaxKey(S) == CHOOSE k \in S : \A j \in S : KeyLeq(j, k)

\* A value is a natural; 0 is an absent key, and a delete intends 0. The
\* counter's value is its count, so an absent counter counts 0.
Absent == 0

\* The part key's value carries two parts as digits: part 1 is the ones
\* digit, part 2 the tens digit, so 11 is "both parts at 1". A present value
\* is never 0, since every write of the key leaves some part at least 1.
Parts == {1, 2}
\* Part p of value v, and the value whose parts are p1 and p2.
Part(v, p) == IF p = 1 THEN v % 10 ELSE v \div 10
\* The value whose part 1 is p1 and whose part 2 is p2.
PartValue(p1, p2) == p1 + 10 * p2

\* What every key holds before any commit.
InitStore ==
  \* For every key k, pick its starting value:
  [k \in Keys |->
     \* the seed's head key, the definition, the seed's block, the version key, both skew keys, the
     \* own key, grant 1 and the first and last key of the roster start at 1 (present);
     IF k \in {HeadKey(Seed), DefKey, BlockKey(Seed), VersionKey, SkewA, SkewB, OwnKey,
               AclKey(1), RosterLow, RosterHigh} THEN 1
     \* the part key starts with both parts at 1 (the value 11);
     ELSE IF k = PartKey THEN PartValue(1, 1)
     \* the checking balance starts at 30;
     ELSE IF k = CheckingKey THEN 30
     \* every other key starts absent.
     ELSE Absent]

\* The kinds of committed write (the engine's WriteKind), and those that
\* replace a key's value outright.
Kinds     == {"Put", "Delete", "RangeDelete", "Merge"}
\* The kinds that replace a key's value outright: everything but a merge operand.
Replacing == {"Put", "Delete", "RangeDelete"}

\* What a transaction did with a key, as a conflict reason names it (Access).
Accesses == {"Read", "ReadParts", "ReadPresence", "ReadForUpdate", "ScannedThenWrote",
             "ScannedRange", "Put", "Delete", "Merge"}

\* The anomalies and needless refusals the later workloads' invariants watch
\* for; RepeatableRead.tla says what each means.
FlagNames == {"HoldStale", "PartsStale", "PartsRefusedNeedlessly", "ValueStale",
              "IdenticalRewriteRefused", "Skew", "LogDecisionStale", "PhantomAppend",
              "OwnMismatch", "ScanStale", "BadReason", "WriteFreeStale", "ReadOnlyAnomaly",
              "WriteFreeRefusedNeedlessly"}

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The newest sequence a decided commit took: every commit that writes takes the next one when it
  \* is decided. The engine's decided frontier (`last_decided_seq`, src/engine/mod.rs).
  clock,
  \* What a read of each key returns right now, as a map from key to value (0 for an absent key):
  \* the database as its newest reader sees it.
  store,
  \* Every committed write still worth remembering, as a set of records (THE HISTORY above): the
  \* versions the engine keeps in its memtables and tables, each stamped with its sequence and kind
  \* (src/engine/internal_key.rs).
  hist,
  \* Where each transaction is in its life: "idle" (not begun), "open", "decided" (its commit was
  \* decided and applied, and is not yet visible to readers), "committed", "aborted" (it lost a race)
  \* or "failed" (a database latched read-only refused its writes) (a Transaction,
  \* src/transaction.rs).
  phase,
  \* The sequence each transaction took as its snapshot when it began (Transaction::snapshot_seq).
  snap,
  \* The keys each transaction read one by one and recorded, each answered as of its snapshot
  \* (Transaction::tracked, one KeyState per key).
  tracked,
  \* The stretches each transaction's scans walked, each a pair <<first key, last key>> (ScanRun,
  \* src/transaction/scan_range.rs).
  runs,
  \* The keys each transaction's commit puts or deletes (the buffered Write::Put and Write::Delete,
  \* src/transaction/write_buffer.rs).
  written,
  \* The keys each transaction's commit merges an operand into (the buffered Write::Merge, same
  \* file).
  merged,
  \* What each put or deleted key will hold after the commit (0 for a delete).
  intended,
  \* A value a transaction read and then built its write from: the definition the writer or a
  \* migrator read, or the count a read-merger read. Model bookkeeping, used only to notice a stale
  \* write.
  seen,
  \* The operations an own-writer or a part writer chose, as a list in the order it made them.
  ops,
  \* The get_for_update reads a pessimistic transaction made later than its snapshot, each as <<key,
  \* the sequence it was served at>> (KeyState::for_update, src/transaction.rs).
  forUpdate,
  \* The anomalies and needless refusals noticed so far, as a set of names from FlagNames. Model
  \* bookkeeping: the engine has no such variable.
  flags,
  \* For each committed block, the parents it recorded. Model bookkeeping for the head-set check.
  parents,
  \* TRUE once some write committed although the definition it was built from had been replaced.
  \* Model bookkeeping.
  stale,
  \* TRUE once a merger was refused although every newer write to its keys stored exactly what it
  \* writes. Model bookkeeping.
  dupRefused,
  \* TRUE once a read-merger committed although the count it read had moved. Model bookkeeping.
  wrongReceipt,
  \* TRUE once a blind increment was refused although committing it would have been correct. Model
  \* bookkeeping.
  needlessRefusal,
  \* The counter's contract, part 1: what the last put, delete or range delete stored in it. Model
  \* bookkeeping (a "ghost": the engine has no such variable).
  cbase,
  \* The counter's contract, part 2: the sequence that write committed at.
  cepoch,
  \* The counter's contract, part 3: the commit sequences of the increments counted on top of that
  \* write.
  cinc,
  \* The sequence each committed transaction's receipt carries (CommitReceipt::seq,
  \* src/transaction/receipt.rs): the one its writes took, or its snapshot when it wrote nothing.
  receipt,
  \* TRUE once a failure latched the database read-only (RegolithEngine::is_latched,
  \* src/engine/mod.rs): every commit that writes is refused from then on.
  latched,
  \* The newest sequence readers see, the published horizon (`visible_seq`, src/engine/mod.rs): a
  \* new transaction takes its snapshot here. It trails `clock` while a decided commit is not yet
  \* published.
  published

\* Every variable, so a step that changes none of them is a stutter.
vars == <<clock, store, hist, phase, snap, tracked, runs, written, merged, intended, seen,
          ops, forUpdate, flags, parents, stale, dupRefused, wrongReceipt,
          needlessRefusal, cbase, cepoch, cinc, receipt, latched, published>>

\* The keys that hold a value now.
Stored == {k \in Keys : store[k] # Absent}

----------------------------------------------------------------------------
\* THE HISTORY, read the way the engine walks a key's versions.

\* Record a is newer than record b: a later commit, or, within one commit,
\* the operand above the put it was merged onto. Records of one commit for
\* different keys are ordered by key, so the order is total.
Newer(a, b) ==
  \* a later commit is newer;
  \/ a.seq > b.seq
  \* within one commit, an operand is newer than a record that is not one (the operand was merged
  \* onto the put);
  \/ a.seq = b.seq /\ a.kind = "Merge" /\ b.kind # "Merge"
  \* within one commit, two operands or two non-operands of different keys are ordered by key. This
  \* only makes the order total.
  \/ a.seq = b.seq /\ (a.kind = "Merge") = (b.kind = "Merge")
     \* (That key comparison: b's key sorts before a's, and they differ.)
     /\ KeyLeq(b.key, a.key) /\ a.key # b.key

\* The newest record of a nonempty set of records.
Newest(S) == CHOOSE w \in S : \A v \in S : v = w \/ Newer(w, v)

\* The writes of key k committed after sequence s: what a snapshot at s
\* does not see.
After(k, s) == {w \in hist : w.key = k /\ w.seq > s}

\* What key k held at sequence s: the value its newest write at or below s
\* left, or its initial value. A read at snapshot s returns this.
ValueAt(k, s) ==
  \* S: the writes of key k at or below sequence s.
  LET S == {w \in hist : w.key = k /\ w.seq <= s}
  \* With none, the key still holds its starting value; otherwise the value the newest of them left.
  IN IF S = {} THEN InitStore[k] ELSE Newest(S).val

\* The newest committed entry of key k is an unresolved merge operand.
NewestIsOperand(k) ==
  \* S is every write of k. The key has one, and the newest is an operand.
  LET S == {w \in hist : w.key = k} IN S # {} /\ Newest(S).kind = "Merge"

\* The oldest sequence any open transaction can still read at: the oldest
\* open snapshot, or the published horizon when none is open (a transaction
\* that begins later snapshots there). ph and cl are the phases and the
\* published horizon after the step.
Horizon(ph, cl) ==
  \* open: the snapshots of the transactions still open, together with the published horizon.
  LET open == {snap[t] : t \in {u \in Txns : ph[u] = "open"}} \cup {cl}
  \* The smallest of them.
  IN CHOOSE m \in open : \A n \in open : m <= n

\* The history h with what no open snapshot can read folded away, as
\* compaction folds versions per live snapshot: every record newer than the
\* horizon stays, and of the older ones only each key's newest, which
\* ValueAt needs for a read at the horizon or later. Every rule and ghost
\* reads the history at an open transaction's anchors (all at or above the
\* horizon) or its newest entries, so this changes no verdict; it only
\* merges states that differ in history nobody can see.
Prune(h, ph, cl) ==
  \* hz: the oldest sequence anyone can still read at.
  LET hz == Horizon(ph, cl)
  \* Keep a record when it is newer than that,
  IN {r \in h : \/ r.seq > hz
                \* or when it is the newest record of its key at or below that: the one a read at
                \* the horizon returns.
                \/ r = Newest({q \in h : q.key = r.key /\ q.seq <= hz})}

----------------------------------------------------------------------------
\* One forward scan over the snapshot S, from the first key of range lo
\* through the last key of range hi.

\* The keys it yields.
Yield(S, lo, hi) == {k \in S : lo <= k[1] /\ k[1] <= hi}

\* TxnScanStream::yield_cursor: the stretch it records, first and last key,
\* none when it yields nothing.
Stretch(Y) == IF Y = {} THEN {} ELSE {<<MinKey(Y), MaxKey(Y)>>}

\* TxnScanStream::yield_cursor: the yielded keys it records as reads, only
\* where IsolationLevel::validates_scanned_keys holds. Only transactions at the configuration's Level
\* scan (the anomaly's writers, the one role with a level of its own, scan nothing), so Level is
\* their level.
ScanReads(Y) == IF Level = "Serializable" THEN Y ELSE {}

----------------------------------------------------------------------------
\* WHO CONSULTS THE CLASSIFIER.

\* The level transaction t runs at (TxnOptions::isolation, src/transaction/txn_options.rs). The
\* engine lets each transaction pick its own: the anomaly's withdrawal and deposit run at
\* SnapshotIsolation beside a report at the configuration's Level, and every other role runs at
\* Level.
LevelOf(t) == IF t \in Withdrawers \cup Depositors THEN "SnapshotIsolation" ELSE Level

\* A pessimistic transaction (TransactionDb, src/transaction.rs). At DefraLevel it validates exactly
\* as at RepeatableRead and consults no classifier [3.1].
Pessimistic(t) == t \in Promoters \cup PessimisticReporters

\* The transaction runs under DefraLevel's optimistic rules: key classes,
\* value validation, blind merges, write-free commits.
Classified(t) == LevelOf(t) = "DefraLevel" /\ ~Pessimistic(t)

\* KeyClass::ContentAddressed, applied where the classifier is consulted.
CA(t, k) == Classified(t) /\ k[1] \in ContentAddressedRanges

\* A key the classifier declares Log, applied where it is consulted.
LogKey(t, k) == Classified(t) /\ k[1] \in LogRanges

\* The transaction appends to the log at commit.
Appends(t) == t \in LogAppenders \cup LogReaders

\* Plan section [3.8]: no put, delete, merge or append (Transaction::drain_for_commit's
\* `write_free`). Such a commit takes no part in the commit pipeline, at every level.
WriteFree(t) == written[t] = {} /\ merged[t] = {} /\ ~Appends(t)

\* Plan section [3.8]: DefraLevel's write-free relaxation (drain_for_commit's `relaxed`). A
\* write-free optimistic DefraLevel transaction validates no plain read and no scan: its snapshot is
\* consistent on its own; it still checks what it read with get_for_update (KeptReads). MUTANT
\* WriteFreePessimistic extends that to a pessimistic one, which the plan rejects; MUTANT
\* WriteFreeUnchecked to a write-free transaction at any level.
ValidatesNothing(t) ==
  \* It writes nothing, and
  /\ WriteFree(t)
  \* either it runs under DefraLevel's optimistic rules,
  /\ \/ Classified(t)
     \* or it is pessimistic and the planted bug WriteFreePessimistic lets it skip validation too,
     \/ Pessimistic(t) /\ Mutant = "WriteFreePessimistic"
     \* or the planted bug WriteFreeUnchecked lets every write-free transaction skip it.
     \/ Mutant = "WriteFreeUnchecked"

\* policy::exempt_content_addressed: the content-addressed keys the commit
\* puts or merges, which nothing validates. The newest write decides, so a
\* key put (intended not absent) or merged counts; a key only deleted does
\* not. MUTANT CaDeleteExempt exempts a delete too (the rule before E17).
CaExempt(t) ==
  \* Of the keys the commit writes or merges into, those where:
  {k \in written[t] \cup merged[t] :
     \* the key is content-addressed for this transaction, and
     /\ CA(t, k)
     \* the commit merges into it,
     /\ \/ k \in merged[t]
        \* or puts it (it will hold a value),
        \/ intended[t][k] # Absent
        \* or deletes it while the planted bug CaDeleteExempt exempts deletes too.
        \/ Mutant = "CaDeleteExempt"}

----------------------------------------------------------------------------
\* THE READS A COMMIT VALIDATES. Each is a record: the key, the anchor (the
\* sequence the read was served at), the access a conflict reason names,
\* the rule that validates it, and the parts it names (all parts unless it
\* is a projected read). The rules:
\*   "seq"       any newer write conflicts (every level but DefraLevel)
\*   "value"     Plan section [3.5]: the newest newer write conflicts unless it is a
\*               put or delete leaving exactly the value read
\*   "presence"  E17: conflicts only when the newest write left the key absent
\*   "parts"     Plan section [3.4]: walk newest first to the first write that
\*               changes a named part; it conflicts unless it is a put
\*               leaving exactly the value read
Rd(k, at, mine, rule, parts) ==
  \* A record with five named fields.
  [key |-> k, at |-> at, mine |-> mine, rule |-> rule, parts |-> parts]

\* How a full read is validated: by value at DefraLevel, by sequence
\* elsewhere. MUTANT SeqOnlyValidation compares sequences at DefraLevel too.
FullRule(t) == IF Classified(t) /\ Mutant # "SeqOnlyValidation" THEN "value" ELSE "seq"

\* The read of key k at anchor at found a value.
Found(k, at) == ValueAt(k, at) # Absent

\* The class rule for one plain read (a point read, a get_for_update, or a
\* written key a scan stretch covers): the read records it keeps, none when
\* the class drops it.
\*   Log key             never validated (Plan section [3.2]).
\*   content-addressed   E17/D7: a read of a key the commit puts or merges is
\*                       dropped; otherwise one that found the key is
\*                       presence-only and one that found nothing is full.
\*     MUTANT PresenceReadFull   a read that found the key is validated in
\*                               full by sequence, as RepeatableRead validates
\*                               any read, even when the commit puts the key.
\*     MUTANT PresenceUnchecked  a read that found the key is never
\*                               validated (the rule before E17).
\*   anything else       full.
Classify(t, k, at, mine) ==
  \* A log key: no record, the read is never validated.
  IF LogKey(t, k) THEN {}
  \* A content-addressed key the read found:
  ELSE IF CA(t, k) /\ Found(k, at)
    \* under the planted bug PresenceReadFull, one full read by sequence;
    THEN CASE Mutant = "PresenceReadFull"  -> {Rd(k, at, mine, "seq", Parts)}
           \* under the planted bug PresenceUnchecked, nothing;
           [] Mutant = "PresenceUnchecked" -> {}
           \* when the commit itself puts or merges the key, nothing;
           [] k \in CaExempt(t)            -> {}
           \* otherwise a presence-only read: it fails only if the key is gone.
           [] OTHER                        -> {Rd(k, at, "ReadPresence", "presence", Parts)}
  \* A content-addressed key the read did not find, which the commit puts or merges: nothing.
  ELSE IF CA(t, k) /\ k \in CaExempt(t) THEN {}
  \* Any other read: a full read, by value at DefraLevel and by sequence elsewhere.
  ELSE {Rd(k, at, mine, FullRule(t), Parts)}

\* No get_for_update in the first workloads; the term of validation_set is
\* kept, with an empty set, so the transcription stays complete.
ForUpdate(t) == {}

\* Transaction::validation_set: the recorded point reads the level validates.
ValidatedReads(t) ==
  \* Of the keys t recorded as read, keep:
  {k \in tracked[t] :
     \* every one at RepeatableRead, Serializable and DefraLevel
     \* (IsolationLevel::validates_every_read), and, under the planted bug WriteFreeChecksPlainReads,
     \* every one of a write-free transaction at any level;
     IF \/ LevelOf(t) \in {"RepeatableRead", "Serializable", "DefraLevel"}
        \/ WriteFree(t) /\ Mutant = "WriteFreeChecksPlainReads"
     THEN TRUE
     \* at ReadCommitted only a key the commit also writes;
     ELSE IF LevelOf(t) = "ReadCommitted" THEN k \in written[t]
     \* at SnapshotIsolation a key read with get_for_update or one the commit also writes.
     ELSE k \in ForUpdate(t) \/ k \in written[t]}

\* scan_range::cover: every written or merged key inside a stretch a scan
\* walked joins the reads, anchored at the begin snapshot.
\* policy::run_is_commutative: at DefraLevel a stretch whose two ends lie in
\* one declared range is dropped before cover sees it.
Commutative(t, s) ==
  \* Only DefraLevel drops stretches.
  /\ LevelOf(t) = "DefraLevel"
  \* Under the planted bug PolicyIgnoresRange,
  /\ IF Mutant = "PolicyIgnoresRange"
       \* the stretch is dropped when either end lies in a commutative range;
       THEN s[1][1] \in CommutativeRanges \/ s[2][1] \in CommutativeRanges
       \* the real rule drops it only when both ends lie in the same commutative range.
       ELSE s[1][1] \in CommutativeRanges /\ s[2][1] = s[1][1]
\* The stretches of t that cover reads: those the policy did not drop.
CoveredRuns(t) == {s \in runs[t] : ~Commutative(t, s)}

\* Key k lies inside a stretch of t that covers reads.
Walked(t, k) == \E s \in CoveredRuns(t) : KeyLeq(s[1], k) /\ KeyLeq(k, s[2])

\* The parts a projected read names: part 1, which its decision uses.
\* MUTANT PartsNamesOther: a part reader names part 2 while deciding on
\* part 1, the caller misuse [3.4, Precondition]. A peeker and a no-part
\* decider name no part at all: get_parts(key, {}).
NamedParts(t) ==
  \* A transaction that reads by no parts names the empty set.
  IF t \in NoPartTxns THEN {}
  \* Under the planted misuse a part reader names part 2;
  ELSE IF t \in PartReaders /\ Mutant = "PartsNamesOther" THEN {2}
  \* everyone else names part 1.
  ELSE {1}

\* Plan section [3.4]: the read record of a part reader's or rewriter's get_parts.
\* At another level, or pessimistic, get_parts is a get. At DefraLevel it is
\* never validated with no part named, whatever it found: such a read leaves
\* no record at all (Transaction::read_projected). With a part named it is
\* validated in full when it found nothing (existence is part of every
\* decision on parts) or when the transaction puts the key (a put replaces
\* every part), and otherwise over its parts.
PartRead(t) ==
  \* full: the read must be validated in whole. Either it found nothing (the planted bug
  \* PartsNoAbsentFallback forgets this),
  LET full == \/ ~Found(PartKey, snap[t]) /\ Mutant # "PartsNoAbsentFallback"
              \* or the transaction puts the key (the planted bug PartsNoPutFallback forgets this).
              \/ PartKey \in written[t] /\ Mutant # "PartsNoPutFallback"
  \* A transaction that does not read by parts has no such read.
  IN IF t \notin PartTxns THEN {}
     \* Without DefraLevel's optimistic rules, get_parts is a plain get: one full read.
     ELSE IF ~Classified(t) THEN {Rd(PartKey, snap[t], "Read", FullRule(t), Parts)}
     \* No part named: the read decided nothing, so nothing is validated, found or not.
     ELSE IF NamedParts(t) = {} THEN {}
     \* A full read, reported as a read by parts.
     ELSE IF full THEN {Rd(PartKey, snap[t], "ReadParts", FullRule(t), Parts)}
     \* Otherwise a projected read over the named parts.
     ELSE {Rd(PartKey, snap[t], "ReadParts", "parts", NamedParts(t))}

\* The get_for_update reads a commit validates, each at the sequence it was served at. At
\* ReadCommitted none: validation_set keeps a read there only when the commit writes its key, and a
\* get_for_update records no write (under the planted bug ReadCommittedChecksForUpdate it keeps
\* them). Under the planted bug WriteFreeUnchecked a write-free commit keeps none either.
ForUpdateKept(t) ==
  \* None at ReadCommitted (unless the planted bug ReadCommittedChecksForUpdate keeps them), or for a
  \* write-free commit under the planted bug WriteFreeUnchecked;
  IF \/ LevelOf(t) = "ReadCommitted" /\ Mutant # "ReadCommittedChecksForUpdate"
     \* (the second case: a write-free commit under the planted bug WriteFreeUnchecked)
     \/ WriteFree(t) /\ Mutant = "WriteFreeUnchecked"
  \* keeps none;
  THEN {}
  \* otherwise each one, with the check its key's class gives it (Classify: a log key gets none, a
  \* content-addressed key that was found is checked only for presence, any other key in full).
  ELSE UNION {Classify(t, f[1], f[2], "ReadForUpdate") : f \in forUpdate[t]}

\* Every read the commit validates. A point read and a stretch over the same
\* written key are one read, recorded as the point read.
KeptReads(t) ==
  \* A transaction under the write-free relaxation keeps only its get_for_update reads
  \* (drain_for_commit's `retain` of Access::ReadForUpdate).
  IF ValidatesNothing(t) THEN ForUpdateKept(t)
  \* The point reads the level validates, each run through the class rule;
  ELSE UNION {Classify(t, k, snap[t], "Read") : k \in ValidatedReads(t)}
       \* plus the written or merged keys a covered stretch walked over that were not point reads;
       \cup UNION {Classify(t, k, snap[t], "ScannedThenWrote") :
                     k \in {j \in written[t] \cup merged[t] : Walked(t, j)} \ ValidatedReads(t)}
       \* plus the get_for_update reads, each at the sequence it was served at;
       \cup ForUpdateKept(t)
       \* plus the read by parts.
       \cup PartRead(t)

\* The keys whose read stands for a write of the key (read_stands_for,
\* src/engine/commit/early.rs): every kept read but a presence-only one.
StandingKeys(t) == {r.key : r \in {q \in KeptReads(t) : q.rule # "presence"}}

\* RegolithEngine::validate, the write loop: a merged key takes the blind-merge
\* path only when no read of it stands for the write and the batch neither
\* puts nor deletes it (`Replaced`), its put being validated as a write.
BlindMerged(t) ==
  \* The merged keys, minus the keys a read stands for (the planted bug ReadMergeBlind forgets
  \* this),
  merged[t] \ ((IF Mutant = "ReadMergeBlind" THEN {} ELSE StandingKeys(t))
               \* minus the keys the batch also puts or deletes (the planted bug PutMergeBlind
               \* forgets this).
               \cup (IF Mutant = "PutMergeBlind" THEN {} ELSE written[t]))

----------------------------------------------------------------------------
\* THE NEWER WRITE THAT DECIDES. Each operator returns the set holding the
\* record that makes a check fail, empty when the check passes.

\* Plan section [3.5]: a put, delete or range delete that leaves exactly the value
\* the read returned is not a change.
Identical(w, r) == w.kind \in Replacing /\ w.val = ValueAt(r.key, r.at)

\* Plan section [3.4]: a newer write changes one of the parts P. MUTANT
\* PartsIgnorePut forgets that a put, delete or range delete always does;
\* MUTANT PartsIgnoreTouch forgets the operands that touch P.
Decisive(w, P) ==
  \* A put, delete or range delete always changes every part (the planted bug PartsIgnorePut forgets
  \* this).
  \/ w.kind \in Replacing /\ Mutant # "PartsIgnorePut"
  \* A merge operand changes the parts when it touches one of P (the planted bug PartsIgnoreTouch
  \* forgets this).
  \/ w.kind = "Merge" /\ w.touch \cap P # {} /\ Mutant # "PartsIgnoreTouch"

\* The verdict of one rule over the writes `newer` of the key after the read: the record that
\* makes the read stale, or none.
RuleDecider(r, newer) ==
  \* Nothing newer: the read stands.
  IF newer = {} THEN {}
  \* Rule "seq": any newer write fails it, and the newest is named.
  ELSE CASE r.rule = "seq" -> {Newest(newer)}
         \* Rule "value": it fails unless the newest newer write leaves exactly the value read.
         [] r.rule = "value" ->
              IF Identical(Newest(newer), r) THEN {} ELSE {Newest(newer)}
         \* Rule "presence": it fails only when the newest newer write removed the key.
         [] r.rule = "presence" ->
              IF Newest(newer).kind \in {"Delete", "RangeDelete"} THEN {Newest(newer)} ELSE {}
         \* Rule "parts":
         [] r.rule = "parts" ->
              \* D: the newer writes that change a named part.
              LET D == {w \in newer : Decisive(w, r.parts)}
              \* It fails unless there is none, or the newest of them is a put leaving exactly the
              \* value read.
              IN IF D = {} \/ Identical(Newest(D), r) THEN {} ELSE {Newest(D)}

\* The record that fails read r, at the two points a check judges it at (write_free.rs): with every
\* decided commit counted (`clock`), the point a commit through the pipeline would be decided at,
\* and at the published point (`published`), where a write-free commit is placed. A writing commit
\* is checked while nothing is decided and unpublished, so for it the two points are one.
ReadDecider(r) ==
  \* decided: every write of the key after the read, published or only decided; under the planted
  \* bug WriteFreePublishedOnly, only the published ones.
  LET decided   == {w \in After(r.key, r.at) :
                      Mutant # "WriteFreePublishedOnly" \/ w.seq <= published}
      \* visible: the published writes of the key after the read.
      visible   == {w \in decided : w.seq <= published}
      \* atDecided: what the rule says with every decided write counted.
      atDecided == RuleDecider(r, decided)
  \* Under the planted bug ValueAboveHorizonRefuses, any write above the published point refuses a
  \* read checked by value, a rewrite of the same bytes included (the rule of 5791d6a).
  IN IF Mutant = "ValueAboveHorizonRefuses" /\ r.rule = "value"
        \* (some counted write lies above the published point)
        /\ \E w \in decided : w.seq > published
     \* then the newest counted write is named.
     THEN {Newest(decided)}
     \* Refused with every decided write counted: as strict as the pipeline.
     ELSE IF atDecided # {} THEN atDecided
     \* A read checked by sequence that holds there holds at the published point too. Under the
     \* planted bug WriteFreeJudgedAtDecidedOnly every rule stops here, unjudged at the published
     \* point.
     ELSE IF r.rule = "seq" \/ Mutant = "WriteFreeJudgedAtDecidedOnly" THEN {}
     \* Any other rule is judged at the published point as well.
     ELSE RuleDecider(r, visible)

\* write_matches_committed: the write stores what the key already holds, and
\* the batch merges nothing on top of it. The engine also refuses an elision
\* when the key's newest entry is an unresolved merge operand. That is not
\* modeled. In the first workloads it cannot change an outcome: a counter
\* with an operand on top holds at least one, which a reset's delete never
\* equals, and a rebase's put is already refused elision by the merge its own
\* batch carries; Red_PutMergeElides shows that line only without it. In the
\* later workloads leaving it out only allows more elisions; every GREEN
\* config was also run with it, and stays GREEN.
Elided(t, k) ==
  \* The write would store exactly what the key holds now,
  /\ intended[t][k] = store[k]
  \* and the batch merges nothing on top of it (the planted bug PutMergeElides forgets this).
  /\ k \notin merged[t] \/ Mutant = "PutMergeElides"

\* The written keys the write loop checks: not exempt, not standing behind
\* a read, not blind-merged.
WriteChecked(t) == written[t] \ (StandingKeys(t) \cup BlindMerged(t) \cup CaExempt(t))

\* The record that fails a blind merge into k. At DefraLevel
\* (IsolationLevel::blind_merges_commute) only a newer replacement does
\* (RegolithEngine::landed_above), walking past newer operands; elsewhere any
\* newer write. MUTANT RangeDeleteNotReplacement: a range tombstone is not
\* a replacement here.
MergeDecider(t, k) ==
  \* newer: the writes of k after t's snapshot.
  LET newer == After(k, snap[t])
      \* repl: those of them that replace the value outright
      repl  == {w \in newer : /\ w.kind \in Replacing
                              \* (a range delete is forgotten under the planted bug
                              \* RangeDeleteNotReplacement).
                              /\ ~(w.kind = "RangeDelete" /\ Mutant = "RangeDeleteNotReplacement")}
  \* Under DefraLevel's optimistic rules,
  IN IF Classified(t)
       \* only the newest newer replacement fails the merge (nothing at all under the planted bug
       \* MergeIgnoresReplacement);
       THEN IF Mutant = "MergeIgnoresReplacement" \/ repl = {} THEN {} ELSE {Newest(repl)}
       \* at any other level, any newer write does.
       ELSE IF newer = {} THEN {} ELSE {Newest(newer)}

\* Plan section [3.15]: the records that fail a validated scan: any write newer than
\* the snapshot inside the range it covered, decided or published. A checker, or a range reader
\* that writes nothing, consumes its whole range, so the range covered is all of it. MUTANT
\* PlainScanDecides: the checker decides on a plain scan, which validates no range. MUTANT
\* WriteFreeIgnoresRanges: a write-free commit skips its validated scans.
ScanDecider(t) ==
  \* newer: every write inside the grant range after t's snapshot (only the published ones under
  \* the planted bug WriteFreePublishedOnly).
  LET newer == {w \in hist : /\ w.key[1] = AclRange /\ w.seq > snap[t]
                             /\ (Mutant # "WriteFreePublishedOnly" \/ w.seq <= published)}
  \* Nothing fails when t makes no validated scan, or decides on a plain scan (the planted bug), or
  \* validates nothing, or skips its scans as a write-free commit (the planted bug), or nothing
  \* landed;
  IN IF \/ t \notin Checkers \cup RangeReaders
        \* (a checker deciding on a plain scan, or a transaction that validates nothing)
        \/ Mutant = "PlainScanDecides" \/ ValidatesNothing(t)
        \* (a write-free commit skipping its scans, the planted bug)
        \/ WriteFree(t) /\ Mutant = "WriteFreeIgnoresRanges"
        \* (nothing landed in the range)
        \/ newer = {}
       \* otherwise the newest such write fails the scan.
       THEN {} ELSE {Newest(newer)}

----------------------------------------------------------------------------
\* THE COMMIT DECISION. A candidate is one failed check: the key, what this
\* transaction did with it, the anchor it was checked at, the parts it
\* named, and the deciding record.
Cand(k, mine, at, parts, w) == [key |-> k, mine |-> mine, at |-> at, parts |-> parts, w |-> w]

\* RegolithEngine::validate, the read loop: every kept read whose key is not
\* blind-merged.
ReadCands(t) ==
  \* One candidate per kept read whose check failed, leaving out the keys that take the blind-merge
  \* path.
  UNION {{Cand(r.key, r.mine, r.at, r.parts, w) : w \in ReadDecider(r)} :
           r \in {q \in KeptReads(t) : q.key \notin BlindMerged(t)}}

\* RegolithEngine::validate, the write loop with `writes_at` the begin snapshot.
\* A content-addressed key the classifier exempts is skipped.
WriteCands(t) ==
  \* One candidate per checked written key that has a newer write and is not elided: a Put or a
  \* Delete, checked at the snapshot, blaming the key's newest newer write.
  {Cand(k, IF intended[t][k] = Absent THEN "Delete" ELSE "Put", snap[t], Parts,
        Newest(After(k, snap[t]))) :
     k \in {j \in WriteChecked(t) : After(j, snap[t]) # {} /\ ~Elided(t, j)}}

\* The same loop for a key the commit only merges into: never elided.
MergeCands(t) ==
  \* One candidate per blind-merged key (not exempt) whose merge check failed.
  UNION {{Cand(k, "Merge", snap[t], Parts, w) : w \in MergeDecider(t, k)} :
           k \in BlindMerged(t) \ CaExempt(t)}

\* Plan section [3.15]: a failed validated scan names the key that landed in the range.
ScanCands(t) == {Cand(w.key, "ScannedRange", snap[t], Parts, w) : w \in ScanDecider(t)}

\* Every failed check of t's commit.
Candidates(t) == ReadCands(t) \cup WriteCands(t) \cup MergeCands(t) \cup ScanCands(t)

\* The commit aborts when any check failed.
Conflicts(t) == Candidates(t) # {}

\* Plan section [3.14]: the reason an abort carries. The engine reports the first
\* failed check in its loop order; any failed check is a valid reason, so the
\* model picks one. `theirs` and `latest` come from the deciding record.
\* MUTANT ReasonNewestWrite reports the key's newest write instead, which is
\* not the one that decided when newer writes that decide nothing (an
\* operand over a replacement, for a blind merge) sit above it.
\* The reason t's abort carries.
ReasonOf(t) ==
  \* c: any one failed check.
  LET c == CHOOSE c \in Candidates(t) : TRUE
      \* w: the write it blames: the deciding record, or under the planted bug ReasonNewestWrite the
      \* key's newest write.
      w == IF Mutant = "ReasonNewestWrite" THEN Newest(After(c.key, c.at)) ELSE c.w
  \* The reason: the key, what t did with it, the kind of the other write, where t looked, the other
  \* write's sequence, and the parts t named.
  IN [key |-> c.key, mine |-> c.mine, theirs |-> w.kind, observed |-> c.at,
      latest |-> w.seq, parts |-> c.parts]

----------------------------------------------------------------------------
\* WHAT A COMMIT WRITES.

\* An own-writer's operations folded onto value v in operation order
\* [3.9]: a put stores 5, a delete removes the key, a merge adds one.
RECURSIVE Fold(_, _)
\* Fold the list s of operations onto the value v, first operation first.
Fold(s, v) ==
  \* No operation left: the value is the answer.
  IF s = <<>> THEN v
  \* Otherwise apply the first one (a put stores 5, a delete leaves the key absent, a merge adds
  \* one) and go on with the rest.
  ELSE Fold(Tail(s), CASE Head(s) = "put" -> 5 [] Head(s) = "delete" -> Absent
                       [] Head(s) = "merge" -> v + 1)

\* The same operations with every put and delete first and every merge after
\* them: what a 0.1.x commit applies (MUTANT PutsBeforeMerges).
PutsFirst(s) ==
  \* reps: the puts and deletes, in order.
  LET reps == SelectSeq(s, LAMBDA o : o # "merge")
      \* mrgs: the merges, in order.
      mrgs == SelectSeq(s, LAMBDA o : o = "merge")
  \* Only the last put or delete counts, and every merge is applied after it.
  IN (IF reps = <<>> THEN <<>> ELSE <<reps[Len(reps)]>>) \o mrgs

\* The operations an own-writer's commit applies.
CommitOps(t) == IF Mutant = "PutsBeforeMerges" THEN PutsFirst(ops[t]) ELSE ops[t]

\* The parts a part writer's operand touches; any other operand touches every
\* part (MergeOperator::touches defaults to true).
Touch(t) ==
  \* A part writer that chose "touch1": part 1 only.
  IF t \in PartWriters /\ ops[t] = <<"touch1">> THEN {1}
  \* A part writer that chose "touch2": part 2 only.
  ELSE IF t \in PartWriters /\ ops[t] = <<"touch2">> THEN {2}
  \* Anyone else: every part.
  ELSE Parts

\* One operand of t applied to value v of key k: on the part key, one more
\* on each part it touches; on a content-addressed block, the block's own
\* bytes (an idempotent operand); on anything else, one more.
Operand(t, k, v) ==
  \* On the part key: one more on each part the operand touches.
  CASE k = PartKey ->
         PartValue(Part(v, 1) + (IF 1 \in Touch(t) THEN 1 ELSE 0),
                   Part(v, 2) + (IF 2 \in Touch(t) THEN 1 ELSE 0))
    \* On a content-addressed block: the block's own bytes, whatever was there.
    [] k[1] = BlockRange -> 1
    \* On any other key: one more.
    [] OTHER -> v + 1

\* A put or delete first, then a merge on top of what it left.
Applied(t, k) ==
  \* An own-writer's key: its operations folded onto what the key holds.
  IF t \in OwnWriters /\ k = OwnKey THEN Fold(CommitOps(t), store[k])
  \* base: what a put or delete of this commit leaves, or else what the key holds.
  ELSE LET base == IF k \in written[t] THEN intended[t][k] ELSE store[k]
       \* A merge of this commit goes on top of the base.
       IN IF k \in merged[t] THEN Operand(t, k, base) ELSE base

\* Plan section [3.6]: the position an append takes, assigned in the ordered step
\* from the head the view holds now, dense from 1.
AppendPos == store[LogHead] + 1
\* The keys an append writes: its entry and the head.
AppendKeys(t) == IF Appends(t) THEN {LogEntry(AppendPos), LogHead} ELSE {}
\* An entry holds its appender's id; the head holds the newest position.
AppendValue(t, k) == IF k = LogHead THEN AppendPos ELSE t

\* What key k holds after t commits.
NewValue(t, k) ==
  \* A key the append writes gets the append's value.
  IF k \in AppendKeys(t) THEN AppendValue(t, k)
  \* A key the commit puts, deletes or merges gets the applied value.
  ELSE IF k \in written[t] \cup merged[t] THEN Applied(t, k)
  \* Every other key keeps what it holds.
  ELSE store[k]

\* The records a commit at sequence n adds to the history: a Put or Delete
\* per written key, a Merge per merged key (newer than a put of the same
\* key), and the append's two puts.
Records(t, n) ==
  \* One Put or Delete record per written key, holding what the key is set to;
  {[seq |-> n, key |-> k, kind |-> IF intended[t][k] = Absent THEN "Delete" ELSE "Put",
    val |-> intended[t][k], touch |-> Parts] : k \in written[t]}
  \* one Merge record per merged key, holding the value after the merge and the parts the operand
  \* touches;
  \cup {[seq |-> n, key |-> k, kind |-> "Merge", val |-> Applied(t, k), touch |-> Touch(t)] :
          k \in merged[t]}
  \* and one Put record per key the append writes.
  \cup {[seq |-> n, key |-> k, kind |-> "Put", val |-> AppendValue(t, k), touch |-> Parts] :
          k \in AppendKeys(t)}

====
