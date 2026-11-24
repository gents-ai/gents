//! The filter region of an SSTable: the user-key bloom filter plus the
//! optional prefix bloom filter, decoded as one unit.
//!
//! On-disk format, written by `SsTableWriter::finish`:
//!
//! ```text
//! [prefix section length: 7 bytes LE][prefix form: u8][prefix section][user-key bloom bytes]
//! prefix section, by form:
//!   0, 1  [prefix bloom bytes]
//!   2     [extractor name length: varint][extractor name][prefix bloom bytes]
//! ```
//!
//! A length of `0` means the file was written without a prefix extractor,
//! and the whole header is then zero. The header is written
//! unconditionally, so the region decodes the same way either way.
//!
//! The prefix form (`prefix_call::PrefixForm`) says which key the prefix
//! bloom's entries were built from and whether the table names the
//! extractor that built them. Only a table of form `2` is asked, and only
//! by a seek under an extractor of the name it records; a table of form `0`
//! or `1` records no name and is read without its prefix filter. The form's
//! byte was the top byte of an eight-byte length before the forms existed,
//! and no table has a prefix bloom of 2^56 bytes, so every table written
//! then reads as form `0`. A form this build does not know is a later one:
//! its prefix bloom is left unused, which rules nothing out. A build from
//! before the forms reads the header of a later form as a length past the
//! region and refuses the table, where asking its bloom the old way would
//! have ruled out keys it holds.
//!
//! The two filters travel together because the footer describes them as
//! one region: one region, one decode, one cache entry, one charge.

use std::io;

use super::block::{encode_varint, read_varint};
use super::bloom::{BloomFilter, decode_bloom_block};
use super::prefix_call::{PrefixForm, PrefixProbe};

/// Header bytes in front of the prefix section.
const PREFIX_LEN_HEADER: usize = 8;

/// Where the header's top byte, the prefix bloom's form, starts.
const PREFIX_FORM_SHIFT: u32 = 56;

/// The header's low seven bytes: the prefix section's length.
const PREFIX_LEN_MASK: u64 = (1 << PREFIX_FORM_SHIFT) - 1;

/// Smallest legal encoded bloom block (`decode_bloom_block` reads a
/// 4-byte `num_hashes` header).
const MIN_BLOOM_BLOCK: usize = 4;

/// The header of a bloom region whose prefix section is `prefix_len` bytes
/// of `form`. A region with no prefix bloom has an all-zero header,
/// whatever the form.
pub(crate) fn prefix_header(
    form: PrefixForm,
    prefix_len: usize,
) -> io::Result<[u8; PREFIX_LEN_HEADER]> {
    let len = u64::try_from(prefix_len)
        .ok()
        .filter(|len| *len <= PREFIX_LEN_MASK)
        .ok_or_else(|| invalid_data("prefix bloom is too large to record"))?;
    let form = if len == 0 { 0 } else { u64::from(form.tag()) };
    Ok(((form << PREFIX_FORM_SHIFT) | len).to_le_bytes())
}

/// The prefix section of a table whose prefix bloom of `form` is `bloom`,
/// built by the extractor named `name`: the bloom, behind the name in the
/// named form. Empty when there is no bloom.
pub(crate) fn prefix_section(form: PrefixForm, name: &str, bloom: Vec<u8>) -> Vec<u8> {
    if form != PrefixForm::Named || bloom.is_empty() {
        return bloom;
    }
    let mut out = Vec::with_capacity(10 + name.len() + bloom.len());
    encode_varint(&mut out, name.len() as u64);
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&bloom);
    out
}

/// Both bloom filters of one SSTable.
pub(crate) struct FilterBlock {
    user: BloomFilter,
    /// The name of the extractor that built the prefix bloom, and the
    /// bloom. `None` for a table with no prefix bloom, or one of a form
    /// that records no name or that this build does not know: a seek reads
    /// such a table without its prefix filter.
    prefix: Option<(Box<[u8]>, BloomFilter)>,
    /// The form of the prefix bloom, when the table has one of a known form.
    #[cfg(test)]
    form: Option<PrefixForm>,
    charge: usize,
}

impl FilterBlock {
    /// Decode the filter region read from an SSTable's bloom range.
    ///
    /// Rejects a region too short to hold the length header, a prefix
    /// section that runs past the region, a name that runs past its section,
    /// and either bloom block being too short to carry its own header.
    pub(crate) fn decode(region: &[u8]) -> io::Result<Self> {
        let Some(header) = region.first_chunk::<PREFIX_LEN_HEADER>() else {
            return Err(invalid_data(
                "bloom region too short for prefix-bloom length header",
            ));
        };
        let header = u64::from_le_bytes(*header);
        let form = PrefixForm::from_tag((header >> PREFIX_FORM_SHIFT) as u8);
        let prefix_len = usize::try_from(header & PREFIX_LEN_MASK)
            .map_err(|_| invalid_data("prefix bloom length is too large to address"))?;
        let user_offset = PREFIX_LEN_HEADER
            .checked_add(prefix_len)
            .ok_or_else(|| invalid_data("prefix bloom length overflows"))?;
        if user_offset > region.len() {
            return Err(invalid_data("prefix bloom length exceeds bloom region"));
        }
        if prefix_len > 0 && prefix_len < MIN_BLOOM_BLOCK {
            return Err(invalid_data("prefix bloom block too short"));
        }
        if region.len() - user_offset < MIN_BLOOM_BLOCK {
            return Err(invalid_data("user bloom block too short"));
        }

        let section = &region[PREFIX_LEN_HEADER..user_offset];
        let form = form.filter(|_| prefix_len > 0);
        let prefix = match form {
            Some(PrefixForm::Named) => {
                let mut at = 0;
                let name_len = read_varint(section, &mut at)
                    .map_err(|_| invalid_data("prefix bloom name length is malformed"))?;
                let name = at
                    .checked_add(name_len)
                    .and_then(|end| section.get(at..end))
                    .ok_or_else(|| invalid_data("prefix bloom name exceeds its section"))?;
                let bloom = &section[at + name.len()..];
                if bloom.len() < MIN_BLOOM_BLOCK {
                    return Err(invalid_data("prefix bloom block too short"));
                }
                Some((Box::from(name), decode_bloom_block(bloom)))
            }
            _ => None,
        };
        let user = decode_bloom_block(&region[user_offset..]);

        Ok(Self {
            user,
            prefix,
            #[cfg(test)]
            form,
            // The decoded filters hold the region's bit arrays minus the
            // two 4-byte `num_hashes` headers and the 8-byte length
            // header, so the region length is the charge to within a
            // handful of bytes and never under-reports.
            charge: std::mem::size_of::<Self>() + region.len(),
        })
    }

    /// Whether this SSTable *might* contain `user_key`. `false` means
    /// definitely not.
    pub(crate) fn may_contain(&self, user_key: &[u8]) -> bool {
        self.user.may_contain(user_key)
    }

    /// Whether this SSTable *might* contain a key of the seek `probe` is
    /// for. Conservatively `true` when the file carries no prefix bloom
    /// this seek can ask: none, one an extractor of another name built, or
    /// one that records no name; and when the seek has no key, since there
    /// is then no negative information to act on.
    pub(crate) fn may_have_prefix(&self, probe: &PrefixProbe) -> bool {
        match &self.prefix {
            Some((built_by, filter)) => probe
                .key_for(built_by)
                .is_none_or(|key| filter.may_contain(key)),
            None => true,
        }
    }

    /// The form of this table's prefix bloom, when it has one of a form this
    /// build knows.
    #[cfg(test)]
    pub(crate) fn prefix_form(&self) -> Option<PrefixForm> {
        self.form
    }

    /// Heap bytes held, for block-cache charging.
    pub(crate) fn charge(&self) -> usize {
        self.charge
    }
}

impl std::fmt::Debug for FilterBlock {
    /// Shape only: a bloom filter's bits are not readable and printing
    /// them would leak nothing useful and a great deal of noise.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterBlock")
            .field("has_prefix_bloom", &self.prefix.is_some())
            .field("charge", &self.charge)
            .finish()
    }
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::column_family::prefix_key;
    use crate::engine::bloom::{BloomFilterBuilder, encode_bloom_block};
    use crate::options::{FixedLengthPrefix, PrefixExtractor};

    fn bloom(keys: &[&[u8]]) -> Vec<u8> {
        let mut builder = BloomFilterBuilder::new(10);
        for key in keys {
            builder.add_key(key);
        }
        encode_bloom_block(&builder.build())
    }

    /// A bloom region with `section` as its prefix section, behind the
    /// header `header` makes for its length.
    fn region_with(
        header: impl Fn(usize) -> [u8; PREFIX_LEN_HEADER],
        section: &[u8],
        user_keys: &[&[u8]],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&header(section.len()));
        out.extend_from_slice(section);
        out.extend_from_slice(&bloom(user_keys));
        out
    }

    /// A region as this build writes it, of `form`, by the extractor named
    /// `name`.
    fn region(
        form: PrefixForm,
        name: &str,
        prefix_keys: Option<&[&[u8]]>,
        user_keys: &[&[u8]],
    ) -> Vec<u8> {
        let section = prefix_section(form, name, prefix_keys.map(bloom).unwrap_or_default());
        region_with(|len| prefix_header(form, len).unwrap(), &section, user_keys)
    }

    /// A region as a build from before the forms wrote it: the header is
    /// the length, eight bytes of it.
    fn region_before_forms(prefix_keys: Option<&[&[u8]]>, user_keys: &[&[u8]]) -> Vec<u8> {
        let section = prefix_keys.map(bloom).unwrap_or_default();
        region_with(|len| (len as u64).to_le_bytes(), &section, user_keys)
    }

    /// Whether `filter` lets a seek for the caller's `prefix` in family 1
    /// through, under `extractor`.
    fn lets_through(filter: &FilterBlock, extractor: &dyn PrefixExtractor, prefix: &[u8]) -> bool {
        let prefixed = prefix_key(1, prefix);
        filter.may_have_prefix(&PrefixProbe::new(extractor, &prefixed))
    }

    const FORMS: [PrefixForm; 3] = [
        PrefixForm::EngineKey,
        PrefixForm::CallerKey,
        PrefixForm::Named,
    ];

    #[test]
    fn decodes_region_without_prefix_bloom() {
        let three = FixedLengthPrefix(3);
        for form in FORMS {
            let bytes = region(form, three.name(), None, &[b"alpha", b"beta"]);
            assert_eq!(bytes[..PREFIX_LEN_HEADER], [0; PREFIX_LEN_HEADER]);
            assert_eq!(bytes, region_before_forms(None, &[b"alpha", b"beta"]));
            let filter = FilterBlock::decode(&bytes).unwrap();
            assert!(filter.may_contain(b"alpha"));
            assert!(filter.may_contain(b"beta"));
            assert_eq!(filter.prefix_form(), None);
            assert!(
                lets_through(&filter, &three, b"anything"),
                "no prefix bloom means no negative information"
            );
        }
    }

    #[test]
    fn a_named_prefix_bloom_is_asked_only_under_its_extractors_name() {
        let three = FixedLengthPrefix(3);
        let entry = prefix_key(1, b"pre");
        let bytes = region(
            PrefixForm::Named,
            three.name(),
            Some(&[&entry]),
            &[b"prefix-key"],
        );
        assert_eq!(bytes[PREFIX_LEN_HEADER - 1], PrefixForm::Named.tag());
        let filter = FilterBlock::decode(&bytes).unwrap();
        assert!(filter.may_contain(b"prefix-key"));
        assert_eq!(filter.prefix_form(), Some(PrefixForm::Named));
        assert!(lets_through(&filter, &three, b"pre"));
        assert!(lets_through(&filter, &three, b"prefix"));
        assert!(!lets_through(&filter, &three, b"zzz"));
        assert!(
            lets_through(&filter, &three, b"pr"),
            "the extractor gives no key for it, so nothing is ruled out"
        );
        // Another width is another extractor: its prefixes are not the ones
        // the bloom holds, so the bloom is not asked.
        let four = FixedLengthPrefix(4);
        assert_ne!(four.name(), three.name());
        assert!(lets_through(&filter, &four, b"zzzz"));
    }

    /// A table that records no name is read without its prefix filter, in
    /// either older form, whatever its bloom holds.
    #[test]
    fn a_prefix_bloom_that_names_no_extractor_is_not_asked() {
        let three = FixedLengthPrefix(3);
        for form in [PrefixForm::EngineKey, PrefixForm::CallerKey] {
            let entry = prefix_key(1, b"pre");
            let bytes = region(form, three.name(), Some(&[&entry]), &[b"prefix-key"]);
            let filter = FilterBlock::decode(&bytes).unwrap();
            assert_eq!(filter.prefix_form(), Some(form));
            assert!(lets_through(&filter, &three, b"zzz"));
            assert!(filter.may_contain(b"prefix-key"));
        }
        let before = region_before_forms(Some(&[&[9, 9, 9]]), &[b"prefix-key"]);
        let filter = FilterBlock::decode(&before).unwrap();
        assert_eq!(filter.prefix_form(), Some(PrefixForm::EngineKey));
        assert!(lets_through(&filter, &three, b"pre"));
    }

    #[test]
    fn a_prefix_bloom_of_an_unknown_form_is_left_unused_and_the_user_bloom_still_read() {
        let three = FixedLengthPrefix(3);
        let entry = prefix_key(1, b"pre");
        let mut bytes = region(
            PrefixForm::Named,
            three.name(),
            Some(&[&entry]),
            &[b"prefix-key"],
        );
        bytes[PREFIX_LEN_HEADER - 1] = 3;
        let filter = FilterBlock::decode(&bytes).unwrap();
        assert_eq!(filter.prefix_form(), None);
        assert!(
            lets_through(&filter, &three, b"zzz"),
            "nothing is ruled out"
        );
        assert!(filter.may_contain(b"prefix-key"));
        assert!(!filter.may_contain(b"another-key"));
    }

    /// What a build from before the forms makes of a later table: it reads
    /// the whole header as the length, which runs past the region, so it
    /// refuses the table instead of asking its bloom the old way.
    #[test]
    fn a_later_header_read_as_a_plain_length_runs_past_the_region() {
        let entry = prefix_key(1, b"pre");
        for form in [PrefixForm::CallerKey, PrefixForm::Named] {
            let bytes = region(form, "x", Some(&[&entry]), &[b"prefix-key"]);
            let header = u64::from_le_bytes(bytes[..PREFIX_LEN_HEADER].try_into().unwrap());
            assert!(header > bytes.len() as u64);
            assert_eq!(header >> PREFIX_FORM_SHIFT, u64::from(form.tag()));
        }
    }

    #[test]
    fn a_name_that_runs_past_its_section_is_refused() {
        let bloom = bloom(&[b"k"]);
        // A name length past the section, and one that never ends.
        let mut long = Vec::new();
        encode_varint(&mut long, 1 << 20);
        long.extend_from_slice(&bloom);
        let endless = vec![0x80; 4];
        // A whole name, and too little bloom behind it.
        let mut short_bloom = Vec::new();
        encode_varint(&mut short_bloom, 2);
        short_bloom.extend_from_slice(b"ab\0\0");
        for section in [long, endless, short_bloom] {
            let bytes = region_with(
                |len| prefix_header(PrefixForm::Named, len).unwrap(),
                &section,
                &[b"k"],
            );
            let err = FilterBlock::decode(&bytes).expect_err("a malformed name");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn a_prefix_bloom_too_large_for_the_header_is_refused_not_truncated() {
        let largest = usize::try_from(PREFIX_LEN_MASK).unwrap_or(usize::MAX);
        assert!(prefix_header(PrefixForm::Named, largest).is_ok());
        if let Some(too_large) = largest.checked_add(1) {
            let err = prefix_header(PrefixForm::Named, too_large).expect_err("too large");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn rejects_short_region() {
        for len in 0..PREFIX_LEN_HEADER {
            let err = FilterBlock::decode(&vec![0u8; len]).expect_err("short region");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn rejects_prefix_length_past_region() {
        let mut bytes = region(PrefixForm::Named, "x", None, &[b"k"]);
        for form in FORMS {
            let claimed = prefix_header(form, bytes.len() + 1).unwrap();
            bytes[..PREFIX_LEN_HEADER].copy_from_slice(&claimed);
            let err = FilterBlock::decode(&bytes).expect_err("prefix past end");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn rejects_truncated_prefix_bloom() {
        let mut bytes = region(PrefixForm::Named, "x", None, &[b"k"]);
        for form in FORMS {
            bytes[..PREFIX_LEN_HEADER].copy_from_slice(&prefix_header(form, 2).unwrap());
            let err = FilterBlock::decode(&bytes).expect_err("prefix bloom too short");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn rejects_truncated_user_bloom() {
        let bytes = vec![0u8; PREFIX_LEN_HEADER + 3];
        let err = FilterBlock::decode(&bytes).expect_err("user bloom too short");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn charge_covers_the_whole_region() {
        let entry = prefix_key(1, b"pre");
        let bytes = region(PrefixForm::Named, "x", Some(&[&entry]), &[b"a", b"b", b"c"]);
        let filter = FilterBlock::decode(&bytes).unwrap();
        assert!(filter.charge() >= bytes.len());
    }
}
