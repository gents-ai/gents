---- MODULE BackupSeal ----
\* Backups of a database encrypted at rest, and their restores (plan 4.12,
\* D57).
\*
\* THE STORY. regolith's BackupEngine (src/backup.rs) copies a database's
\* tables into a shared pool, one file per table named by its content, and
\* writes one small metadata file per backup saying which tables it holds,
\* where each goes and which keys each one spans. On an encrypted database
\* the tables are already sealed, but before D57 that metadata was plain
\* text: anyone with the backup disk could read the smallest and largest
\* key of every table, and a restore wrote a plain MANIFEST that said the
\* same. D57 seals the metadata through the database's own key provider.
\*
\* Picture one database, key 1 current, two tables. Backup 1 copies both
\* tables and writes its metadata sealed under key 1. The operator rotates
\* to key 2 and takes backup 2. Months later someone restores backup 1 with
\* a provider whose current key is 2. Things that can go wrong:
\*   - the metadata, or the restored MANIFEST, is written in plain text;
\*   - the restore opens backup 1 under the CURRENT key (2), not the key the
\*     file names (1), and refuses a provider that has every key it needs;
\*   - someone edits the plain listing (which names the shared files) so it
\*     points at another backup's copy of a table, or renames backup 2's
\*     file to backup 1's name, and the restore puts back the wrong tables;
\*   - the restore refuses (no key, a wrong key, a missing key) only after
\*     it has already copied tables into the target;
\*   - backup 2, sealed under key 2, still lists a table under key 1, and
\*     the provider has the right bytes for key 2 and other bytes under
\*     key 1: the restore sees key 1 is there, writes the MANIFEST and
\*     returns Ok, and the target then fails to open, and refuses a second
\*     restore because it holds a MANIFEST;
\*   - deleting a backup removes a shared table a sealed backup still needs,
\*     because the engine holds no key and skipped what it could not read;
\*   - a power cut leaves metadata that lists a table the pool never got,
\*     or a MANIFEST naming a table the target never got;
\*   - a restore runs over a target that is already a database;
\*   - backup 2's metadata stops reading (a flipped bit, a file a newer
\*     build wrote, a disk that fails the read) and the listing quietly
\*     leaves it out, so whoever reads the list believes it has every
\*     backup when it does not; or the listing needs a key, and leaves out
\*     every sealed backup;
\*   - a delete counts only the listings it can read, and removes the
\*     tables unreadable backup 2 lists, which a newer build, or the disk
\*     once it reads again, would need;
\*   - a delete removes backup 1's metadata, its sync of meta/ fails, and
\*     the call is run again: the file is already gone, so the second call
\*     skips the sync and removes backup 1's tables, and a power cut then
\*     brings the unsynced removal back undone: backup 1 is listed again,
\*     and its tables are gone.
\*
\* WHAT THE CODE DOES (each line below is one of these, by name).
\*   create_backup resolves the sealer for the current key first, copies
\*     every table it lists into the pool (each copy is written to a staging
\*     name, synced, renamed and its directory synced, so it lands whole or
\*     not at all), and only then writes the metadata the same way, sealed
\*     under that key and naming it.
\*   The metadata keeps the listing (each table's content id and size, which
\*     the pool's file names show anyway) in the clear, so listing and
\*     deleting need no key, and binds it with the tag: the tag's associated
\*     data is the backup id and every clear byte (src/backup/format.rs).
\*   restore refuses a target that holds a MANIFEST, then reads the metadata
\*     and checks, before it writes anything: a provider is given, it has the
\*     key the file names, the tag holds (right key bytes, listing unchanged,
\*     the file is the one this id wrote), and it has its current key and
\*     every key a table names, with the bytes that sealed the table: it
\*     authenticates each table's footer under its key
\*     (sstable::authenticate_footer). Then it copies every listed table
\*     into the target, and writes the MANIFEST last, sealed under the
\*     current key.
\*   list_backups names every metadata file in meta/ by its backup id, with
\*     its summary when its metadata reads and the reason when it does not.
\*     It reads only the clear listing, so it needs no key.
\*   delete_backup removes one backup's metadata, purge_old_backups the
\*     metadata of every backup but the newest `keep` by id, readable or
\*     not. Then both sweep the pool (collect_shared,
\*     src/backup/collect.rs), which starts with a sync of meta/ and removes
\*     nothing unless it succeeds: a removal this call made, or one an
\*     earlier call made whose own sync failed, is then durable before any
\*     pool file goes. Until that sync a power cut may undo a removal. The
\*     walk of shared/ hands the pool files out
\*     a batch of Sweep at a time; for each batch the sweep walks meta/ one
\*     entry at a time (Env::read_dir hands entries out a batch of Batch at
\*     a time, the way readdir fills its buffer), reads each listing as the
\*     walk reaches it, sealed or not, and strikes every candidate it names;
\*     only once that walk has handed out its last entry does it remove the
\*     candidates left, and go on to the next batch. So it removes every
\*     pool file no remaining listing names (those the deleted backups held,
\*     and those a backup cut short copied and never listed), holding one
\*     batch of candidates and one listing, never the pool and never every
\*     listing. A listing that cannot be read stops the sweep: it removes
\*     nothing more.
\*
\* A SWEEP, IN ONE PICTURE. Backups 1 and 3 share table t; backup 2 does not
\* list it. Delete backup 1: t is a candidate in some batch, and that
\* batch's walk of meta/ hands out backup 2's file, then backup 3's.
\* Reading 2 strikes nothing, reading 3 strikes t, and t stays. Were the
\* walk to lose backup 3's file where one batch of entries ends and the next
\* begins, t would never be struck, and the delete would remove a table
\* backup 3 still lists. Were the walk of shared/ to lose the table that
\* starts a batch of candidates, that table would never be a candidate, and
\* a table nobody lists would stay in the pool for ever.
\*
\* WHAT IS CHECKED.
\*   SealedMetadata        no backup's metadata, and no restored MANIFEST, is
\*                         plain text.
\*   RightKeysRestore      a restore never refuses a provider that has every
\*                         key the backup needs, after a rotation too.
\*   FaithfulRestore       a finished restore holds exactly the tables the
\*                         backup recorded when it was taken.
\*   RefusalWritesNothing  a refused restore wrote nothing.
\*   RestoredOpens         a finished restore names only tables the
\*                         provider it was given opens.
\*   ListedRestores        every table an untampered backup lists is in the
\*                         pool, after any delete and any power cut, even
\*                         while its metadata does not read.
\*   ListingNamesEvery     the listing names every backup the repository
\*                         holds: readable or not, sealed or not.
\*   CollectionBounded     the sweep's candidates are only ever one batch of
\*                         pool files: it holds that, however many backups
\*                         and tables there are.
\*   SweepRemovesEveryUnlisted  a sweep that finishes leaves no pool file
\*                         that no remaining listing names.
\*
\* THE DEFECTS, one per Mutant value, and the promise each breaks.
\*   "PlainMeta"         the metadata is written plain.     SealedMetadata
\*   "PlainRestore"      the restored MANIFEST is plain.     SealedMetadata
\*   "OpenUnderCurrent"  the metadata is opened under the current key.
\*                                                          RightKeysRestore
\*   "ListingUnbound"    the tag does not cover the listing. FaithfulRestore
\*   "IdUnbound"         the tag does not cover the id.      FaithfulRestore
\*   "ManifestFirst"     the MANIFEST before the tables.     FaithfulRestore
\*   "OverDatabase"      a restore over a finished target.   FaithfulRestore
\*   "CheckAfterCopy"    key checks after the copies.   RefusalWritesNothing
\*   "FooterUnchecked"   a table's key is checked for presence only, not
\*                       against the table, as restore did before.
\*                                                          RestoredOpens
\*   "GcSkipsSealed"     a delete counts only plain listings.  ListedRestores
\*   "GcSkipsUnreadable" a delete counts only listings that read.
\*                                                          ListedRestores
\*   "MetaFirst"         metadata before the tables.          ListedRestores
\*   "ListSkipsUnreadable" the listing leaves out a file that does not read,
\*                       as list_backups did before.      ListingNamesEvery
\*   "ListNeedsKey"      the listing leaves out a sealed file. ListingNamesEvery
\*   "StreamSkips"       the walk of meta/ loses the first entry of each
\*                       batch after the first.                ListedRestores
\*   "SweepSkips"        the walk of shared/ loses the pool file that starts
\*                       each batch of candidates after the first.
\*                                                SweepRemovesEveryUnlisted
\*   "SyncOnlyRemoved"   meta/ is synced only when this call removed a file,
\*                       as the code did before.          ListedRestores
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - Bytes. A table is its content id <<t, k>>: table slot t sealed under
\*     key k. Two copies of one table under one key are one pool file, as
\*     content addressing makes them.
\*   - The checksum in front of the tag. It catches accidental damage with
\*     no key; a deliberate change keeps it valid, which is the case here.
\*   - Each copy's own crash steps. Every file is written to a staging name,
\*     synced and renamed, so a power cut leaves it whole or absent: one step
\*     here. A power cut is the Crash step, which drops what was in flight.
\*     A removal is the one write that is not durable at once: it waits for
\*     the sync of its directory, and the Crash step undoes every removal of
\*     metadata not synced yet (`pend`), the worst a power cut can do. The
\*     removals of pool files are synced before the sweep returns, and no
\*     rule here is broken by bringing one back (Collect).
\*   - Unencrypted databases: their backups stay plain, as they always were,
\*     and none of these rules is about them.
\*   - Why a file does not read. A flipped bit (the checksum fails), a
\*     version a newer build wrote, and a disk that fails the read all look
\*     the same to the engine: the metadata does not decode. One flag,
\*     `reads`, stands for all three.
\*   - The order the walks hand entries out in. A directory picks its own
\*     order; here the walk of shared/ hands pool files out by slot and key,
\*     and the walk of meta/ hands listings out by id. Each batch of
\*     candidates is checked against every listing, so the order of either
\*     walk changes no decision the sweep makes: the Lean proof shows it for
\*     every order and every batching (collect_stream_eq, sweep_stream_eq).
\*     The REDs that lose an entry at a boundary still lose one here.
\*   - The walk of meta/ that breaks part way: the sweep stops and removes
\*     nothing more, as for a listing that does not read, which stands for
\*     both.
\*
\* CONFIGURATIONS.
\*   MC_BackupSeal_Green                   every invariant holds: one
\*                                         table, two backups, two restores,
\*                                         deletes and purges, no attacker
\*   MC_BackupSeal_Green_Attack            every invariant holds: one
\*                                         table, two backups, a restore
\*                                         under the attacker, no delete
\*   MC_BackupSeal_Green_Stream            every invariant holds with one
\*                                         table, three backups and a batch
\*                                         boundary after every entry of the
\*                                         walk of meta/, no attacker
\*   MC_BackupSeal_Green_Sweep             every invariant holds with one
\*                                         table, two backups, the attacker
\*                                         and a batch boundary after every
\*                                         pool file of the sweep
\*   MC_BackupSeal_Green_SweepTwoTables    the same sweep with two tables
\*                                         and no attacker
\*   MC_BackupSeal_Red_<defect>            the promise named above fails
\*   Every GREEN run's deletes and purges may find their sync of meta/
\*   failing, and be run again.
\*
\* The Lean proof Regolith/BackupSeal.lean proves the rules for every number
\* of backups and tables, every batching and every order: collect_keeps_listed,
\* collect_stops_on_unreadable and collect_removes_every_unlisted for one
\* collection; collect_stream_eq, stream_keeps_listed and
\* stream_holds_one_batch for one batch read listing by listing; sweep_eq
\* and sweep_removes_every_unlisted for the sweep, batch after batch; and
\* list_pages_all for the listing a page of ids at a time. The StreamSkips,
\* SweepSkips and page-boundary counterexamples are
\* stream_skip_removes_a_listed_file, sweep_skip_leaves_an_unlisted_file and
\* page_skip_loses_a_backup.

\* Naturals for counting backups and restores; FiniteSets for counting how
\* many backups are newer than one, which a purge needs, and how many
\* entries a walk meets; Sequences for the order it meets them in.
EXTENDS Naturals, FiniteSets, Sequences

\* The knobs of a run. Each configuration file (MC_BackupSeal_*.cfg) gives them values.
CONSTANTS
  \* How many backups the model may take in one behaviour: bounds it.
  MaxBackups,
  \* How many restores the model may start in one behaviour: bounds it.
  MaxRestores,
  \* How many entries one batch of a walk of meta/ holds (the readdir
  \* buffer): the walk fetches the next batch when one is used up.
  Batch,
  \* How many pool files one batch of the sweep holds as candidates
  \* (collect::SWEEP).
  Sweep,
  \* How many table slots the database has. Two lets a backup or a restore
  \* copy its tables one at a time, so a power cut can fall between them;
  \* one keeps a run with many backups or restores small.
  Slots,
  \* "none" for the engine, or one defect named above.
  Mutant

\* At least two backups, so two files can be swapped.
ASSUME MaxBackups \in Nat \ {0, 1}
\* Any number of restores; none for a run about deletes alone.
ASSUME MaxRestores \in Nat
\* A batch of the walk of meta/ holds at least one entry.
ASSUME Batch \in Nat \ {0}
\* A batch of the sweep holds at least one pool file.
ASSUME Sweep \in Nat \ {0}
\* One table slot or two: enough for every rule here.
ASSUME Slots \in {1, 2}
\* Only the engine and the defects above.
ASSUME Mutant \in {"none", "PlainMeta", "PlainRestore", "OpenUnderCurrent",
                   "ListingUnbound", "IdUnbound", "ManifestFirst", "OverDatabase",
                   "CheckAfterCopy", "GcSkipsSealed", "GcSkipsUnreadable", "MetaFirst",
                   "ListSkipsUnreadable", "ListNeedsKey", "StreamSkips", "SweepSkips",
                   "SyncOnlyRemoved", "FooterUnchecked"}

\* The two key ids the database rotates between (KeyId).
Keys == {1, 2}
\* The table slots: a table's place in the database (its file id).
Tables == 1..Slots
\* The backup ids, the numbers in meta/000001.backup and on: one per backup
\* the model may take.
Ids == 1..MaxBackups
\* A slot that holds no table.
NoObj == <<0, 0>>
\* Every table there can be: slot t sealed under key k.
Objs == Tables \X Keys
\* What a slot can hold: a table, or nothing.
Slot == Objs \cup {NoObj}
\* Every slot empty.
Empty == [t \in Tables |-> NoObj]
\* The key field of metadata written in plain text.
Plain == 0

\* A restore given no key provider (restore's `None`). Every stand-in value
\* in this module is a record shaped like the values beside it, because TLC
\* refuses to compare a string with a record.
NoProv == [given |-> FALSE, has |-> {}, right |-> {}, cur |-> 1]

\* The providers a restore may be given. `has` is the set of key ids it
\* hands out, `right` the ids among them whose bytes are the ones that
\* sealed, `cur` the key it names current (KeyProvider::current).
Providers ==
  \* No provider at all.
  {NoProv} \cup
  \* Both keys, the right bytes, key 1 current.
  { [given |-> TRUE, has |-> {1, 2}, right |-> {1, 2}, cur |-> 1],
    \* Both keys, the right bytes, key 2 current: after a rotation.
    [given |-> TRUE, has |-> {1, 2}, right |-> {1, 2}, cur |-> 2],
    \* Only key 2: key 1 was retired.
    [given |-> TRUE, has |-> {2}, right |-> {2}, cur |-> 2],
    \* Only key 1.
    [given |-> TRUE, has |-> {1}, right |-> {1}, cur |-> 1],
    \* Key 1 only, but it names key 2 current: a current key it lacks.
    [given |-> TRUE, has |-> {1}, right |-> {1}, cur |-> 2],
    \* Both ids, but other bytes under them: the wrong keys.
    [given |-> TRUE, has |-> {1, 2}, right |-> {}, cur |-> 1],
    \* Both ids after a rotation, the right bytes for key 2 and other bytes
    \* under key 1: what opens a backup's metadata sealed under key 2, and
    \* none of its tables still under key 1.
    [given |-> TRUE, has |-> {1, 2}, right |-> {2}, cur |-> 2] }

\* The values a slot map's tables take, without the empty marker.
Held(f) == {f[t] : t \in Tables} \ {NoObj}

\* The largest number in a non-empty set.
Max(S) == CHOOSE x \in S : \A y \in S : y <= x

\* A metadata file that does not exist (`on` says whether one does). There
\* is nothing to read, so `reads` is TRUE only to give the field a value.
Absent == [on |-> FALSE, key |-> Plain, listing |-> Empty, sealed |-> Empty,
           body |-> Empty, bound |-> 1, reads |-> TRUE]

\* A target with no MANIFEST (`on` says whether it has one).
NoManifest == [on |-> FALSE, body |-> Empty, sealed |-> FALSE, want |-> Empty, opens |-> FALSE]

\* No removal of metadata waits for its sync.
NoPend == [b \in Ids |-> Absent]

\* The backup in flight when there is none.
IdleBk == [phase |-> "idle", id |-> 1, objs |-> Empty, key |-> 1]

\* The collection in flight when there is none.
IdleGc == [phase |-> "idle", sorder |-> <<>>, spos |-> 0, from |-> {}, cand |-> {},
           order |-> <<>>, pos |-> 0]

\* The ids of S in ascending order: the i-th has i - 1 ids of S below it.
Ascending(S) == [i \in 1..Cardinality(S) |->
                   CHOOSE b \in S : Cardinality({c \in S : c < b}) = i - 1]

\* A pool file's place in the one order the model's walk of shared/ hands
\* the pool out in: slot first, then key.
Rank(o) == (o[1] - 1) * 2 + o[2]

\* The pool files of S in that order: the i-th has i - 1 files of S before
\* it.
InOrder(S) == [i \in 1..Cardinality(S) |->
                 CHOOSE o \in S : Cardinality({p \in S : Rank(p) < Rank(o)}) = i - 1]

\* The smaller of two numbers.
Min(a, b) == IF a < b THEN a ELSE b

\* The restore in flight when there is none.
IdleRs == [phase |-> "idle", id |-> 1, p |-> NoProv, m |-> Absent, want |-> Empty,
           ent |-> FALSE, wrote |-> FALSE]

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The database's tables: live[t] is the key table slot t is sealed under,
  \* 0 when the slot holds no table (Version.levels; SsTableReader::seal_key).
  live,
  \* The key the database's provider names current (KeyProvider::current).
  current,
  \* The tables durable in the pool: backup_dir/shared/<content id>.sst.
  shared,
  \* meta[b]: backup b's metadata file, a record:
  \*   on       whether the file exists (Absent is the one that does not);
  \*   key      the key id it is sealed under and names, Plain when plain;
  \*   listing  each slot's content id as the clear listing says now;
  \*   sealed   the listing the tag was computed over when it was written;
  \*   body     the sealed part: which table each slot gets (file id, key
  \*            range, the key the table names);
  \*   bound    the backup id the tag binds (the file's name when written);
\*   reads    whether the engine can read it at all: FALSE once a bit
\*            flipped, a newer build rewrote it, or the disk fails the read
\*            (format::decode_listing returns an error).
  meta,
  \* What backup b held when it was taken, by id: a ghost the checks read,
  \* nothing in the program stores it.
  recorded,
  \* The backup in flight (BackupEngine::create_backup): phase "idle" or
  \* "copy", the id it will write, the tables it lists, the key it seals
  \* under.
  bk,
  \* The restore target's table files: tgt[t] is what sits in its sst/ dir
  \* under slot t's file id.
  tgt,
  \* The restore target's MANIFEST: whether there is one (on), what it
  \* names (body), whether it is sealed, the tables the backup recorded
  \* (want, a ghost), and whether the provider the restore was given opens
  \* every table it names (opens, a ghost: what the restored database's
  \* open will find).
  tman,
  \* The restore in flight (BackupEngine::restore): its phase ("idle",
  \* "check", "copy", "refused"), the backup id, the provider, the metadata
  \* it read, what that backup recorded (ghost), whether the provider was
  \* entitled to it (ghost), and whether it wrote into the target yet.
  rs,
  \* How many backups were started: bounds the model.
  made,
  \* How many restores were started: bounds the model.
  restores,
  \* How many times a file changed behind the engine's back (the attacker
  \* edited a listing or swapped two files, or a file stopped reading): at
  \* most once, which keeps the model small and is all any rule needs.
  adv,
  \* The collection in flight (collect_shared, src/backup/collect.rs), a
  \* record:
  \*   phase   "idle"; "sync" while it syncs meta/ before anything else;
  \*           "batch" when it takes the next batch of candidates;
  \*           "walk" while that batch's walk of meta/ reads the listings;
  \*           "remove" once the walk is done; "done" once the walk of
  \*           shared/ is used up; "stopped" once a listing did not read;
  \*   sorder  the pool files, in the order the walk of shared/ hands them
  \*           out (fixed when the collection starts);
  \*   spos    how many of them the walk of shared/ has handed out;
  \*   from    the batch's pool files (a ghost: the code holds only what
  \*           is left of them, below);
  \*   cand    the candidates still standing: the batch's files no listing
  \*           read so far names (the `unlisted` set);
  \*   order   the backups' metadata files in the order this batch's walk
  \*           of meta/ hands them out (Env::read_dir; by id here);
  \*   pos     how many entries that walk has handed out so far.
  gc,
  \* pend[b]: backup b's metadata file as it was before a delete or a purge
  \* removed it, while that removal is not durable yet: meta/ has not been
  \* synced since (Env::remove_file, then Env::sync_dir). A power cut brings
  \* it back. Absent when nothing of b's waits for a sync.
  pend

\* Every variable, so a step that changes none of them is a stutter.
vars == <<live, current, shared, meta, recorded, bk, tgt, tman, rs, made, restores, adv, gc, pend>>

----------------------------------------------------------------------------
\* The start: an empty database under key 1, an empty pool, no backup.

\* Init describes the one state every run starts in.
Init ==
  \* No table yet.
  /\ live = [t \in Tables |-> 0]
  \* The provider names key 1 current.
  /\ current = 1
  \* Nothing in the pool.
  /\ shared = {}
  \* No metadata file.
  /\ meta = [b \in Ids |-> Absent]
  \* Nothing recorded.
  /\ recorded = [b \in Ids |-> Empty]
  \* No backup running.
  /\ bk = IdleBk
  \* The target holds no table...
  /\ tgt = Empty
  \* ...and no MANIFEST.
  /\ tman = NoManifest
  \* No restore running.
  /\ rs = IdleRs
  \* Nothing started yet.
  /\ made = 0
  \* Nothing restored yet.
  /\ restores = 0
  \* The attacker has not acted.
  /\ adv = 0
  \* No collection running.
  /\ gc = IdleGc
  \* No removal waits for a sync.
  /\ pend = NoPend

----------------------------------------------------------------------------
\* The database's own steps.

\* A flush writes table slot t, sealed under the current key.
WriteTable(t) ==
  \* Between collections: a collection never reads the database, so a flush
  \* that runs beside one is the same as one that runs after it.
  /\ gc.phase = "idle"
  \* Only an empty slot: the model keeps each slot to one table at a time.
  /\ live[t] = 0
  \* The new table names the key current right now.
  /\ live' = [live EXCEPT ![t] = current]
  \* Nothing else moves.
  /\ UNCHANGED <<current, shared, meta, recorded, bk, tgt, tman, rs, made, restores, adv, gc, pend>>

\* A compaction rewrites table slot t under the current key (reseal_tables).
Reseal(t) ==
  \* Between collections, as for a flush.
  /\ gc.phase = "idle"
  \* Only a table under another key.
  /\ live[t] \notin {0, current}
  \* Now it names the current key: new bytes, a new content id.
  /\ live' = [live EXCEPT ![t] = current]
  \* Nothing else moves.
  /\ UNCHANGED <<current, shared, meta, recorded, bk, tgt, tman, rs, made, restores, adv, gc, pend>>

\* The operator rotates the provider's current key.
Rotate ==
  \* Between collections, as for a flush.
  /\ gc.phase = "idle"
  \* 1 becomes 2 and 2 becomes 1.
  /\ current' = 3 - current
  \* Nothing written changes.
  /\ UNCHANGED <<live, shared, meta, recorded, bk, tgt, tman, rs, made, restores, adv, gc, pend>>

----------------------------------------------------------------------------
\* Taking a backup (BackupEngine::create_backup).

\* The id the engine gives the next backup: one past the largest it finds
\* (next_backup_id).
NextId ==
  \* The ids whose metadata exists.
  LET present == {b \in Ids : meta[b].on}
  \* None: start at 1. Otherwise one past the largest.
  IN IF present = {} THEN 1 ELSE Max(present) + 1

\* A backup starts: it resolves the current key's sealer and captures the
\* tables the database holds right now.
StartBackup ==
  \* One backup at a time (create_backup takes &mut self).
  /\ bk.phase = "idle"
  \* And no restore or collection running on the same engine.
  /\ rs.phase = "idle"
  \* And no collection is running.
  /\ gc.phase = "idle"
  \* Within the model's budget.
  /\ made < MaxBackups
  \* The id it would take is one the model has room for.
  /\ NextId \in Ids
  \* It will list every table the database holds, under the key each names,
  \* and seal under the current key.
  /\ bk' = [phase |-> "copy", id |-> NextId,
            objs |-> [t \in Tables |-> IF live[t] = 0 THEN NoObj ELSE <<t, live[t]>>],
            key |-> current]
  \* One more backup started.
  /\ made' = made + 1
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, meta, recorded, tgt, tman, rs, restores, adv, gc, pend>>

\* One listed table lands in the pool, whole (ensure_shared_file).
CopyToPool(t) ==
  \* While copying.
  /\ bk.phase = "copy"
  \* A slot that holds a table...
  /\ bk.objs[t] # NoObj
  \* ...the pool does not have yet (a pool file it has is reused).
  /\ bk.objs[t] \notin shared
  \* Now the pool has it.
  /\ shared' = shared \cup {bk.objs[t]}
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, meta, recorded, bk, tgt, tman, rs, made, restores, adv, gc, pend>>

\* The metadata is written, whole, and the backup returns.
WriteMeta ==
  \* While copying.
  /\ bk.phase = "copy"
  \* Only once every table it lists is in the pool. MetaFirst skips this.
  /\ \/ Mutant = "MetaFirst"
     \* The design: every table the backup lists is already in the pool.
     \/ \A t \in Tables : bk.objs[t] # NoObj => bk.objs[t] \in shared
  \* The file: sealed under the backup's key (plain under PlainMeta), the
  \* listing in the clear and the same listing under the tag, the body, and
  \* the id the tag binds.
  \* A file the engine just wrote reads.
  /\ meta' = [meta EXCEPT ![bk.id] =
                [on |-> TRUE, key |-> IF Mutant = "PlainMeta" THEN Plain ELSE bk.key,
                 listing |-> bk.objs, sealed |-> bk.objs, body |-> bk.objs,
                 bound |-> bk.id, reads |-> TRUE]]
  \* What this id now holds, for the checks.
  /\ recorded' = [recorded EXCEPT ![bk.id] = bk.objs]
  \* The write ends with a sync of meta/ (atomic_write), which makes every
  \* removal of metadata made before it durable too.
  /\ pend' = NoPend
  \* The backup is done.
  /\ bk' = IdleBk
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, tgt, tman, rs, made, restores, adv, gc>>

----------------------------------------------------------------------------
\* Deleting backups (BackupEngine::delete_backup, purge_old_backups,
\* remove_backups, collect_shared).

\* The ids whose metadata file is in meta/ right now.
Present == {b \in Ids : meta[b].on}

\* The metadata of every backup in `gone` goes, not durable yet, and the
\* collection starts with its sync of meta/. delete_backup and
\* purge_old_backups both end here. Picture backups 1 and 2, where 2 cannot
\* be read: deleting 1 removes its file, then the sweep finds 2 unreadable
\* and removes no pool file, since it cannot know which ones 2 needs.
\* Nothing from the deleted backups' own metadata is needed, so a backup that
\* cannot be read is deleted too. A file that is already gone (a call run
\* again after its sync failed) is not removed again, and its removal may
\* still be waiting for a sync.
Remove(gone) ==
  \* No backup in flight on this engine (both calls take &mut self)...
  /\ bk.phase = "idle"
  \* ...no restore...
  /\ rs.phase = "idle"
  \* ...and no other collection.
  /\ gc.phase = "idle"
  \* Every file in `gone` is removed (one that is not there stays absent).
  /\ meta' = [b \in Ids |-> IF b \in gone THEN Absent ELSE meta[b]]
  \* Each removal made now waits for a sync, keeping the file as it was; one
  \* already waiting keeps waiting.
  /\ pend' = [b \in Ids |-> IF b \in gone /\ meta[b].on THEN meta[b] ELSE pend[b]]
  \* The collection starts with the sync of meta/; the walk of shared/ will
  \* then hand out every pool file, none yet. SyncOnlyRemoved skips the sync
  \* when this call removed no file, as the code did before, though a
  \* removal an earlier call made may still wait for it.
  /\ LET removed == \E b \in gone : meta[b].on
         \* Whether this call syncs meta/ before it sweeps.
         syncs == Mutant # "SyncOnlyRemoved" \/ removed
     \* Into the sync, or straight into the sweep.
     IN gc' = [IdleGc EXCEPT !.phase = IF syncs THEN "sync" ELSE "batch",
                             !.sorder = InOrder(shared)]
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, recorded, bk, tgt, tman, rs, made, restores, adv>>

\* The sync of meta/ succeeds (Env::sync_dir): every removal of metadata
\* made so far, by this call or an earlier one, is durable now, and the
\* sweep may start.
SyncMeta ==
  \* The collection is at its sync.
  /\ gc.phase = "sync"
  \* No removal waits any more.
  /\ pend' = NoPend
  \* The sweep takes its first batch next.
  /\ gc' = [gc EXCEPT !.phase = "batch"]
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, meta, recorded, bk, tgt, tman, rs, made, restores, adv>>

\* The sync of meta/ fails: the call returns the error and removes no pool
\* file. The removals it made stay, not durable: the next delete or purge
\* finds those files gone, and its own sync is what makes them durable.
SyncFails ==
  \* The collection is at its sync.
  /\ gc.phase = "sync"
  \* The call is over, having swept nothing.
  /\ gc' = IdleGc
  \* Nothing else moves: the removals still wait.
  /\ UNCHANGED <<live, current, shared, meta, recorded, bk, tgt, tman, rs, made, restores, adv, pend>>

\* The sweep takes its next batch: the next Sweep pool files the walk of
\* shared/ hands out. The file that fills a batch is in it, so the next batch
\* starts right after it; SweepSkips starts every batch after the first one
\* file too far, losing it. An empty batch after the first means the walk is
\* used up; the first is swept even empty, so a listing that cannot be read
\* is reported whatever the pool holds.
NextBatch ==
  \* Between batches.
  /\ gc.phase = "batch"
  \* Where this batch starts, and where it ends.
  /\ LET start == IF Mutant = "SweepSkips" /\ gc.spos > 0 THEN gc.spos + 1 ELSE gc.spos
         \* The batch ends Sweep files later, or at the end of the pool if that comes first.
         stop == Min(start + Sweep, Len(gc.sorder))
         \* The batch's pool files.
         batch == {gc.sorder[i] : i \in (start + 1)..stop}
     \* An empty batch after the first?
     IN IF batch = {} /\ gc.spos > 0
        \* Nothing left: the sweep is done.
        THEN gc' = [gc EXCEPT !.phase = "done"]
        \* Candidates are the batch; this batch's walk of meta/ will hand
        \* out every backup's file, in id order (see WHAT THE MODEL LEAVES
        \* OUT).
        ELSE gc' = [gc EXCEPT !.phase = "walk", !.spos = stop, !.from = batch,
                              !.cand = batch, !.order = Ascending(Present), !.pos = 0]
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, meta, recorded, bk, tgt, tman, rs, made, restores, adv, pend>>

\* The walk of meta/ hands out its next entry and the sweep reads that
\* backup's listing, striking every candidate the listing names: a file a
\* backup lists must stay. Each entry is read as the walk reaches it, so the
\* sweep holds the candidates and one listing, never every listing.
\*
\* The walk comes in batches of Batch entries (readdir's buffer). After a
\* batch is used up the next one is fetched; StreamSkips fetches it one entry
\* too far, so the first entry of every later batch is never handed out. A
\* listing that does not read stops the sweep (GcSkipsUnreadable passes over
\* it as if it named nothing); GcSkipsSealed reads a sealed listing as if it
\* named nothing.
WalkNext ==
  \* A walk is running and has entries left.
  /\ gc.phase = "walk"
  \* (An entry is left.)
  /\ gc.pos < Len(gc.order)
  \* At a batch boundary (a whole batch handed out), StreamSkips loses one.
  /\ LET skip == Mutant = "StreamSkips" /\ gc.pos > 0 /\ gc.pos % Batch = 0
         \* The position of the entry handed out now.
         i == IF skip THEN gc.pos + 2 ELSE gc.pos + 1
     \* Is that position past the end of the walk?
     IN IF i > Len(gc.order)
        \* The lost entry was the last one: the walk ends with nothing more.
        THEN gc' = [gc EXCEPT !.pos = i]
        \* Otherwise backup c's listing is read.
        ELSE LET c == gc.order[i]
             \* Does its listing fail to read (the bug GcSkipsUnreadable not planted)?
             IN IF ~meta[c].reads /\ Mutant # "GcSkipsUnreadable"
                \* It does not read: the sweep stops, removing nothing more.
                THEN gc' = [gc EXCEPT !.pos = i, !.phase = "stopped"]
                \* It counts unless it does not read (GcSkipsUnreadable) or is
                \* sealed (GcSkipsSealed); what it lists is struck.
                ELSE LET counted == /\ meta[c].reads
                                    \* and, under the bug GcSkipsSealed, only when it is not sealed.
                                    /\ Mutant # "GcSkipsSealed" \/ meta[c].key = Plain
                         \* The pool files that listing names, if it counts.
                         names == IF counted THEN Held(meta[c].listing) ELSE {}
                     \* The walk moves on, and those files stop being candidates for removal.
                     IN gc' = [gc EXCEPT !.pos = i, !.cand = @ \ names]
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, meta, recorded, bk, tgt, tman, rs, made, restores, adv, pend>>

\* The walk of meta/ has handed out its last entry (or lost it): the sweep
\* stops reading and turns to removing.
WalkDone ==
  \* A walk with no entry left.
  /\ gc.phase = "walk"
  \* (No entry is left.)
  /\ gc.pos >= Len(gc.order)
  \* Removal comes next.
  /\ gc' = [gc EXCEPT !.phase = "remove"]
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, meta, recorded, bk, tgt, tman, rs, made, restores, adv, pend>>

\* Every candidate still standing is removed from the pool, and the sweep
\* goes on to the next batch, or is done once the walk of shared/ is used
\* up. The code removes them one at a time and then syncs shared/ once; one
\* step loses nothing here, because no backup lists any of them, so a power
\* cut that keeps any subset of the removals is as safe as keeping them all.
Collect ==
  \* After a batch's walk.
  /\ gc.phase = "remove"
  \* The pool loses exactly the unstruck candidates.
  /\ shared' = shared \ gc.cand
  \* The next batch, or the end.
  /\ gc' = [gc EXCEPT !.phase = IF gc.spos >= Len(gc.sorder) THEN "done" ELSE "batch"]
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, meta, recorded, bk, tgt, tman, rs, made, restores, adv, pend>>

\* The sweep returns: done, or stopped by a listing that does not read.
Finish ==
  \* Done or stopped.
  /\ gc.phase \in {"done", "stopped"}
  \* The engine is free again.
  /\ gc' = IdleGc
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, meta, recorded, bk, tgt, tman, rs, made, restores, adv, pend>>

\* delete_backup(b): backup b goes, whether or not its file reads, and even
\* when there is no such backup, in which case only the collection runs.
Delete(b) == Remove({b})

\* purge_old_backups(keep): every backup with at least `keep` newer backups
\* goes, readable or not. With backups 1 and 2, keep = 1 removes 1, keep = 0
\* removes both, and keep = 2 removes none.
Purge(keep) == Remove({b \in Present : Cardinality({c \in Present : c > b}) >= keep})

----------------------------------------------------------------------------
\* Restoring a backup (BackupEngine::restore).

\* Whether p opens every table of `body`: it has each table's key, with the
\* bytes that sealed the table.
TablesOpen(body, p) ==
  \* For each slot that holds a table, its key is one p has right.
  \A t \in Tables : body[t] # NoObj => body[t][2] \in p.has \cap p.right

\* Whether provider p has everything restoring backup b from file m needs:
\* the key the file names, the right bytes, its current key, every key a
\* table names, and a file that is the one id b wrote, unchanged and
\* readable.
Entitled(m, b, p) ==
  \* The file reads: no key opens metadata that does not decode.
  /\ m.reads
  \* A provider.
  /\ p.given
  \* The key the file names...
  /\ m.key \in p.has
  \* ...with the bytes that sealed it.
  /\ m.key \in p.right
  \* Its own current key, which the MANIFEST is sealed under.
  /\ p.cur \in p.has
  \* Every key a listed table names, with the bytes that sealed it.
  /\ TablesOpen(m.body, p)
  \* The listing is the one the tag covers, and the file is b's own.
  /\ m.listing = m.sealed
  \* The file was written as backup b's own.
  /\ m.bound = b

\* Whether the engine opens file m as backup b under p (format::decode).
MetaOpens(m, b, p) ==
  \* A file that does not read opens under nothing: the checksum or the
  \* version check refuses it before any key is looked at.
  /\ m.reads
  \* Plain metadata needs no key.
  /\ IF m.key = Plain THEN TRUE
     \* Sealed metadata:
     ELSE
       \* A sealed file needs a provider (Error::KeyProviderRequired)...
       /\ p.given
       \* ...with the key the file names (Error::UnknownKey). OpenUnderCurrent
       \* opens it under the provider's current key instead.
       /\ IF Mutant = "OpenUnderCurrent"
          \* The bug: it opens only when the provider's current key is the key the file names.
          THEN p.cur = m.key /\ p.cur \in p.has
          \* The design: it opens when the provider has the key the file names.
          ELSE m.key \in p.has
       \* The tag holds only under the bytes that sealed it...
       /\ m.key \in p.right
       \* ...over the listing as it was sealed (ListingUnbound skips this)...
       /\ Mutant = "ListingUnbound" \/ m.listing = m.sealed
       \* ...and the id it was sealed as (IdUnbound skips this).
       /\ Mutant = "IdUnbound" \/ m.bound = b

\* Whether p has its current key and every key a table names, the checks
\* restore makes before its first write. Each table's key is checked
\* against the table itself, its footer's tag verified under the bytes p
\* gives (sstable::authenticate_footer); FooterUnchecked checks only that p
\* has the key, as restore did before.
KeysProvided(m, p) ==
  \* No provider: nothing is sealed, nothing to provide.
  \/ ~p.given
  \* Otherwise the current key and each table's key.
  \/ /\ p.cur \in p.has
     \* The key of every table the backup lists: with the bytes that sealed
     \* it, or, under FooterUnchecked, merely present.
     /\ IF Mutant = "FooterUnchecked"
        \* The bug: present is enough.
        THEN \A t \in Tables : m.body[t] # NoObj => m.body[t][2] \in p.has
        \* The design: each table's footer verifies.
        ELSE TablesOpen(m.body, p)

\* A restore of backup b with provider p starts: it reads b's metadata.
StartRestore(b, p) ==
  \* One restore at a time, and no backup or collection running on this
  \* engine.
  /\ rs.phase = "idle"
  \* No backup is being created.
  /\ bk.phase = "idle"
  \* No collection is running.
  /\ gc.phase = "idle"
  \* Within the model's budget.
  /\ restores < MaxRestores
  \* The backup exists.
  /\ meta[b].on
  \* The target holds no MANIFEST: a target that does is refused before
  \* this, with nothing written. OverDatabase restores over it.
  /\ Mutant = "OverDatabase" \/ ~tman.on
  \* The restore holds the file as it read it, and the ghosts.
  /\ rs' = [phase |-> "check", id |-> b, p |-> p, m |-> meta[b],
            want |-> recorded[b], ent |-> Entitled(meta[b], b, p), wrote |-> FALSE]
  \* One more restore started.
  /\ restores' = restores + 1
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, meta, recorded, bk, tgt, tman, made, adv, gc, pend>>

\* Every check, before any write.
Check ==
  \* Only right after the read.
  /\ rs.phase = "check"
  \* The file opens, and the keys are there. CheckAfterCopy leaves the key
  \* checks to the end.
  /\ LET pass == /\ MetaOpens(rs.m, rs.id, rs.p)
                 \* and every key is provided (the bug CheckAfterCopy leaves that check for later).
                 /\ Mutant = "CheckAfterCopy" \/ KeysProvided(rs.m, rs.p)
     \* Go on to copy, or refuse.
     IN rs' = [rs EXCEPT !.phase = IF pass THEN "copy" ELSE "refused"]
  \* Nothing written.
  /\ UNCHANGED <<live, current, shared, meta, recorded, bk, tgt, tman, made, restores, adv, gc, pend>>

\* One table is copied from the pool into the target, whole: the pool file
\* the listing names, put at slot t's file id (copy_file_atomic).
CopyToTarget(t) ==
  \* While copying.
  /\ rs.phase = "copy"
  \* A slot the listing fills...
  /\ rs.m.listing[t] # NoObj
  \* ...whose pool file is there (verify_shared_file)...
  /\ rs.m.listing[t] \in shared
  \* ...and not copied yet.
  /\ tgt[t] # rs.m.listing[t]
  \* The target now holds it.
  /\ tgt' = [tgt EXCEPT ![t] = rs.m.listing[t]]
  \* This restore has written.
  /\ rs' = [rs EXCEPT !.wrote = TRUE]
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, meta, recorded, bk, tman, made, restores, adv, gc, pend>>

\* The MANIFEST is written, whole, and the restore returns.
WriteManifest ==
  \* While copying.
  /\ rs.phase = "copy"
  \* Only once every listed table is in the target. ManifestFirst skips it.
  /\ \/ Mutant = "ManifestFirst"
     \* The design: every table the backup lists is already in the target.
     \/ \A t \in Tables : rs.m.listing[t] # NoObj => tgt[t] = rs.m.listing[t]
  \* CheckAfterCopy checks the keys only now, after the copies.
  /\ IF Mutant = "CheckAfterCopy" /\ ~KeysProvided(rs.m, rs.p)
     \* It refuses here, the tables already in the target.
     THEN /\ rs' = [rs EXCEPT !.phase = "refused"]
          \* No MANIFEST is written.
          /\ UNCHANGED tman
     \* The engine: the MANIFEST names the body's tables, sealed whenever a
     \* provider was given (plain under PlainRestore).
     ELSE /\ tman' = [on |-> TRUE, body |-> rs.m.body,
                      sealed |-> rs.p.given /\ Mutant # "PlainRestore",
                      want |-> rs.want, opens |-> TablesOpen(rs.m.body, rs.p)]
          \* The restore is over.
          /\ rs' = IdleRs
  \* The target's tables do not move.
  /\ UNCHANGED <<live, current, shared, meta, recorded, bk, tgt, made, restores, adv, gc, pend>>

\* A refused restore returns its error.
EndRefusal ==
  \* Only a refusal.
  /\ rs.phase = "refused"
  \* The engine is free again.
  /\ rs' = IdleRs
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, meta, recorded, bk, tgt, tman, made, restores, adv, gc, pend>>

\* The operator removes the target, to restore into an empty one again.
ResetTarget ==
  \* No restore running.
  /\ rs.phase = "idle"
  \* Something to remove.
  /\ \/ tgt # Empty
     \* or a MANIFEST.
     \/ tman.on
  \* No table...
  /\ tgt' = Empty
  \* ...and no MANIFEST.
  /\ tman' = NoManifest
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, meta, recorded, bk, rs, made, restores, adv, gc, pend>>

----------------------------------------------------------------------------
\* Power cuts, the attacker, and files that stop reading.

\* The power goes. Every file is whole or absent (staging name, sync,
\* rename), so the disk keeps what it has, except a removal of metadata not
\* synced yet, which comes back undone; whatever was in flight stops.
Crash ==
  \* Something was in flight, or a removal waits for its sync.
  /\ \/ bk.phase # "idle" \/ rs.phase # "idle" \/ gc.phase # "idle"
     \* (A removal of metadata waits for its sync.)
     \/ \E b \in Ids : pend[b].on
  \* The backup stops: its pool files stay, unlisted until a later backup
  \* lists them or a delete removes them.
  /\ bk' = IdleBk
  \* The restore stops: its copies stay, with no MANIFEST after them.
  /\ rs' = IdleRs
  \* The collection stops: what it has not removed stays in the pool,
  \* listed by nobody, for the next one.
  /\ gc' = IdleGc
  \* Every removal of metadata not synced yet is undone: the file is back,
  \* as it was. A synced removal stays.
  /\ meta' = [b \in Ids |-> IF pend[b].on THEN pend[b] ELSE meta[b]]
  \* Nothing waits for a sync any more.
  /\ pend' = NoPend
  \* The disk keeps everything else.
  /\ UNCHANGED <<live, current, shared, recorded, tgt, tman, made, restores, adv>>

\* The attacker edits backup b's clear listing so slot t names pool file o,
\* fixing the checksum: another backup's copy of the table, or none.
TamperListing(b, t, o) ==
  \* Once.
  /\ adv = 0
  \* On a repository at rest: no engine shares a backup directory with
  \* another writer, so no edit lands in the middle of a collection.
  /\ gc.phase = "idle"
  \* The file exists.
  /\ meta[b].on
  \* Slot t's table under either key, or nothing, but not what is there.
  /\ o \in {<<t, k>> : k \in Keys} \cup {NoObj}
  \* (It differs from what the listing holds now, so the edit changes something.)
  /\ o # meta[b].listing[t]
  \* The listing changes; the tag and the body do not.
  /\ meta' = [meta EXCEPT ![b].listing[t] = o]
  \* The attacker is done.
  /\ adv' = 1
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, recorded, bk, tgt, tman, rs, made, restores, gc, pend>>

\* The attacker swaps the two metadata files' names, each whole and valid.
Swap ==
  \* Once.
  /\ adv = 0
  \* On a repository at rest, as above.
  /\ gc.phase = "idle"
  \* Both exist.
  /\ meta[1].on
  \* (Backup 2's metadata file exists too.)
  /\ meta[2].on
  \* Each file now sits under the other's name.
  /\ meta' = [meta EXCEPT ![1] = meta[2], ![2] = meta[1]]
  \* The attacker is done.
  /\ adv' = 1
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, recorded, bk, tgt, tman, rs, made, restores, gc, pend>>

\* Backup b's metadata file stops reading: a bit flips on the disk, a newer
\* build rewrites it in a format this one does not read, or the disk fails
\* every read of it. The file is still there, under its name.
Damage(b) ==
  \* Once, in place of the attacker's one act.
  /\ adv = 0
  \* The file exists...
  /\ meta[b].on
  \* ...and reads until now.
  /\ meta[b].reads
  \* From now on the engine cannot decode it. What it said stays in the
  \* record, because the tables it named still matter: a newer build, or
  \* the disk once it reads again, would restore them.
  /\ meta' = [meta EXCEPT ![b].reads = FALSE]
  \* It happened.
  /\ adv' = 1
  \* Nothing else moves.
  /\ UNCHANGED <<live, current, shared, recorded, bk, tgt, tman, rs, made, restores, gc, pend>>

\* Every step anything can take.
Next ==
  \* A flush or a compaction writes a table; a copy lands in the pool or in
  \* the target.
  \/ \E t \in Tables : WriteTable(t) \/ Reseal(t) \/ CopyToPool(t) \/ CopyToTarget(t)
  \* The operator rotates the current key.
  \/ Rotate
  \* A backup starts.
  \/ StartBackup
  \* A backup writes its metadata and returns.
  \/ WriteMeta
  \* A backup is deleted, with the pool files nothing else lists.
  \/ \E b \in Ids : Delete(b)
  \* All but the newest `keep` backups are deleted, the same way.
  \/ \E keep \in 0..MaxBackups : Purge(keep)
  \* The collection's sync of meta/ succeeds, or fails and ends the call.
  \/ SyncMeta \/ SyncFails
  \* The sweep takes a batch, its walk reads the listings one entry at a
  \* time, the walk ends, the unlisted candidates go, and it returns.
  \/ NextBatch \/ WalkNext \/ WalkDone \/ Collect \/ Finish
  \* A restore of any backup starts, under any provider.
  \/ \E b \in Ids, p \in Providers : StartRestore(b, p)
  \* The restore runs its checks.
  \/ Check
  \* The restore writes its MANIFEST and returns.
  \/ WriteManifest
  \* A refused restore returns its error.
  \/ EndRefusal
  \* The operator removes the target.
  \/ ResetTarget
  \* The power goes.
  \/ Crash
  \* The attacker edits a listing...
  \/ \E b \in Ids, t \in Tables, o \in Slot : TamperListing(b, t, o)
  \* ...or swaps two files' names...
  \/ Swap
  \* ...or a metadata file stops reading.
  \/ \E b \in Ids : Damage(b)

\* Start at Init, then take steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Limits a configuration may put on a run to keep it small (TLC's
\* CONSTRAINT: a state that breaks one is checked, and the run goes no
\* further from it). Each leaves out one kind of step, so a run can look at
\* the others in every order without also multiplying by that kind. Every
\* pair of kinds still meets in some GREEN run.

\* No delete and no purge: no collection is ever in flight.
NoCollection == gc.phase = "idle"

\* Nothing changes behind the engine's back: no edited listing, no swapped
\* files, no metadata that stops reading.
NoAttack == adv = 0

\* No sync of meta/ fails: a removal of metadata waits for its sync only
\* while the collection is at that sync. A run about another rule leaves
\* the failure and the call run again after it out; every GREEN run keeps
\* them.
NoSyncFailure == gc.phase = "sync" \/ \A b \in Ids : ~pend[b].on

----------------------------------------------------------------------------
\* Invariants.

\* A metadata file as the engine writes it.
MetaType == [on : BOOLEAN, key : Keys \cup {Plain}, listing : [Tables -> Slot],
             sealed : [Tables -> Slot], body : [Tables -> Slot], bound : Ids,
             reads : BOOLEAN]

\* Every variable holds what its comment says.
TypeOK ==
  \* Each slot is empty or names a key.
  /\ live \in [Tables -> {0} \cup Keys]
  \* The current key is one of the two.
  /\ current \in Keys
  \* The pool holds tables.
  /\ shared \subseteq Objs
  \* Each id has a file or none.
  /\ meta \in [Ids -> MetaType]
  \* What each id held.
  /\ recorded \in [Ids -> [Tables -> Slot]]
  \* The backup in flight.
  /\ bk \in [phase : {"idle", "copy"}, id : Ids, objs : [Tables -> Slot], key : Keys]
  \* The target's tables.
  /\ tgt \in [Tables -> Slot]
  \* The target's MANIFEST.
  /\ tman \in [on : BOOLEAN, body : [Tables -> Slot], sealed : BOOLEAN, opens : BOOLEAN,
               want : [Tables -> Slot]]
  \* The restore in flight.
  /\ rs \in [phase : {"idle", "check", "copy", "refused"}, id : Ids, p : Providers,
             m : MetaType, want : [Tables -> Slot], ent : BOOLEAN, wrote : BOOLEAN]
  \* The budgets.
  /\ made \in 0..MaxBackups
  \* The restores started are within their budget.
  /\ restores \in 0..MaxRestores
  \* A file changed behind the engine's back at most once.
  /\ adv \in 0..1
  \* The collection in flight: its walks and its candidates.
  /\ gc.phase \in {"idle", "sync", "batch", "walk", "remove", "done", "stopped"}
  \* The sweep's order is a sequence of pool files.
  /\ gc.sorder \in Seq(Objs)
  \* Its position is within the pool, or one past it.
  /\ gc.spos \in 0..Cardinality(Objs) + 1
  \* The batch in hand is a set of pool files.
  /\ gc.from \subseteq Objs
  \* The candidates for removal are pool files.
  /\ gc.cand \subseteq Objs
  \* The walk's order is a sequence of backup ids.
  /\ gc.order \in Seq(Ids)
  \* Its position is within the backups, or one past them.
  /\ gc.pos \in 0..MaxBackups + 1
  \* What waits for a sync is a metadata file or nothing, for each id.
  /\ pend \in [Ids -> MetaType]

\* No backup's metadata is plain, and no restored MANIFEST is. It rules out
\* the PlainMeta story (backup 1's file names every table's key range in
\* plain text) and the PlainRestore story (a restore with the right key
\* writes a MANIFEST that names them all in plain text). This is D57.
SealedMetadata ==
  \* Every metadata file names a key it is sealed under.
  /\ \A b \in Ids : meta[b].on => meta[b].key \in Keys
  \* Every restored MANIFEST is sealed.
  /\ tman.on => tman.sealed

\* A restore never refuses a provider that has every key the backup needs.
\* It rules out the OpenUnderCurrent story: backup 1 sealed under key 1, the
\* provider rotated to key 2 but still holding key 1, and the restore
\* refusing because it tried key 2.
RightKeysRestore == rs.phase = "refused" => ~rs.ent

\* A finished restore holds exactly what the backup held when it was taken:
\* its MANIFEST names those tables, and each sits in the target. It rules
\* out the ListingUnbound story (an edited listing puts another backup's
\* copy of a table under this backup's MANIFEST), the IdUnbound story
\* (backup 2's file under backup 1's name restores backup 2 as backup 1),
\* the ManifestFirst story (a power cut after the MANIFEST and before the
\* tables) and the OverDatabase story (a restore over a finished target
\* replaces tables its MANIFEST names).
FaithfulRestore ==
  \* When the target has a MANIFEST...
  tman.on =>
    \* ...it names exactly the tables the backup recorded...
    /\ tman.body = tman.want
    \* ...and each one it names is in the target.
    /\ \A t \in Tables : tman.body[t] # NoObj => tgt[t] = tman.body[t]

\* A finished restore names only tables the provider it was given opens,
\* so the restored database opens with it. It rules out the FooterUnchecked
\* story: backup 2's metadata opens under the right bytes for key 2, its
\* table under key 1 is only checked to be there, and the restore writes a
\* MANIFEST naming a table that does not open under the other bytes the
\* provider holds for key 1.
RestoredOpens == tman.on => tman.opens

\* A refused restore wrote nothing. It rules out the CheckAfterCopy story:
\* a provider without its current key is refused only after every table was
\* copied into the target.
RefusalWritesNothing == rs.phase = "refused" => ~rs.wrote

\* Every table a backup lists is in the pool, unless the attacker edited
\* that listing, and even while the file does not read: the read may come
\* back, or a newer build read it. It rules out the GcSkipsSealed story
\* (deleting backup 1 removes a table sealed backup 2 still lists), the
\* GcSkipsUnreadable story (deleting backup 1 removes a table unreadable
\* backup 2 still lists), the MetaFirst story (a power cut after the
\* metadata and before its tables) and the SyncOnlyRemoved story (a delete
\* run again after its sync failed removes backup 1's tables, and a power
\* cut brings backup 1's metadata back).
ListedRestores ==
  \* For every backup id b:
  \A b \in Ids :
    \* For a file whose listing is the one it was sealed with...
    (meta[b].on /\ meta[b].listing = meta[b].sealed) =>
      \* ...every table it lists is in the pool.
      \A t \in Tables : meta[b].listing[t] # NoObj => meta[b].listing[t] \in shared

\* The sweep's candidates are always among one batch's pool files, at most
\* Sweep of them: striking only ever takes files away. It rules out a
\* collection that gathers the pool, or what every backup lists, into one
\* set, which grows with the repository; the code keeps one batch.
CollectionBounded ==
  \* Candidates come from the batch...
  /\ gc.cand \subseteq gc.from
  \* ...and a batch holds at most Sweep files.
  /\ Cardinality(gc.from) <= Sweep

\* A sweep that finishes leaves no pool file that no remaining listing
\* names: those the deleted backups held, and those a backup cut short
\* copied and never listed, are all gone. It rules out the SweepSkips story
\* (the walk of shared/ loses the file that starts the second batch, so it
\* is never a candidate and stays in the pool, listed by nobody, for ever).
SweepRemovesEveryUnlisted ==
  \* Once the sweep is done...
  gc.phase = "done" =>
    \* ...every pool file is one a remaining listing names.
    shared \subseteq UNION {Held(meta[c].listing) : c \in Present}

\* What list_backups returns at this moment: the id of every backup it
\* names, each with its summary or the reason it cannot be read (the model
\* keeps only the ids, which is what the rule below is about). It reads the
\* directory and each file's clear listing, so it changes nothing and needs
\* no key: it is a function of the files, checked in every state.
\* ListSkipsUnreadable leaves out a file that does not read, as the code did
\* before; ListNeedsKey leaves out a sealed file, as a listing that opened
\* the seal would with no key at hand.
Listing ==
  \* Of the ids whose file is in meta/, the ones the listing names:
  {b \in Present :
     \* all of them for the engine; the first defect drops each file that
     \* does not read...
     /\ Mutant = "ListSkipsUnreadable" => meta[b].reads
     \* ...and the second drops each sealed one.
     /\ Mutant = "ListNeedsKey" => meta[b].key = Plain}

\* The listing names every backup the repository holds, readable or not,
\* sealed or not, and nothing else. It rules out the ListSkipsUnreadable
\* story (backup 2's metadata flips a bit, the list shows only backup 1,
\* and the caller believes backup 1 is all there is) and the ListNeedsKey
\* story (an engine with no key lists no backup of an encrypted database).
\* A caller deciding what to keep, delete or restore needs the whole list.
ListingNamesEvery ==
  \* The ids the listing names are exactly the ids with a file in meta/.
  Listing = Present

====
