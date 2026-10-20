/-!
# Transcript retrieval index

The retrieval index is a derived, node-local projection of canonical
transcripts: `TranscriptChunk` rows hold chunk text and a DefraDB-generated
embedding behind a DiskANN vector index. Canonical `AgentMessage` rows stay the
only writable transcript; chunks are rebuilt from them and never read back as
transcript truth.
-/

namespace Retrieval

/-- One canonical transcript message as the background indexer observes it.

`settled` holds when the message's request has a terminal `lifecycle_state`, or
when the message has no request (a fork copy). `AgentMessage` rows are
immutable, so a settled message's text never changes and indexing it once is
final. Unsettled messages belong to responses still in flight; embedding them
would put provider work on the foreground path and index partial turns. -/
structure Message where
  sequence : Nat
  settled : Bool
  deriving DecidableEq, Repr

/-- Durable per-session index progress.

`indexed` is the set of message sequences that own chunk rows. `watermark` is
the first sequence not known to be indexed: every sequence in
`[start, watermark)` is indexed. The watermark only bounds the indexer's scan;
the indexed set alone decides which messages still need work. -/
structure SessionIndex where
  indexed : List Nat
  watermark : Nat
  deriving DecidableEq, Repr

/-- Session message sequences are allocated from 1 by the writer
(`max_sequence + 1` inside the creating transaction), so a fresh session's
scan starts there. -/
def SessionIndex.initial : SessionIndex :=
  { indexed := [], watermark := 1 }

/-- Work one indexer pass performs for a session, and the progress it commits. -/
structure Step where
  next : SessionIndex
  index : List Nat
  delete : List Nat
  deriving DecidableEq, Repr

def present (msgs : List Message) : List Nat :=
  msgs.map (·.sequence)

/-- Settled messages at or above the watermark that own no chunks yet. -/
def toIndex (msgs : List Message) (st : SessionIndex) : List Nat :=
  (msgs.filter fun m =>
      m.settled && decide (st.watermark ≤ m.sequence) && decide (m.sequence ∉ st.indexed)).map
    (·.sequence)

/-- Indexed sequences whose canonical message no longer exists. Production never
deletes messages, but raw GraphQL against a served node can, and stale chunks
must not outlive their source. -/
def toDelete (msgs : List Message) (st : SessionIndex) : List Nat :=
  st.indexed.filter fun s => decide (s ∉ present msgs)

def maxOf : List Nat → Nat
  | [] => 0
  | a :: rest => max a (maxOf rest)

def advanceFuel (covered : List Nat) : Nat → Nat → Nat
  | 0, w => w
  | fuel + 1, w => if w ∈ covered then advanceFuel covered fuel (w + 1) else w

/-- The first sequence at or after `w` that is not covered. The fuel bound
exceeds every covered sequence, so the scan always ends on an uncovered one. -/
def advance (covered : List Nat) (w : Nat) : Nat :=
  advanceFuel covered (maxOf covered + 1) w

/-- One indexer pass. Chunk writes for `index` and deletes for `delete` commit
before the watermark; a crash between them replays (`crash_replay_converges`). -/
def step (msgs : List Message) (st : SessionIndex) : Step :=
  let add := toIndex msgs st
  let covered := (st.indexed ++ add).filter fun s => decide (s ∈ present msgs)
  { next := { indexed := covered, watermark := advance covered st.watermark }
  , index := add
  , delete := toDelete msgs st }

/-- Durable state after the chunk writes commit and before the watermark does. -/
def crashedAfterChunks (msgs : List Message) (st : SessionIndex) : SessionIndex :=
  { indexed := (step msgs st).next.indexed, watermark := st.watermark }

/-- Messages the session still owes the index: unsettled responses and settled
messages without chunks. A session is fresh exactly when this is zero. -/
def pending (msgs : List Message) (st : SessionIndex) : Nat :=
  (msgs.filter fun m => decide (m.sequence ∉ st.indexed)).length

/-- A ranked index candidate: a chunk's canonical message reference and the
DiskANN similarity DefraDB returned for it (higher is nearer). -/
structure Candidate where
  ref : Nat
  score : Int
  deriving DecidableEq, Repr

def ranksBefore (a b : Candidate) : Bool :=
  a.score > b.score || (a.score == b.score && a.ref ≤ b.ref)

/-- The hits a semantic search returns. A candidate survives only when its
canonical source materializes for the caller (`authorized`), so a chunk never
answers for a message the caller cannot read through the canonical reader.
Survivors are ordered nearest first, ties by reference, and cut to `limit`. -/
def selectHits (cands : List Candidate) (authorized : List Nat) (limit : Nat) :
    List Candidate :=
  ((cands.filter fun c => decide (c.ref ∈ authorized)).mergeSort ranksBefore).take limit

end Retrieval
