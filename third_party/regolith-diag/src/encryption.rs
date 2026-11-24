//! Encryption at rest: the keys a database seals its files under.
//!
//! With [`crate::Options::key_provider`] set, regolith seals every frame it
//! writes with AES-256-GCM-SIV (RFC 8452): each SSTable data, index and
//! filter block, each write-ahead log record, and each manifest edit batch.
//! A random-access read still decrypts exactly one block, and a frame whose
//! tag fails is treated as corruption wherever a failed checksum is.
//!
//! Values are plaintext inside the engine: the memtable, the block cache,
//! merge operators, byte-equality checks, value-validated reads,
//! content-addressed keys, appends, allocations and ingested tables read at
//! their sequence all see the bytes the caller wrote, so every mechanism
//! works unchanged on an encrypted store.
//!
//! # Where the tag replaces the checksum, and where it cannot
//!
//! - **Table blocks, index and filter regions, and the table footer**: the
//!   tag replaces the checksum. Nothing in a table is read before its frame
//!   opens, and a failure is a corrupt table either way.
//! - **Write-ahead log records**: the tag replaces the payload check. The
//!   header keeps its own check, because replay tests every offset after a
//!   damaged record for a header (to read P past the damage) and that test
//!   must stay one short hash, with no key. The tag binds the payload to
//!   that header, its offset and its log. A record whose tag fails is an
//!   unusable record under the same O < P rule as one whose check fails:
//!   dropped and reported in the newest log's unsynced tail, refused below
//!   P or in an earlier log, naming the file.
//! - **The write-ahead log stamp**: sealed under the log's key, with its own
//!   check kept, and durable before the log exists. A wrong key then fails
//!   the stamp and refuses the open instead of failing every record and
//!   reading as a torn tail (`proofs/tla/WalRecovery.tla`, RED
//!   `StampNotSealed` and `StampUnsynced`).
//! - **Manifest batches**: the checksum stays, checked first and without a
//!   key. Only it can tell a batch a crash tore (dropped, as ever) from a
//!   whole batch under a wrong or missing key (refused): with the tag alone
//!   the two read alike, and one of them would be handled wrongly
//!   (`proofs/tla/ManifestSeal.tla`).
//!
//! # Nonces never repeat under one key
//!
//! Every frame draws a fresh 96-bit nonce from the operating system's
//! random source at the moment it is sealed; nothing is derived from a file
//! id or an offset. That is deliberate: the engine rewrites offsets (a
//! failed group is truncated away and the next group is written at the
//! same offset; a manifest's torn tail is trimmed and appended over), file
//! ids can be reused after a crash, and a checkpoint or a restored backup
//! is a second database writing the same ids under the same keys. A
//! position-derived nonce would repeat in every one of those cases.
//!
//! Random 96-bit nonces repeat with probability at most `n^2 / 2^97` among
//! `n` frames under one key (the birthday bound): about 2^-17 after 2^40
//! frames, which is four pebibytes of 4 KiB blocks, and rotating the key
//! starts the count again. Even then, GCM-SIV is misuse resistant: two
//! frames that did draw one nonce reveal only whether the two frames were
//! identical, never the key stream or the key.
//!
//! # Keys and rotation
//!
//! A [`KeyProvider`] names the key new files are sealed under
//! ([`KeyProvider::current`]) and hands out key material by id
//! ([`KeyProvider::key`]). Every sealed file records the id of the key it
//! was sealed under (a table in its footer, a write-ahead log in its
//! stamp, a manifest in each edit batch), so rotating `current` takes
//! effect for each new table, each new log (the next memtable switch) and
//! each new manifest batch, while everything already written stays
//! readable for as long as the provider still provides the key it names.
//! A key id that some file names and the provider does not provide refuses
//! the open with [`crate::Error::UnknownKey`].
//!
//! # Turning encryption on, and re-encrypting
//!
//! A database written without encryption opens with a provider: its files
//! stay readable, and from then on every file regolith writes is sealed.
//! The first read-write open rewrites the manifest sealed, and recovery
//! rewrites the replayed write-ahead log into a sealed one. Tables are
//! re-sealed as compaction rewrites them, which is automatic but not
//! guaranteed to reach cold data. The one-time re-encryption is a manual
//! compaction: on an encrypted database, [`crate::Db::compact_range`] (any
//! range, any column family) also rewrites every table in the database not
//! sealed under the current key, the active write-ahead log, and the
//! manifest when it holds a batch under another key. After it returns, no
//! file names any other key and nothing is left in plaintext; the same call
//! completes a key rotation, after which the provider may drop the old key.
//!
//! [`crate::Db::ingest_external_files`] installs a table sealed under any
//! key the provider has as it is; a plaintext table is written in sealed
//! under the current key on its way in, so build tables to ingest with
//! [`crate::SstFileWriter`] under the same provider to skip that rewrite.
//!
//! An encrypted database opened without a provider refuses with
//! [`crate::Error::KeyProviderRequired`]; it never reads as empty.
//!
//! # What this does not hide
//!
//! File sizes, the size and count of frames, and which key id sealed each
//! file are visible on disk. The format authenticates each frame and binds
//! it to its file and position, but a database opened with a provider still
//! reads files written before encryption was turned on, so an attacker who
//! can replace whole files with unsealed ones, or with older sealed ones, is
//! outside what this protects against.
//!
//! # Backups
//!
//! [`crate::BackupEngine`] copies sealed tables byte for byte and seals
//! each backup's own metadata (which table goes where, its key range, the
//! key it names) through the database's provider, under the current key,
//! naming that key like every other frame. Each table's content id and
//! size, which the backup directory's file names and sizes show anyway,
//! stay readable, so listing and deleting backups needs no key; the tag
//! covers them and the backup's id all the same. A restore takes a
//! provider, refuses without one or without a key it needs before writing
//! anything, and writes the restored MANIFEST sealed under the provider's
//! current key. A backup of a database without a provider stays plaintext.

use std::fmt;

use zeroize::Zeroizing;

/// The name of one key a [`KeyProvider`] provides.
///
/// Sealed files record the id, never the key. An id must name the same key
/// material for as long as any file names it: a provider that hands out
/// different bytes under an id already in use makes every frame sealed
/// under the old bytes fail its tag, which reads as corruption.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyId(pub u32);

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A 256-bit key, wiped from memory when it drops.
///
/// Regolith keeps the key schedule derived from it for as long as the
/// database is open, and wipes that too when the database drops. `Debug`
/// never prints the bytes.
#[derive(Clone)]
pub struct KeyMaterial(Zeroizing<[u8; 32]>);

impl KeyMaterial {
    /// Wrap 32 bytes of key material. The caller should wipe its own copy.
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for KeyMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyMaterial(..)")
    }
}

/// The source of the keys a database seals its files under. Install one
/// with [`crate::Options::key_provider`].
///
/// Both methods are synchronous and must answer from memory: they run on
/// any thread that creates or opens a file (the committing thread when a
/// write-ahead log rotates, flush and compaction threads, and the thread
/// that opens the database). They must not block, panic or re-enter the
/// database.
///
/// Regolith asks for each key id once and keeps what it derives from the
/// key until the database drops; [`KeyProvider::current`] is asked each
/// time a file is created and each time a manifest batch is written.
///
/// # Example
///
/// A provider that holds its keys in memory, the newest one current. A real
/// one loads them from a key store before the database opens.
///
/// ```
/// use std::sync::Arc;
///
/// use regolith::{Db, Error, KeyId, KeyMaterial, KeyProvider, MemEnv, Options};
///
/// struct Keys(Vec<(KeyId, [u8; 32])>);
///
/// impl KeyProvider for Keys {
///     fn current(&self) -> KeyId {
///         self.0.last().map_or(KeyId(0), |(newest, _)| *newest)
///     }
///
///     fn key(&self, id: KeyId) -> Option<KeyMaterial> {
///         let (_, bytes) = self.0.iter().find(|(held, _)| *held == id)?;
///         Some(KeyMaterial::new(*bytes))
///     }
/// }
///
/// // One in-memory filesystem, so the database can be opened again.
/// let env = Arc::new(MemEnv::new());
/// let open = |keys: Option<Keys>| {
///     let options = Options::memory().env(env.clone());
///     Db::open("sealed", match keys {
///         Some(keys) => options.key_provider(Arc::new(keys)),
///         None => options,
///     })
/// };
///
/// let db = open(Some(Keys(vec![(KeyId(1), [7; 32])])))?;
/// db.put(b"k", b"sealed under key 1")?;
/// db.close()?;
/// // A closed handle still holds the directory until it drops.
/// drop(db);
///
/// // Without a provider, or without the key its files name, the open is
/// // refused; the database never reads as empty.
/// assert!(matches!(open(None), Err(Error::KeyProviderRequired)));
/// let other = Keys(vec![(KeyId(2), [9; 32])]);
/// assert!(matches!(open(Some(other)), Err(Error::UnknownKey { id: KeyId(1) })));
///
/// // Rotation: key 2 becomes current, and key 1 stays for what it sealed.
/// let db = open(Some(Keys(vec![(KeyId(1), [7; 32]), (KeyId(2), [9; 32])])))?;
/// assert_eq!(db.get(b"k")?.as_deref(), Some(&b"sealed under key 1"[..]));
/// # Ok::<(), regolith::Error>(())
/// ```
pub trait KeyProvider: Send + Sync + 'static {
    /// The key new files are sealed under. [`KeyProvider::key`] must
    /// provide it: a current key the provider cannot provide refuses the
    /// open, and fails any later write that needs a new file, with
    /// [`crate::Error::UnknownKey`].
    fn current(&self) -> KeyId;

    /// The key `id` names, or `None` when this provider does not have it.
    /// `None` for an id some file names refuses the open (or fails the
    /// read) with [`crate::Error::UnknownKey`]; it is never read as an
    /// empty or damaged file.
    fn key(&self, id: KeyId) -> Option<KeyMaterial>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_material_never_prints_its_bytes() {
        let key = KeyMaterial::new([0xAB; 32]);
        let shown = format!("{key:?}");
        assert_eq!(shown, "KeyMaterial(..)");
        assert!(!shown.contains("171"), "{shown}");
        assert_eq!(key.as_bytes(), &[0xAB; 32]);
    }

    #[test]
    fn key_id_displays_as_its_number() {
        assert_eq!(KeyId(7).to_string(), "7");
    }
}
