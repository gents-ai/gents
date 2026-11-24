//! Column families - multiple logically isolated keyspaces inside
//! one [`crate::Db`].
//!
//! # Design
//!
//! Column families in regolith are implemented as **key-prefix
//! namespaces** on top of the single underlying LSM engine. Every
//! logical operation on a CF wraps the caller's user key in a
//! 4-byte big-endian `cf_id` prefix before handing it to the
//! engine. The engine sees one global keyspace; the logical
//! isolation is airtight because distinct `cf_id`s produce disjoint
//! byte ranges.
//!
//! ## Trade-offs vs per-CF memtables
//!
//! This design shares:
//!
//! - One memtable, one WAL, one manifest, one compaction thread.
//! - One block cache, one bloom / prefix-bloom budget.
//!
//! That means **cross-CF writes are atomic for free** (a
//! [`crate::WriteBatch`] touching multiple CFs lands in a single
//! underlying `apply_batch` call and crash-recovers as a unit), but
//! per-CF `write_buffer_size` / per-CF compaction strategies are
//! not available. The issue that introduced this feature explicitly
//! scopes those out of v1.
//!
//! ## Atomic flush across column families
//!
//! Because every CF shares one memtable, one WAL, and one
//! manifest, multi-CF writes are atomic in regolith by construction.
//! A flush produces one SSTable that either contains every key in
//! a batch or none of them; the WAL is the source of truth until
//! the manifest edit lands, so a crash mid-flush replays the
//! whole batch on reopen.
//!
//! [`crate::Options::atomic_flush`] is accepted for parity with
//! storage engines that require an explicit opt-in to get this
//! guarantee - under regolith's design its value is irrelevant, the
//! guarantee is always on.
//!
//! ## Metadata storage
//!
//! A reserved [`META_CF_ID`] = `0` holds CF registry entries:
//!
//! - `[0,0,0,0] || "next_id"` → `u32` big-endian counter (next id
//!   to hand out to a freshly created CF).
//! - `[0,0,0,0] || "name:" || <name>` → `u32` big-endian id of the
//!   CF with that name.
//!
//! Users cannot create a CF with id `0`; the default user-facing
//! CF is [`DEFAULT_CF_ID`] = `1` and is created lazily on first
//! [`crate::Db::open`] of a database that doesn't already contain
//! it. User keys in any CF cannot collide with metadata keys
//! because metadata lives under the reserved prefix `[0,0,0,0]`,
//! which no user-facing CF ever produces.
//!
//! ## Dropping a CF
//!
//! [`crate::Db::drop_column_family`] issues a `delete_range` over
//! the CF's prefix range and then removes the `name:` and the
//! handle's lookups. The range delete is O(1) write work regardless
//! of how many keys the CF contained; space is reclaimed on the
//! next compaction over the range. Dropped CFs leave no trace on
//! reopen.
//!
//! ## Creating and dropping against concurrent writes
//!
//! A create or a drop commits as an ordered unit of its own, and the
//! registry changes inside that same unit, right after its commit is
//! durable and applied. Every write checks the families it touches when it
//! is decided, in ring order too, before it takes a sequence. So a
//! write racing a drop either takes a sequence below the drop's range
//! tombstone, which then deletes it, or is refused with
//! [`crate::Error::InvalidColumnFamily`]: it never lands in a family
//! that is already dropped, and never reports success for bytes no
//! handle can read. The registry itself takes no lock: liveness is one
//! lookup in a lock-free map.

use std::sync::Arc;

use kovan_map::HashMap;

use crate::portability::{AtomicU32, Ordering};

/// Reserved column-family id used to store the CF registry. Users
/// cannot create a CF with this id; user-facing CFs start at
/// [`DEFAULT_CF_ID`].
pub(crate) const META_CF_ID: u32 = 0;

/// Id of the default user-facing column family. Auto-created on
/// the first [`crate::Db::open`] of any database and always present
/// thereafter.
pub(crate) const DEFAULT_CF_ID: u32 = 1;

/// Largest CF id that can be allocated.
///
/// `u32::MAX` is excluded rather than reserved: [`cf_upper_bound`]
/// needs an id strictly above the one it bounds, and there is no byte
/// string above every key prefixed with `u32::MAX`.
pub(crate) const MAX_CF_ID: u32 = u32::MAX - 1;

/// Name of the default column family.
pub const DEFAULT_CF_NAME: &str = "default";

/// A handle to a column family. Cheap to clone; carries only the
/// CF's name and numeric id. Handles become invalid after their CF
/// is dropped; result-returning CF operations reject stale handles.
#[derive(Debug, Clone)]
pub struct ColumnFamilyHandle {
    pub(crate) name: Arc<String>,
    pub(crate) id: u32,
}

impl ColumnFamilyHandle {
    /// The CF's display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Internal: the numeric id used to derive the key prefix.
    pub(crate) fn id(&self) -> u32 {
        self.id
    }
}

impl PartialEq for ColumnFamilyHandle {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for ColumnFamilyHandle {}

/// Length of the big-endian CF id [`prefix_key`] puts in front of a key.
pub(crate) const CF_PREFIX_LEN: usize = 4;

/// The error a write or a read gets for a handle whose family is not live
/// in this database: dropped, or another database's.
pub(crate) fn invalid_handle_error(cf: &ColumnFamilyHandle) -> crate::Error {
    crate::Error::invalid_column_family(format!(
        "column family handle '{}' with id {} is not live",
        cf.name(),
        cf.id()
    ))
}

/// The error a write gets when a key names a family id that is not live.
pub(crate) fn dropped_family_error(cf_id: u32) -> crate::Error {
    crate::Error::invalid_column_family(format!("column family id {cf_id} is not live"))
}

/// Encode a user key for a given CF. Returns
/// `cf_id_be(4) || user_key`.
pub(crate) fn prefix_key(cf_id: u32, key: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(CF_PREFIX_LEN + key.len());
    out.extend_from_slice(&cf_id.to_be_bytes());
    out.extend_from_slice(key);
    out
}

/// The key the caller wrote, from the engine's key for it: what
/// [`prefix_key`] put the family id in front of. Every hook the caller
/// implements is handed this, never the engine's key.
///
/// A key too short to carry an id is handed back whole. The database never
/// stores one; the engine's own unit tests do.
pub(crate) fn callers_key(prefixed: &[u8]) -> &[u8] {
    prefixed.get(CF_PREFIX_LEN..).unwrap_or(prefixed)
}

/// The id of the column family the engine's key `prefixed` belongs to, when
/// it carries one.
pub(crate) fn family_of(prefixed: &[u8]) -> Option<u32> {
    prefixed
        .first_chunk::<CF_PREFIX_LEN>()
        .map(|id| u32::from_be_bytes(*id))
}

/// Shortest byte string strictly greater than every key prefixed
/// with `cf_id`. Used by CF drop to issue a
/// `delete_range(prefix, upper_bound)` and by `iter_cf` to bound
/// the scan.
///
/// `cf_id` is never above [`MAX_CF_ID`]: no byte string is greater than
/// every key prefixed with `u32::MAX`, because byte strings have no
/// upper bound, so the id space stops one short of it and
/// [`CfRegistry::next_id`] refuses to mint `u32::MAX`. The saturating
/// add keeps the function total anyway, degrading to an empty range
/// rather than wrapping to zero and aliasing the reserved metadata CF.
pub(crate) fn cf_upper_bound(cf_id: u32) -> Vec<u8> {
    cf_upper_bound_bytes(cf_id).to_vec()
}

/// [`cf_upper_bound`] without the allocation, for a comparison on the
/// commit path.
pub(crate) fn cf_upper_bound_bytes(cf_id: u32) -> [u8; CF_PREFIX_LEN] {
    cf_id.saturating_add(1).to_be_bytes()
}

/// Lower bound (inclusive) of the CF's key range.
pub(crate) fn cf_lower_bound(cf_id: u32) -> Vec<u8> {
    cf_id.to_be_bytes().to_vec()
}

/// Well-known metadata keys in the reserved [`META_CF_ID`] CF.
/// Callers use these directly via `Db::put` / `Db::get` on the
/// meta CF prefix.
pub(crate) mod meta {
    use super::META_CF_ID;

    pub(crate) fn next_id_key() -> Vec<u8> {
        let mut k = META_CF_ID.to_be_bytes().to_vec();
        k.extend_from_slice(b"next_id");
        k
    }

    pub(crate) fn name_key(name: &str) -> Vec<u8> {
        let mut k = META_CF_ID.to_be_bytes().to_vec();
        k.extend_from_slice(b"name:");
        k.extend_from_slice(name.as_bytes());
        k
    }

    /// Prefix used to walk every `name:*` entry in the meta CF
    /// via a range scan at open time.
    pub(crate) fn name_scan_prefix() -> Vec<u8> {
        let mut k = META_CF_ID.to_be_bytes().to_vec();
        k.extend_from_slice(b"name:");
        k
    }

    /// Exclusive upper bound of the `name:*` range, built by
    /// incrementing the trailing `:` of the prefix to `;`.
    pub(crate) fn name_scan_upper() -> Vec<u8> {
        let mut k = META_CF_ID.to_be_bytes().to_vec();
        k.extend_from_slice(b"name;");
        k
    }

    /// Parse the name component out of a `name:<name>` meta key.
    pub(crate) fn name_from_key(key: &[u8]) -> Option<&str> {
        let prefix = name_scan_prefix();
        key.strip_prefix(prefix.as_slice())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
    }
}

/// Buckets each registry map starts with. A database holds a handful of
/// column families, and the maps grow on demand past this.
const REGISTRY_BUCKETS: usize = 64;

/// In-memory mirror of the on-disk CF registry, read without any lock.
///
/// # Invariants
///
/// - **Liveness is one entry.** A family is live exactly while `by_id`
///   holds its id. Every check a write or a read makes ([`Self::contains_id`],
///   [`Self::is_live_handle`]) is one lock-free lookup there, and the two
///   transitions are one map write each: [`Self::publish`] inserts the id
///   (the family is born), [`Self::retire`] removes it (the family is
///   dropped). `by_name` only finds the id for a name, and a name whose id
///   `by_id` does not hold is absent.
/// - **A family's life moves one way.** Ids are never reused: `next_id` only
///   grows, and it is persisted with every create. So an id goes from
///   unborn to live to dropped and never back, and a stale handle can never
///   name a newer family.
/// - **Births and deaths are ordered with the writes.** The engine publishes
///   and retires a family only inside its ordered commit step, right after
///   the write that persists the change, and every write checks the families
///   it touches in ring order too, when it is decided
///   (`RegolithEngine::create_family`, `drop_family`, `cf_fence`). So a
///   write either commits before a drop,
///   and the drop's range tombstone deletes it, or is refused because the
///   family is gone; it never lands after the tombstone.
pub(crate) struct CfRegistry {
    /// id → the live family with that id.
    by_id: HashMap<u32, Arc<CfEntry>>,
    /// name → id of the live family with that name.
    by_name: HashMap<String, u32>,
    /// The id the next create takes. Advanced only inside the ordered
    /// commit step, so two creates never draw one id.
    next_id: AtomicU32,
}

/// What the registry keeps per live family.
struct CfEntry {
    /// Shared with every handle made for the family, so a lookup by name
    /// allocates nothing.
    name: Arc<String>,
}

impl CfRegistry {
    pub(crate) fn new() -> Self {
        Self {
            by_id: HashMap::with_capacity(REGISTRY_BUCKETS),
            by_name: HashMap::with_capacity(REGISTRY_BUCKETS),
            next_id: AtomicU32::new(DEFAULT_CF_ID + 1),
        }
    }

    /// Replace the registry's contents with what the meta CF holds. Called
    /// once by `Db::open`, before any handle exists.
    pub(crate) fn load(&self, entries: impl IntoIterator<Item = (String, u32)>, next_id: u32) {
        // A clear swaps in a new table and retires the old one, which an
        // open, loading into the registry it just made, need not pay.
        if !self.by_id.is_empty() {
            self.by_id.clear();
        }
        if !self.by_name.is_empty() {
            self.by_name.clear();
        }
        for (name, id) in entries {
            let _ = self.insert(id, name);
        }
        self.next_id.store(next_id, Ordering::Release);
    }

    fn insert(&self, id: u32, name: String) -> Arc<String> {
        let shared = Arc::new(name.clone());
        let entry = Arc::new(CfEntry {
            name: Arc::clone(&shared),
        });
        // `by_id` first: the family is live from this insert, and only then
        // findable by name.
        self.by_id.insert(id, entry);
        self.by_name.insert(name, id);
        shared
    }

    pub(crate) fn get(&self, name: &str) -> Option<ColumnFamilyHandle> {
        let id = self.by_name.get(name)?;
        let entry = self.by_id.get(&id)?;
        (*entry.name == name).then(|| ColumnFamilyHandle {
            name: Arc::clone(&entry.name),
            id,
        })
    }

    /// Whether `id` names a live column family. One lock-free lookup.
    ///
    /// The default column family is live for as long as the database is
    /// open, without a lookup: `Db::open` registers it on every load
    /// (`Db::load_cf_registry`) and `Db::drop_column_family` refuses to
    /// drop it, so `retire` can never take it out. Answering it from the
    /// constant keeps the common batch, which is entirely in the default
    /// column family, off the map.
    pub(crate) fn contains_id(&self, id: u32) -> bool {
        id == DEFAULT_CF_ID || self.by_id.contains_key(&id)
    }

    /// Whether `cf` names a live family of this database: its id is live
    /// and carries its name.
    pub(crate) fn is_live_handle(&self, cf: &ColumnFamilyHandle) -> bool {
        self.by_id
            .get(&cf.id())
            .is_some_and(|entry| *entry.name == *cf.name)
    }

    /// The id the next create would take, or `None` once the id space is
    /// exhausted. The last id is never minted: [`cf_upper_bound`] cannot
    /// express an exclusive bound above it, so a CF holding it could be
    /// neither iterated nor dropped.
    ///
    /// Only the engine's ordered commit step calls this, and it publishes
    /// the id before any other create can read it.
    pub(crate) fn next_id(&self) -> Option<u32> {
        let id = self.next_id.load(Ordering::Acquire);
        (id <= MAX_CF_ID).then_some(id)
    }

    /// Make the family `name` with `id` live, after its meta entry is
    /// committed, and return its handle. `id` came from [`Self::next_id`]
    /// in the same ordered step.
    pub(crate) fn publish(&self, id: u32, name: &str) -> ColumnFamilyHandle {
        let name = self.insert(id, name.to_string());
        self.next_id.fetch_max(id + 1, Ordering::AcqRel);
        ColumnFamilyHandle { name, id }
    }

    /// Drop the family `id`, after the range tombstone and the meta delete
    /// that drop it are committed. Its removal from `by_id` is the moment it
    /// stops being live; the name goes after, and only if it still names
    /// this id.
    pub(crate) fn retire(&self, id: u32) {
        if let Some(entry) = self.by_id.remove(&id) {
            self.by_name
                .remove_if(entry.name.as_str(), |held| *held == id);
        }
    }

    /// Every live family's name, in arbitrary order: each family live for
    /// the whole call is listed once, and one created or dropped meanwhile
    /// may or may not be.
    pub(crate) fn names(&self) -> Vec<String> {
        self.by_id
            .values()
            .map(|entry| entry.name.as_str().to_string())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_key_contains_cf_id() {
        let k = prefix_key(5, b"hello");
        assert_eq!(&k[0..4], &[0, 0, 0, 5]);
        assert_eq!(&k[4..], b"hello");
    }

    #[test]
    fn cf_bounds_are_adjacent() {
        let lo = cf_lower_bound(7);
        let hi = cf_upper_bound(7);
        assert_eq!(lo, vec![0, 0, 0, 7]);
        assert_eq!(hi, vec![0, 0, 0, 8]);
        assert!(lo < hi);
    }

    #[test]
    fn meta_keys_live_under_reserved_cf() {
        let next = meta::next_id_key();
        assert_eq!(&next[0..4], &[0, 0, 0, 0]);
        let name = meta::name_key("foo");
        assert_eq!(&name[0..4], &[0, 0, 0, 0]);
        assert!(name.ends_with(b"foo"));
    }

    #[test]
    fn meta_name_from_key_parses() {
        let key = meta::name_key("widgets");
        assert_eq!(meta::name_from_key(&key), Some("widgets"));
    }

    #[test]
    fn meta_scan_range_is_tight() {
        let lo = meta::name_scan_prefix();
        let hi = meta::name_scan_upper();
        assert!(lo < hi);
        let foo = meta::name_key("foo");
        assert!(lo <= foo);
        assert!(foo < hi);
    }

    #[test]
    fn registry_refuses_to_mint_the_last_id() {
        // `cf_upper_bound` cannot express an exclusive bound above
        // `u32::MAX`, so a CF holding it could be neither iterated nor
        // dropped. The registry stops one short instead.
        let r = CfRegistry::new();
        r.load(std::iter::empty(), MAX_CF_ID);
        let id = r.next_id().expect("one id left");
        let last = r.publish(id, "last");
        assert_eq!(last.id, MAX_CF_ID);
        assert!(r.next_id().is_none());
        assert!(cf_lower_bound(last.id) < cf_upper_bound(last.id));
    }

    #[test]
    fn registry_ids_are_monotonic_and_never_reused() {
        let r = CfRegistry::new();
        let a = r.publish(r.next_id().expect("id space"), "alpha");
        let b = r.publish(r.next_id().expect("id space"), "beta");
        assert!(a.id < b.id);
        assert_eq!(r.get("alpha").unwrap(), a);
        assert_eq!(r.get("beta").unwrap(), b);
        r.retire(b.id);
        let again = r.publish(r.next_id().expect("id space"), "beta");
        assert!(again.id > b.id, "a dropped id is never handed out again");
        assert!(!r.is_live_handle(&b));
        assert!(r.is_live_handle(&again));
    }

    #[test]
    fn registry_default_cf_is_live_without_a_lookup() {
        let r = CfRegistry::new();
        assert!(r.contains_id(DEFAULT_CF_ID));
        assert!(!r.contains_id(META_CF_ID));
        assert!(!r.contains_id(DEFAULT_CF_ID + 1));

        let h = r.publish(r.next_id().expect("id space"), "x");
        assert!(r.contains_id(h.id));
        r.retire(h.id);
        assert!(!r.contains_id(h.id));
        assert!(r.get("x").is_none());
        assert!(r.contains_id(DEFAULT_CF_ID));
    }

    #[test]
    fn a_handle_names_its_family_by_id_and_name() {
        let r = CfRegistry::new();
        let h = r.publish(r.next_id().expect("id space"), "x");
        let renamed = ColumnFamilyHandle {
            name: Arc::new("y".to_string()),
            id: h.id,
        };
        assert!(r.is_live_handle(&h));
        assert!(!r.is_live_handle(&renamed), "another database's handle");
    }

    #[test]
    fn retiring_a_reused_name_leaves_the_newer_family() {
        let r = CfRegistry::new();
        let old = r.publish(r.next_id().expect("id space"), "x");
        r.retire(old.id);
        let new = r.publish(r.next_id().expect("id space"), "x");
        // A second retire of the old id must not take the name from the new
        // family.
        r.retire(old.id);
        assert_eq!(r.get("x"), Some(new));
        assert_eq!(r.names(), vec!["x".to_string()]);
    }
}
