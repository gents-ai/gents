//! Sealed tables: the SSTable format with every region an AEAD frame.
//!
//! A sealed table has the layout of an unsealed one, region for region,
//! and differs in three places:
//!
//! ```text
//! data block   [nonce: 12][sealed payload || compression: u8][tag: 16]
//! meta region  [nonce: 12][sealed payload][tag: 16]
//! footer       [seven u64 fields: 56][key id: u32][reserved: u32 = 0]
//!              [salt: 16][nonce: 12][tag: 16][magic: u64]
//! ```
//!
//! The tag replaces the checksum each region carries in an unsealed table,
//! and the footer's tag replaces its checksum: the footer is authenticated
//! with nothing to encrypt. The compression byte moves inside the sealed
//! payload, at its end, so an uncompressed block is decrypted straight into
//! the buffer the block decoder keeps.
//!
//! Every frame's associated data is `[b'S'][region kind][salt][offset]`:
//! the kind is one of `checksum::META_KIND_*` or [`DATA_KIND`], the salt is
//! sixteen random bytes drawn when the table is created and kept in its
//! footer, and the offset is where the frame starts. A frame therefore
//! opens only as the region it was written as, at the offset it was written
//! at, in the table it was written in. The table's key id is in the footer
//! and covered by its tag, and every frame of a table is sealed under it.
//!
//! The magic says which layout a table has: `REGOSST\x07` sealed with a
//! flat index, `REGOSST\x08` sealed with a partitioned one.

use std::io;
use std::path::Path;

use super::{COMPRESSION_LZ4, COMPRESSION_NONE, COMPRESSION_SNAPPY, invalid_data};
use crate::encryption::KeyId;
use crate::engine::checksum;
use crate::engine::seal::{DOMAIN_SST, Keyring, NONCE_LEN, OVERHEAD, Sealer, TAG_LEN};
use crate::env::Env;

/// Sealed, flat index.
pub(super) const MAGIC_V7: u64 = 0x5245474F_53535407;

/// Sealed, partitioned index.
pub(super) const MAGIC_V8: u64 = 0x5245474F_53535408;

/// Bytes of a sealed footer.
pub(super) const SEALED_FOOTER_SIZE: usize = 116;

/// The region kind a data block's associated data names. Distinct from
/// every `checksum::META_KIND_*`.
const DATA_KIND: u8 = 0;

const SALT_LEN: usize = 16;

/// Where the footer's fields after the seven offsets sit.
const KEY_ID_AT: usize = 56;
const SALT_AT: usize = 64;
const NONCE_AT: usize = SALT_AT + SALT_LEN;
const TAG_AT: usize = NONCE_AT + NONCE_LEN;
const MAGIC_AT: usize = TAG_AT + TAG_LEN;

/// The key and the identity every frame of one sealed table is bound to.
#[derive(Clone)]
pub(crate) struct TableSeal {
    sealer: Sealer,
    salt: [u8; SALT_LEN],
}

impl TableSeal {
    /// The seal for a new table: the provider's current key and a fresh
    /// salt.
    pub(super) fn fresh(keyring: &Keyring) -> io::Result<Self> {
        let mut salt = [0u8; SALT_LEN];
        getrandom::fill(&mut salt).map_err(|e| {
            io::Error::other(format!("the operating system gave no random bytes: {e}"))
        })?;
        Ok(Self {
            sealer: keyring.current()?,
            salt,
        })
    }

    /// The key every frame of this table is sealed under.
    pub(crate) fn key_id(&self) -> KeyId {
        self.sealer.id()
    }

    fn aad(&self, kind: u8, offset: u64) -> [u8; 2 + SALT_LEN + 8] {
        let mut aad = [0u8; 2 + SALT_LEN + 8];
        aad[0] = DOMAIN_SST;
        aad[1] = kind;
        aad[2..2 + SALT_LEN].copy_from_slice(&self.salt);
        aad[2 + SALT_LEN..].copy_from_slice(&offset.to_le_bytes());
        aad
    }

    /// Append the metadata region `payload` of `kind`, written at `offset`,
    /// to `out` as one frame.
    pub(super) fn seal_region(
        &self,
        kind: u8,
        offset: u64,
        payload: &[u8],
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        self.sealer
            .seal_frame(&self.aad(kind, offset), payload, out)
    }

    /// Append the data block `payload`, compressed by `codec` and written at
    /// `offset`, to `out` as one frame.
    pub(super) fn seal_data_block(
        &self,
        offset: u64,
        codec: u8,
        payload: &[u8],
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        let start = out.len();
        out.reserve(payload.len() + 1 + OVERHEAD);
        out.extend_from_slice(&[0u8; NONCE_LEN]);
        out.extend_from_slice(payload);
        out.push(codec);
        let (nonce, tag) = self
            .sealer
            .seal(&self.aad(DATA_KIND, offset), &mut out[start + NONCE_LEN..])?;
        out[start..start + NONCE_LEN].copy_from_slice(&nonce);
        out.extend_from_slice(&tag);
        Ok(())
    }

    /// The payload of the metadata region of `kind` read whole at `offset`,
    /// decrypted in place and handed back in the same buffer.
    pub(super) fn open_region(
        &self,
        kind: u8,
        offset: u64,
        mut region: Vec<u8>,
        file_id: u64,
        name: &str,
    ) -> io::Result<Vec<u8>> {
        let Some(plain) = self.sealer.open_frame(&self.aad(kind, offset), &mut region) else {
            return Err(self.failed(file_id, name, offset));
        };
        region.truncate(plain.end);
        region.drain(..plain.start);
        Ok(region)
    }

    /// The compression byte and the payload of the data block frame `frame`
    /// read at `offset`, decrypted into a buffer of their own.
    pub(super) fn open_data_block(
        &self,
        offset: u64,
        frame: &[u8],
        file_id: u64,
    ) -> io::Result<(u8, Vec<u8>)> {
        if frame.len() < OVERHEAD + 1 {
            return Err(invalid_data("block frame too short"));
        }
        let body = NONCE_LEN..frame.len() - TAG_LEN;
        let mut plain = frame[body.clone()].to_vec();
        let nonce: [u8; NONCE_LEN] = frame[..NONCE_LEN]
            .try_into()
            .map_err(|_| invalid_data("block frame too short"))?;
        let tag: [u8; TAG_LEN] = frame[body.end..]
            .try_into()
            .map_err(|_| invalid_data("block frame too short"))?;
        if !self
            .sealer
            .open(&nonce, &self.aad(DATA_KIND, offset), &mut plain, &tag)
        {
            return Err(self.failed(file_id, "data block", offset));
        }
        let codec = plain
            .pop()
            .ok_or_else(|| invalid_data("block frame too short"))?;
        Ok((codec, plain))
    }

    fn failed(&self, file_id: u64, name: &str, offset: u64) -> io::Error {
        invalid_data(format!(
            "table {file_id:06}: the {name} at offset {offset} does not verify under key id {}; \
             the key is wrong or the table is damaged",
            self.key_id()
        ))
    }

    /// The footer of a sealed table, whose seven fields are `fields` and
    /// whose magic is `magic`.
    pub(super) fn encode_footer(
        &self,
        fields: &[u8; 56],
        magic: u64,
    ) -> io::Result<[u8; SEALED_FOOTER_SIZE]> {
        let mut out = [0u8; SEALED_FOOTER_SIZE];
        out[..56].copy_from_slice(fields);
        out[KEY_ID_AT..KEY_ID_AT + 4].copy_from_slice(&self.key_id().0.to_le_bytes());
        out[SALT_AT..NONCE_AT].copy_from_slice(&self.salt);
        out[MAGIC_AT..].copy_from_slice(&magic.to_le_bytes());
        let (nonce, tag) = self.sealer.seal(&footer_aad(&out), &mut [])?;
        out[NONCE_AT..TAG_AT].copy_from_slice(&nonce);
        out[TAG_AT..MAGIC_AT].copy_from_slice(&tag);
        Ok(out)
    }

    /// The seal of the table whose sealed footer is `footer`, once the
    /// footer verifies under the key it names. `path` names the table in
    /// the error.
    pub(super) fn open_footer(
        keyring: Option<&Keyring>,
        footer: &[u8; SEALED_FOOTER_SIZE],
        path: &Path,
    ) -> io::Result<Self> {
        let keyring = keyring.ok_or_else(|| crate::Error::KeyProviderRequired.into_io_error())?;
        let id = KeyId(u32::from_le_bytes(
            footer[KEY_ID_AT..KEY_ID_AT + 4]
                .try_into()
                .map_err(|_| invalid_data("SSTable footer is truncated"))?,
        ));
        if footer[KEY_ID_AT + 4..SALT_AT] != [0u8; 4] {
            return Err(invalid_data(format!(
                "{}: the table footer carries flags this version of regolith does not know",
                path.display()
            )));
        }
        let sealer = keyring.sealer(id)?;
        let nonce: [u8; NONCE_LEN] = footer[NONCE_AT..TAG_AT]
            .try_into()
            .map_err(|_| invalid_data("SSTable footer is truncated"))?;
        let tag: [u8; TAG_LEN] = footer[TAG_AT..MAGIC_AT]
            .try_into()
            .map_err(|_| invalid_data("SSTable footer is truncated"))?;
        if !sealer.open(&nonce, &footer_aad(footer), &mut [], &tag) {
            return Err(invalid_data(format!(
                "{}: the table footer does not verify under key id {id}; the key is wrong or \
                 the table is damaged",
                path.display()
            )));
        }
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&footer[SALT_AT..NONCE_AT]);
        Ok(Self { sealer, salt })
    }
}

/// Authenticate the footer of the table at `path` under the key it names,
/// without opening the rest of the table. An unsealed table's footer
/// checksum is checked instead, and needs no key.
///
/// What a restore checks for each table before it writes anything: a key
/// the provider has, but with bytes other than the ones that sealed the
/// table, fails here with [`io::ErrorKind::InvalidData`] (a key it does not
/// have fails with the provider's error), where opening the restored
/// database would otherwise be the first to find out. One read of the
/// footer per table.
pub(crate) fn authenticate_footer(
    env: &dyn Env,
    path: &Path,
    keyring: Option<&Keyring>,
) -> io::Result<()> {
    let file = env.open_read(path)?;
    let footer = super::read_footer(&*file, file.len()?)?;
    footer.open_seal(keyring, path).map(drop)
}

/// What a sealed footer's tag covers: every byte but the nonce and the tag.
fn footer_aad(footer: &[u8; SEALED_FOOTER_SIZE]) -> [u8; 2 + NONCE_AT + 8] {
    let mut aad = [0u8; 2 + NONCE_AT + 8];
    aad[0] = DOMAIN_SST;
    aad[1] = checksum::META_KIND_FOOTER;
    aad[2..2 + NONCE_AT].copy_from_slice(&footer[..NONCE_AT]);
    aad[2 + NONCE_AT..].copy_from_slice(&footer[MAGIC_AT..]);
    aad
}

/// The bytes the decoded block built from the sealed block payload `plain`,
/// compressed by `codec`, will hold, known before it is decompressed so a
/// size limit is checked first. An uncompressed block keeps `plain` itself.
pub(super) fn decoded_len(codec: u8, plain: &Vec<u8>) -> io::Result<u64> {
    match codec {
        COMPRESSION_NONE => Ok(plain.capacity() as u64),
        COMPRESSION_LZ4 | COMPRESSION_SNAPPY => plain
            .get(..4)
            .map(|b| u64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])))
            .ok_or_else(|| invalid_data("compressed block header too short")),
        _ => Err(invalid_data("unknown compression type")),
    }
}

#[cfg(test)]
#[path = "sealed_tests.rs"]
mod tests;
