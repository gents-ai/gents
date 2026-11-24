//! Every call the engine makes of the caller's [`PrefixExtractor`], and the
//! forms a table's prefix bloom comes in.
//!
//! The extractor is the caller's code, and it carves a prefix out of the
//! caller's key. So a table built here asks it about the key as the caller
//! wrote it, never about one of the database's own entries, and a prefix
//! seek asks it about the prefix the caller passed. What the table's prefix
//! bloom then holds is the id of the key's column family followed by what
//! the extractor returned: the id is put back so that one prefix in two
//! column families is two entries, and a seek in one family is not answered
//! by the other's keys.
//!
//! A table records the extractor that built its prefix bloom by name, in the
//! [`PrefixForm::Named`] form every table is written in. A seek asks a
//! table's bloom only when that name is the name of the extractor the
//! database runs with now: a bloom built from other prefixes can say a
//! prefix is absent from a table that holds it. A table of an older form
//! records no name, so a seek reads it without its prefix filter:
//! [`PrefixForm::EngineKey`], whose entries the extractor carved out of the
//! engine's key (the caller's behind its family id), and
//! [`PrefixForm::CallerKey`], the caller's key with no name. A compaction
//! rewrites either into the named form.
//!
//! A table says which form it holds in the header of its bloom region
//! (`filter_block`).

use crate::column_family::{CF_PREFIX_LEN, META_CF_ID};
use crate::options::PrefixExtractor;

/// Which key a table's prefix bloom was built from, and whether the table
/// names the extractor that built it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrefixForm {
    /// The engine's key, with no name. Read; written only by this crate's
    /// tests.
    EngineKey,
    /// The caller's key, with the family id in front of each entry, and no
    /// name. Read; written only by this crate's tests.
    CallerKey,
    /// The caller's key, as in [`PrefixForm::CallerKey`], and the name of
    /// the extractor that built it.
    Named,
}

impl PrefixForm {
    /// The byte a table records this form as.
    pub(crate) fn tag(self) -> u8 {
        match self {
            Self::EngineKey => 0,
            Self::CallerKey => 1,
            Self::Named => 2,
        }
    }

    /// The form a table recorded as `tag`, or `None` for a form this build
    /// does not know: a later one, whose bloom it then does not use.
    pub(crate) fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::EngineKey),
            1 => Some(Self::CallerKey),
            2 => Some(Self::Named),
            _ => None,
        }
    }
}

/// Split an engine key into its family id and the caller's key. `None` for a
/// key too short to carry an id, and for one of the database's own entries,
/// which the caller's extractor is never asked about.
fn callers(prefixed: &[u8]) -> Option<(&[u8; CF_PREFIX_LEN], &[u8])> {
    let (family, key) = prefixed.split_first_chunk::<CF_PREFIX_LEN>()?;
    (u32::from_be_bytes(*family) != META_CF_ID).then_some((family, key))
}

/// The bloom entry of `form` for the engine key `prefixed`, written into
/// `out`. In the caller-key forms that is the family id, then what the
/// extractor carves out of the caller's key. `false`, with `out` empty, when
/// the key gets no entry.
pub(crate) fn entry(
    form: PrefixForm,
    extractor: &dyn PrefixExtractor,
    prefixed: &[u8],
    out: &mut Vec<u8>,
) -> bool {
    out.clear();
    match form {
        PrefixForm::CallerKey | PrefixForm::Named => {
            let Some((family, key)) = callers(prefixed) else {
                return false;
            };
            let Some(prefix) = extractor.extract(key) else {
                return false;
            };
            out.extend_from_slice(family);
            out.extend_from_slice(prefix);
        }
        PrefixForm::EngineKey => {
            let Some(prefix) = extractor.extract(prefixed) else {
                return false;
            };
            out.extend_from_slice(prefix);
        }
    }
    true
}

/// The bloom key of one prefix seek, and the name of the extractor that
/// made it.
pub(crate) struct PrefixProbe {
    /// The name of the extractor the database runs with.
    name: &'static str,
    /// `None` when the extractor derives no bloom key from the caller's
    /// prefix: the seek then rules out no table.
    caller_key: Option<Vec<u8>>,
}

impl PrefixProbe {
    /// The probe of a seek for the engine prefix `prefixed`. The extractor
    /// is asked about the caller's prefix here, once.
    pub(crate) fn new(extractor: &dyn PrefixExtractor, prefixed: &[u8]) -> Self {
        let caller_key = callers(prefixed).and_then(|(family, prefix)| {
            let extracted = extractor.extract_query(prefix)?;
            let mut key = Vec::with_capacity(CF_PREFIX_LEN + extracted.len());
            key.extend_from_slice(family);
            key.extend_from_slice(extracted);
            Some(key)
        });
        Self {
            name: extractor.name(),
            caller_key,
        }
    }

    /// The bloom key to ask a table's prefix bloom with, when the extractor
    /// named `built_by` built it; `None` when there is none, and the table
    /// is then not ruled out. A bloom another extractor built is never
    /// asked.
    pub(crate) fn key_for(&self, built_by: &[u8]) -> Option<&[u8]> {
        if built_by == self.name.as_bytes() {
            self.caller_key.as_deref()
        } else {
            None
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Test seam: while set, a table this thread starts builds its prefix
    /// bloom in this form.
    static WRITE_FORM: std::cell::Cell<Option<PrefixForm>> = const { std::cell::Cell::new(None) };
}

/// The form a table started now builds its prefix bloom in: the named form,
/// except in a test of this crate that asked for an older one.
pub(crate) fn form_to_write() -> PrefixForm {
    #[cfg(test)]
    if let Some(form) = WRITE_FORM.with(std::cell::Cell::get) {
        return form;
    }
    PrefixForm::Named
}

/// Test-only: run `f` with every table this thread starts built in `form`,
/// as tables written before the next form were.
#[cfg(test)]
pub(crate) fn writing_form<T>(form: PrefixForm, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<PrefixForm>);
    impl Drop for Restore {
        fn drop(&mut self) {
            WRITE_FORM.with(|cell| cell.set(self.0));
        }
    }
    let _restore = Restore(WRITE_FORM.with(|cell| cell.replace(Some(form))));
    f()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::column_family::prefix_key;
    use crate::options::FixedLengthPrefix;

    /// The first three bytes, recording what it is asked.
    #[derive(Default)]
    struct Asked {
        keys: Mutex<Vec<Vec<u8>>>,
        seeks: Mutex<Vec<Vec<u8>>>,
    }

    impl PrefixExtractor for Asked {
        fn extract<'a>(&self, key: &'a [u8]) -> Option<&'a [u8]> {
            self.keys.lock().unwrap().push(key.to_vec());
            FixedLengthPrefix(3).extract(key)
        }

        fn extract_query<'a>(&self, prefix: &'a [u8]) -> Option<&'a [u8]> {
            self.seeks.lock().unwrap().push(prefix.to_vec());
            FixedLengthPrefix(3).extract_query(prefix)
        }

        fn name(&self) -> &'static str {
            "asked"
        }
    }

    #[test]
    fn an_entry_is_the_family_id_and_the_prefix_of_the_callers_key() {
        for form in [PrefixForm::Named, PrefixForm::CallerKey] {
            let extractor = Asked::default();
            let mut out = vec![9];
            assert!(entry(form, &extractor, &prefix_key(7, b"abcdef"), &mut out));
            assert_eq!(out, prefix_key(7, b"abc"));
            // Too short for the extractor: no entry.
            assert!(!entry(form, &extractor, &prefix_key(7, b"ab"), &mut out));
            assert!(out.is_empty());
            assert_eq!(
                *extractor.keys.lock().unwrap(),
                [b"abcdef".to_vec(), b"ab".to_vec()],
                "asked about the caller's key"
            );
        }
    }

    #[test]
    fn the_databases_own_entries_get_no_entry_and_are_not_shown() {
        let extractor = Asked::default();
        let form = PrefixForm::Named;
        let registry = prefix_key(META_CF_ID, b"name:x");
        let mut out = Vec::new();
        assert!(!entry(form, &extractor, &registry, &mut out));
        assert!(!entry(form, &extractor, b"abc", &mut out), "no family id");
        let registry_prefix = prefix_key(META_CF_ID, b"name:");
        let probe = PrefixProbe::new(&extractor, &registry_prefix);
        assert_eq!(probe.key_for(b"asked"), None);
        assert!(extractor.keys.lock().unwrap().is_empty());
        assert!(extractor.seeks.lock().unwrap().is_empty());
    }

    #[test]
    fn an_engine_key_entry_is_what_the_extractor_carves_out_of_the_engines_key() {
        let extractor = Asked::default();
        let prefixed = prefix_key(7, b"abcdef");
        let mut out = Vec::new();
        assert!(entry(
            PrefixForm::EngineKey,
            &extractor,
            &prefixed,
            &mut out
        ));
        assert_eq!(out, [0, 0, 0], "three bytes of the family id");
        assert_eq!(*extractor.keys.lock().unwrap(), [prefixed]);
    }

    /// A seek asks the extractor about the caller's prefix once, and has a
    /// key only for a table the same extractor built.
    #[test]
    fn a_seek_asks_only_a_table_built_by_an_extractor_of_the_same_name() {
        let extractor = Asked::default();
        let prefixed = prefix_key(7, b"abcd");
        let probe = PrefixProbe::new(&extractor, &prefixed);
        let caller_key = prefix_key(7, b"abc");
        assert_eq!(probe.key_for(b"asked"), Some(&caller_key[..]));
        assert_eq!(probe.key_for(b"other"), None);
        assert_eq!(probe.key_for(b""), None);
        assert_eq!(probe.key_for(b"aske"), None);
        assert_eq!(*extractor.seeks.lock().unwrap(), [b"abcd".to_vec()]);
    }

    /// A prefix shorter than the extractor's width rules out no table.
    #[test]
    fn a_prefix_the_extractor_gives_no_key_for_rules_out_no_table() {
        let extractor = Asked::default();
        let prefixed = prefix_key(7, b"ab");
        assert_eq!(
            PrefixProbe::new(&extractor, &prefixed).key_for(b"asked"),
            None
        );
        let short = [0, 7];
        assert_eq!(PrefixProbe::new(&extractor, &short).key_for(b"asked"), None);
    }

    #[test]
    fn a_form_round_trips_through_its_tag_and_an_unknown_tag_is_no_form() {
        for form in [
            PrefixForm::EngineKey,
            PrefixForm::CallerKey,
            PrefixForm::Named,
        ] {
            assert_eq!(PrefixForm::from_tag(form.tag()), Some(form));
        }
        assert_eq!(PrefixForm::from_tag(3), None);
        assert_eq!(PrefixForm::from_tag(0xFF), None);
    }

    #[test]
    fn a_table_is_started_in_the_named_form_unless_a_test_asks_for_an_older_one() {
        assert_eq!(form_to_write(), PrefixForm::Named);
        for form in [PrefixForm::EngineKey, PrefixForm::CallerKey] {
            writing_form(form, || assert_eq!(form_to_write(), form));
        }
        assert_eq!(form_to_write(), PrefixForm::Named);
    }
}
