//! A key's visible versions, newest first, through its first terminator: what
//! a merge-aware point read collects before it folds the operands.
//!
//! The first version is held in place, so a key whose newest visible version
//! is its value or its deletion, which is nearly every key, is read without
//! an allocation; only versions below a merge operand spill into a vector.

use super::KeyVersion;

#[derive(Default)]
pub(crate) struct MergeChain {
    first: Option<KeyVersion>,
    /// The versions after `first`, newest first. Empty while `first` is.
    rest: Vec<KeyVersion>,
}

impl MergeChain {
    #[inline]
    pub(crate) fn push(&mut self, version: KeyVersion) {
        if self.first.is_none() {
            self.first = Some(version);
        } else {
            self.rest.push(version);
        }
    }

    pub(crate) fn len(&self) -> usize {
        usize::from(self.first.is_some()) + self.rest.len()
    }

    /// Keeps the newest `len` versions.
    pub(crate) fn truncate(&mut self, len: usize) {
        match len.checked_sub(1) {
            None => {
                self.first = None;
                self.rest.clear();
            }
            Some(rest) => self.rest.truncate(rest),
        }
    }

    /// The oldest version.
    pub(crate) fn last(&self) -> Option<&KeyVersion> {
        self.rest.last().or(self.first.as_ref())
    }

    /// Takes the oldest version.
    pub(crate) fn pop(&mut self) -> Option<KeyVersion> {
        self.rest.pop().or_else(|| self.first.take())
    }

    /// The versions, newest first.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &KeyVersion> {
        self.first.iter().chain(&self.rest)
    }
}

/// What the key walks collect a key's versions into, one at a time: a
/// [`MergeChain`], or a plain vector.
pub(crate) trait VersionSink {
    fn push_version(&mut self, version: KeyVersion);
}

impl VersionSink for MergeChain {
    #[inline]
    fn push_version(&mut self, version: KeyVersion) {
        self.push(version);
    }
}

impl VersionSink for Vec<KeyVersion> {
    #[inline]
    fn push_version(&mut self, version: KeyVersion) {
        self.push(version);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slice::DbSlice;

    fn version(seq: u64) -> KeyVersion {
        (seq, 0, DbSlice::empty())
    }

    fn seqs(chain: &MergeChain) -> Vec<u64> {
        chain.iter().map(|version| version.0).collect()
    }

    /// The chain keeps a vector's order and length through every operation a
    /// read uses, whether its versions sit in place or spill.
    #[test]
    fn the_chain_reads_as_the_vector_of_its_versions() {
        let mut chain = MergeChain::default();
        assert_eq!((chain.len(), chain.last().map(|v| v.0)), (0, None));
        for seq in [9, 7, 5, 3] {
            chain.push(version(seq));
        }
        assert_eq!((seqs(&chain), chain.len()), (vec![9, 7, 5, 3], 4));
        assert_eq!(chain.last().map(|v| v.0), Some(3));
        chain.truncate(2);
        assert_eq!(seqs(&chain), vec![9, 7]);
        assert_eq!(chain.pop().map(|v| v.0), Some(7));
        assert_eq!(chain.last().map(|v| v.0), Some(9));
        assert_eq!(chain.pop().map(|v| v.0), Some(9));
        assert_eq!((chain.len(), chain.pop().map(|v| v.0)), (0, None));
        chain.push(version(4));
        chain.push(version(2));
        chain.truncate(0);
        assert_eq!(chain.len(), 0);
        chain.push(version(1));
        assert_eq!(seqs(&chain), vec![1]);
    }
}
