//! The authenticated encryption every sealed frame uses, and the cache of
//! key schedules derived from a [`KeyProvider`]'s keys.
//!
//! A sealed frame is `[nonce: 12][ciphertext][tag: 16]`, AES-256-GCM-SIV
//! (RFC 8452) under the key a [`Sealer`] holds. The caller binds each frame
//! to where it belongs with the associated data: which file type and region
//! it is, the file's random identity, and its offset in the file. A frame
//! copied anywhere else therefore fails its tag, exactly as one whose bytes
//! changed does.
//!
//! Each frame's nonce is 12 bytes from the operating system's random
//! source, drawn per frame, so how a file is written (appends, a rollback
//! that rewrites an offset, a file copied and both copies written on) has
//! no bearing on whether two frames share one: only two random draws
//! colliding can. The chance that any two of `n` frames under one key draw
//! the same nonce is at most `n^2 / 2^97`, and GCM-SIV keeps even that case
//! to revealing only whether the two frames were identical. The full
//! argument is in [`crate::encryption`].

use std::io;
use std::ops::Range;
use std::sync::Arc;

use aes_gcm_siv::aead::{AeadInOut, KeyInit};
use aes_gcm_siv::{Aes256GcmSiv, Nonce, Tag};
use kovan_map::HopscotchMap;

use crate::encryption::{KeyId, KeyProvider};

/// Bytes of the nonce at the head of a sealed frame.
pub(crate) const NONCE_LEN: usize = 12;

/// Bytes of the tag at the end of a sealed frame.
pub(crate) const TAG_LEN: usize = 16;

/// Bytes a sealed frame adds to what it seals.
pub(crate) const OVERHEAD: usize = NONCE_LEN + TAG_LEN;

/// Domain bytes that lead every frame's associated data, so a frame of
/// one file type can never verify as another's. Part of the on-disk
/// format.
pub(crate) const DOMAIN_SST: u8 = b'S';
pub(crate) const DOMAIN_WAL: u8 = b'W';
pub(crate) const DOMAIN_MANIFEST: u8 = b'M';
pub(crate) const DOMAIN_BACKUP: u8 = b'B';

/// A database's key provider, with the key schedule of every key it has
/// handed out, derived once and wiped when the keyring drops.
///
/// Consulted when a file is created or opened and when a manifest batch is
/// written, never per block: a table or a log resolves its [`Sealer`] once
/// and holds it.
pub(crate) struct Keyring {
    provider: Arc<dyn KeyProvider>,
    ciphers: HopscotchMap<u32, Arc<Aes256GcmSiv>>,
}

impl Keyring {
    pub(crate) fn new(provider: Arc<dyn KeyProvider>) -> Self {
        Self {
            provider,
            ciphers: HopscotchMap::new(),
        }
    }

    /// The key new files are sealed under, as the provider names it now.
    pub(crate) fn current_id(&self) -> KeyId {
        self.provider.current()
    }

    /// The sealer for the key new files are sealed under.
    pub(crate) fn current(&self) -> io::Result<Sealer> {
        self.sealer(self.current_id())
    }

    /// The sealer for key `id`, or [`crate::Error::UnknownKey`] when the
    /// provider does not provide it.
    pub(crate) fn sealer(&self, id: KeyId) -> io::Result<Sealer> {
        if let Some(cipher) = self.ciphers.get(&id.0) {
            return Ok(Sealer { id, cipher });
        }
        let key = self
            .provider
            .key(id)
            .ok_or_else(|| crate::Error::UnknownKey { id }.into_io_error())?;
        let cipher = Arc::new(Aes256GcmSiv::new(key.as_bytes().into()));
        let cipher = self.ciphers.get_or_insert(id.0, cipher);
        Ok(Sealer { id, cipher })
    }
}

/// Twelve bytes from the operating system's random source: one frame's
/// nonce.
pub(crate) fn fresh_nonce() -> io::Result<[u8; NONCE_LEN]> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce)
        .map_err(|e| io::Error::other(format!("the operating system gave no random nonce: {e}")))?;
    Ok(nonce)
}

/// One key, ready to seal and open frames.
#[derive(Clone)]
pub(crate) struct Sealer {
    id: KeyId,
    cipher: Arc<Aes256GcmSiv>,
}

impl Sealer {
    /// The id of the key this sealer holds.
    pub(crate) fn id(&self) -> KeyId {
        self.id
    }

    /// Encrypt `buf` in place under a fresh nonce, binding `aad`, and hand
    /// back the nonce and the tag.
    pub(crate) fn seal(
        &self,
        aad: &[u8],
        buf: &mut [u8],
    ) -> io::Result<([u8; NONCE_LEN], [u8; TAG_LEN])> {
        let nonce = fresh_nonce()?;
        Ok((nonce, self.seal_with(&nonce, aad, buf)?))
    }

    /// Encrypt `buf` in place under `nonce`, binding `aad`, and hand back the
    /// tag. `nonce` must come from [`fresh_nonce`] and be used for this one
    /// frame: the commit pipeline draws it when it decides a commit, so every
    /// thread that writes that commit's record seals the same bytes.
    pub(crate) fn seal_with(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        buf: &mut [u8],
    ) -> io::Result<[u8; TAG_LEN]> {
        let tag = self
            .cipher
            .encrypt_inout_detached(&Nonce::from(*nonce), aad, buf.into())
            .map_err(|_| io::Error::other("a frame is too large to seal"))?;
        Ok(tag.into())
    }

    /// Decrypt `buf` in place. `false` when the tag does not verify under
    /// this key, `aad` and `nonce`; `buf` then holds no plaintext.
    pub(crate) fn open(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        buf: &mut [u8],
        tag: &[u8; TAG_LEN],
    ) -> bool {
        self.cipher
            .decrypt_inout_detached(&Nonce::from(*nonce), aad, buf.into(), &Tag::from(*tag))
            .is_ok()
    }

    /// Append `plain` to `out` as one sealed frame.
    pub(crate) fn seal_frame(&self, aad: &[u8], plain: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
        let start = out.len();
        out.reserve(plain.len() + OVERHEAD);
        out.extend_from_slice(&[0u8; NONCE_LEN]);
        out.extend_from_slice(plain);
        let (nonce, tag) = self.seal(aad, &mut out[start + NONCE_LEN..])?;
        out[start..start + NONCE_LEN].copy_from_slice(&nonce);
        out.extend_from_slice(&tag);
        Ok(())
    }

    /// Open the sealed frame `frame` in place, returning where its
    /// plaintext now sits; `None` when it is too short to be a frame or its
    /// tag fails.
    pub(crate) fn open_frame(&self, aad: &[u8], frame: &mut [u8]) -> Option<Range<usize>> {
        let body_len = frame.len().checked_sub(OVERHEAD)?;
        let (nonce, rest) = frame.split_at_mut(NONCE_LEN);
        let (body, tag) = rest.split_at_mut(body_len);
        let nonce: [u8; NONCE_LEN] = (&*nonce).try_into().ok()?;
        let tag: [u8; TAG_LEN] = (&*tag).try_into().ok()?;
        self.open(&nonce, aad, body, &tag)
            .then_some(NONCE_LEN..NONCE_LEN + body_len)
    }
}

#[cfg(test)]
pub(crate) mod test_keys {
    //! A key provider for tests: a fixed set of keys and a settable current
    //! one.

    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::encryption::{KeyId, KeyMaterial, KeyProvider};
    use crate::portability::{AtomicU64, Ordering};

    pub(crate) struct TestKeys {
        current: AtomicU64,
        keys: HashMap<u32, [u8; 32]>,
    }

    impl TestKeys {
        /// Keys `ids`, each derived from its id, with the first current.
        pub(crate) fn new(ids: &[u32]) -> Arc<Self> {
            Self::derived(ids, 0)
        }

        /// Keys `ids` derived from each id and `variant`: the same ids as
        /// [`TestKeys::new`] gives under other bytes when `variant` is not 0.
        pub(crate) fn derived(ids: &[u32], variant: u8) -> Arc<Self> {
            Arc::new(Self {
                current: AtomicU64::new(u64::from(ids[0])),
                keys: ids.iter().map(|&id| (id, key_bytes(id, variant))).collect(),
            })
        }

        pub(crate) fn set_current(&self, id: u32) {
            self.current.store(u64::from(id), Ordering::SeqCst);
        }
    }

    fn key_bytes(id: u32, variant: u8) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = (id as u8)
                .wrapping_mul(31)
                .wrapping_add(i as u8)
                .wrapping_add(variant.wrapping_mul(97));
        }
        out
    }

    impl KeyProvider for TestKeys {
        fn current(&self) -> KeyId {
            KeyId(self.current.load(Ordering::SeqCst) as u32)
        }

        fn key(&self, id: KeyId) -> Option<KeyMaterial> {
            self.keys.get(&id.0).map(|k| KeyMaterial::new(*k))
        }
    }

    /// A keyring over [`TestKeys`] for `ids`.
    pub(crate) fn keyring(ids: &[u32]) -> Arc<super::Keyring> {
        Arc::new(super::Keyring::new(TestKeys::new(ids)))
    }

    /// A keyring that names the same `ids` as [`keyring`] but holds other
    /// bytes under them: the wrong key.
    pub(crate) fn wrong_keyring(ids: &[u32]) -> Arc<super::Keyring> {
        Arc::new(super::Keyring::new(TestKeys::derived(ids, 1)))
    }
}

#[cfg(test)]
mod tests {
    use super::test_keys::{TestKeys, keyring};
    use super::*;

    #[test]
    fn a_frame_round_trips_and_carries_its_overhead() {
        let ring = keyring(&[1]);
        let sealer = ring.current().unwrap();
        let mut frame = vec![0xEE];
        sealer
            .seal_frame(b"aad", b"hello, world", &mut frame)
            .unwrap();
        assert_eq!(frame.len(), 1 + 12 + OVERHEAD);
        assert!(
            !frame.windows(5).any(|w| w == b"hello"),
            "plaintext leaked into the frame"
        );
        let range = sealer.open_frame(b"aad", &mut frame[1..]).unwrap();
        assert_eq!(&frame[1..][range], b"hello, world");
    }

    #[test]
    fn any_changed_byte_or_other_aad_fails_the_tag() {
        let ring = keyring(&[1]);
        let sealer = ring.current().unwrap();
        let mut frame = Vec::new();
        sealer.seal_frame(b"where", b"payload", &mut frame).unwrap();
        for i in 0..frame.len() {
            let mut bad = frame.clone();
            bad[i] ^= 0x01;
            assert!(
                sealer.open_frame(b"where", &mut bad).is_none(),
                "a flip at byte {i} verified"
            );
        }
        let mut copy = frame.clone();
        assert!(sealer.open_frame(b"elsewhere", &mut copy).is_none());
        assert!(sealer.open_frame(b"where", &mut frame[..5]).is_none());
    }

    #[test]
    fn a_frame_opens_only_under_the_key_that_sealed_it() {
        let ring = keyring(&[1, 2]);
        let one = ring.sealer(KeyId(1)).unwrap();
        let two = ring.sealer(KeyId(2)).unwrap();
        let mut frame = Vec::new();
        one.seal_frame(b"", b"secret", &mut frame).unwrap();
        assert!(two.open_frame(b"", &mut frame.clone()).is_none());
        assert!(one.open_frame(b"", &mut frame).is_some());
    }

    #[test]
    fn an_unknown_key_is_a_typed_error() {
        let ring = keyring(&[1]);
        let err = ring.sealer(KeyId(5)).err().unwrap();
        assert!(matches!(
            crate::Error::from(err),
            crate::Error::UnknownKey { id: KeyId(5) }
        ));
    }

    #[test]
    fn the_current_key_follows_the_provider_and_ciphers_are_derived_once() {
        let keys = TestKeys::new(&[1, 2]);
        let ring = Keyring::new(keys.clone());
        assert_eq!(ring.current().unwrap().id(), KeyId(1));
        keys.set_current(2);
        let two = ring.current().unwrap();
        assert_eq!(two.id(), KeyId(2));
        let again = ring.sealer(KeyId(2)).unwrap();
        assert!(Arc::ptr_eq(&two.cipher, &again.cipher));
    }

    #[test]
    fn every_frame_draws_its_own_nonce() {
        let ring = keyring(&[1]);
        let sealer = ring.current().unwrap();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10_000 {
            let mut buf = *b"same plaintext";
            let (nonce, _) = sealer.seal(b"same aad", &mut buf).unwrap();
            assert!(seen.insert(nonce), "a nonce repeated");
        }
    }
}
