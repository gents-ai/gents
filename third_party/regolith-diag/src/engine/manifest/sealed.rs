//! Sealed manifests: format 2 of the MANIFEST, every edit batch an AEAD
//! frame.
//!
//! ```text
//! stamp   ["REGOMAN"][format u8 = 2][salt: 16][stamp checksum u32]
//! batch   [len u32][key id u32][nonce: 12][sealed edits][tag: 16][checksum u32]
//! ```
//!
//! `len` counts everything between itself and the checksum, as a format 1
//! batch's does, and the checksum covers the same bytes, so a batch a crash
//! tore is recognised and dropped exactly as in format 1, without the key.
//! The checksum stays because the tag cannot do its job: telling a torn
//! batch from one sealed under a key that is wrong or unknown takes
//! knowing the key id is the one the writer wrote, before any key is
//! looked up. A batch whose checksum holds and whose tag fails is
//! therefore never a torn write, and refuses the open.
//!
//! Each batch names its own key, so a manifest appended to across a key
//! rotation reads back whole. The tag binds the batch to the manifest's
//! random salt, to the offset its `len` field sits at, and to the key id:
//! `[b'M'][salt][offset u64][key id u32]`.

use std::io;

use crate::encryption::KeyId;
use crate::engine::checksum;
use crate::engine::seal::{DOMAIN_MANIFEST, Keyring, NONCE_LEN, OVERHEAD, Sealer};

/// The format byte of a sealed manifest's stamp.
pub(super) const MANIFEST_FORMAT_SEALED: u8 = 2;

/// Bytes of a sealed manifest's stamp.
pub(super) const SEALED_STAMP_LEN: usize = 28;

const SALT_LEN: usize = 16;

/// What every batch of a sealed manifest is sealed with: the database's
/// keyring and this manifest's salt.
pub(super) struct ManifestSeal {
    pub(super) keyring: std::sync::Arc<Keyring>,
    pub(super) salt: [u8; SALT_LEN],
}

/// A fresh random salt for a new sealed manifest.
pub(super) fn fresh_salt() -> io::Result<[u8; SALT_LEN]> {
    let mut salt = [0u8; SALT_LEN];
    getrandom::fill(&mut salt)
        .map_err(|e| io::Error::other(format!("the operating system gave no random bytes: {e}")))?;
    Ok(salt)
}

/// The stamp a sealed manifest whose salt is `salt` begins with.
pub(super) fn encode_stamp(magic: &[u8; 7], salt: &[u8; SALT_LEN]) -> [u8; SEALED_STAMP_LEN] {
    let mut out = [0u8; SEALED_STAMP_LEN];
    out[0..7].copy_from_slice(magic);
    out[7] = MANIFEST_FORMAT_SEALED;
    out[8..24].copy_from_slice(salt);
    let sum = checksum::manifest_record(0, &out[0..24]);
    out[24..28].copy_from_slice(&sum.to_le_bytes());
    out
}

/// The salt of the sealed stamp at the head of `data`, whose magic and
/// format byte have been checked. `None` when the file is too short to
/// hold one: a crash while the manifest was created.
pub(super) fn decode_stamp(data: &[u8]) -> io::Result<Option<[u8; SALT_LEN]>> {
    let Some(stamp) = data.get(..SEALED_STAMP_LEN) else {
        return Ok(None);
    };
    let stored = u32::from_le_bytes([stamp[24], stamp[25], stamp[26], stamp[27]]);
    if stored != checksum::manifest_record(0, &stamp[0..24]) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "MANIFEST stamp checksum mismatch",
        ));
    }
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&stamp[8..24]);
    Ok(Some(salt))
}

fn aad(salt: &[u8; SALT_LEN], offset: u64, id: KeyId) -> [u8; 1 + SALT_LEN + 8 + 4] {
    let mut aad = [0u8; 1 + SALT_LEN + 8 + 4];
    aad[0] = DOMAIN_MANIFEST;
    aad[1..1 + SALT_LEN].copy_from_slice(salt);
    aad[1 + SALT_LEN..1 + SALT_LEN + 8].copy_from_slice(&offset.to_le_bytes());
    aad[1 + SALT_LEN + 8..].copy_from_slice(&id.0.to_le_bytes());
    aad
}

/// Append the edits `plain` to `out` as one sealed batch whose `len` field
/// lands at `offset` of the manifest, sealed under `sealer`.
pub(super) fn encode_batch(
    sealer: &Sealer,
    salt: &[u8; SALT_LEN],
    offset: u64,
    plain: &[u8],
    out: &mut Vec<u8>,
) -> io::Result<()> {
    let start = out.len();
    let body_len = u32::try_from(4 + plain.len() + OVERHEAD).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "MANIFEST edit batch exceeds u32 length",
        )
    })?;
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&sealer.id().0.to_le_bytes());
    sealer.seal_frame(&aad(salt, offset, sealer.id()), plain, out)?;
    let sum = checksum::manifest_record(body_len, &out[start + 4..]);
    out.extend_from_slice(&sum.to_le_bytes());
    Ok(())
}

/// The key the batch whose checksummed `body` sits after the `len` field at
/// `offset` names, and its edits, opened under that key.
pub(super) fn open_batch(
    keyring: &Keyring,
    salt: &[u8; SALT_LEN],
    offset: u64,
    body: &[u8],
) -> io::Result<(KeyId, Vec<u8>)> {
    let refused = |what: String| io::Error::new(io::ErrorKind::InvalidData, what);
    let Some((id, frame)) = body.split_first_chunk::<4>() else {
        return Err(refused(format!(
            "MANIFEST: the edit batch at offset {offset} is too short to be sealed"
        )));
    };
    let id = KeyId(u32::from_le_bytes(*id));
    let sealer = keyring.sealer(id)?;
    let mut frame = frame.to_vec();
    let Some(plain) = sealer.open_frame(&aad(salt, offset, id), &mut frame) else {
        return Err(refused(format!(
            "MANIFEST: the edit batch at offset {offset} does not verify under key id {id}; \
             the key is wrong or the file is damaged"
        )));
    };
    frame.truncate(plain.end);
    frame.drain(..NONCE_LEN);
    Ok((id, frame))
}

#[cfg(test)]
#[path = "sealed_tests.rs"]
mod tests;
