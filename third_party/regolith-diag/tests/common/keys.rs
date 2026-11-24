//! A key provider for tests of encryption at rest: a fixed set of key ids,
//! each key derived from its id, with a current id the test can move.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use regolith::{KeyId, KeyMaterial, KeyProvider};

pub struct Keys {
    current: AtomicU32,
    provided: Vec<u32>,
    /// Which bytes each id names: two providers with different variants
    /// name the same ids but hold different keys under them.
    variant: u8,
    /// Ids whose bytes are the wrong ones whatever the variant.
    wrong: Vec<u32>,
}

impl Keys {
    /// Keys `ids`, the first current.
    pub fn new(ids: &[u32]) -> Arc<Self> {
        Self::variant(ids, 0)
    }

    /// The same ids as [`Keys::new`] under other bytes: the wrong keys.
    pub fn wrong(ids: &[u32]) -> Arc<Self> {
        Self::variant(ids, 1)
    }

    /// Keys `ids`, the first current, with the right bytes except under
    /// the ids in `wrong`.
    pub fn wrong_for(ids: &[u32], wrong: &[u32]) -> Arc<Self> {
        Arc::new(Self {
            current: AtomicU32::new(ids[0]),
            provided: ids.to_vec(),
            variant: 0,
            wrong: wrong.to_vec(),
        })
    }

    fn variant(ids: &[u32], variant: u8) -> Arc<Self> {
        Arc::new(Self {
            current: AtomicU32::new(ids[0]),
            provided: ids.to_vec(),
            variant,
            wrong: Vec::new(),
        })
    }

    /// Seal new files under `id` from now on.
    pub fn set_current(&self, id: u32) {
        self.current.store(id, Ordering::SeqCst);
    }
}

impl KeyProvider for Keys {
    fn current(&self) -> KeyId {
        KeyId(self.current.load(Ordering::SeqCst))
    }

    fn key(&self, id: KeyId) -> Option<KeyMaterial> {
        if !self.provided.contains(&id.0) {
            return None;
        }
        let variant = if self.wrong.contains(&id.0) {
            self.variant.wrapping_add(1)
        } else {
            self.variant
        };
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (id.0 as u8)
                .wrapping_mul(29)
                .wrapping_add(i as u8)
                .wrapping_add(variant.wrapping_mul(101));
        }
        Some(KeyMaterial::new(bytes))
    }
}
