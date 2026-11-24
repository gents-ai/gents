//! The queue a transaction keeps its callbacks in.

/// Entries a [`Queue`] holds in place before it allocates.
const INLINE: usize = 4;

/// A first-in first-out queue whose first `INLINE` entries live in the struct,
/// so a transaction with a few callbacks allocates nothing for the queue
/// itself.
///
/// Entries keep their position for as long as they are queued: [`Queue::take`]
/// moves one out and advances the read position, and [`Queue::put_back`] and
/// [`Queue::reset`] undo that, so a callback that failed can run again where
/// it stood, with the ones behind it. The length doubles as a mark:
/// [`Queue::truncate`] to an earlier value takes back what was pushed since.
pub(super) struct Queue<T> {
    slots: [Option<T>; INLINE],
    spill: Vec<Option<T>>,
    pushed: usize,
    next: usize,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self {
            slots: [const { None }; INLINE],
            spill: Vec::new(),
            pushed: 0,
            next: 0,
        }
    }
}

impl<T> Queue<T> {
    fn slot(&mut self, at: usize) -> Option<&mut Option<T>> {
        match self.slots.get_mut(at) {
            Some(slot) => Some(slot),
            None => self.spill.get_mut(at - INLINE),
        }
    }

    pub(super) fn push(&mut self, entry: T) {
        match self.slots.get_mut(self.pushed) {
            Some(slot) => *slot = Some(entry),
            None => self.spill.push(Some(entry)),
        }
        self.pushed += 1;
    }

    /// Move the entry at the read position out and advance past it. Its
    /// position is the key to [`Queue::put_back`] and [`Queue::reset`].
    pub(super) fn take(&mut self) -> Option<(usize, T)> {
        let at = self.next;
        let entry = self.slot(at).and_then(Option::take)?;
        self.next += 1;
        Some((at, entry))
    }

    pub(super) fn pop(&mut self) -> Option<T> {
        self.take().map(|(_, entry)| entry)
    }

    /// Return the entry [`Queue::take`] moved out of position `at`.
    pub(super) fn put_back(&mut self, at: usize, entry: T) {
        if let Some(slot) = self.slot(at) {
            *slot = Some(entry);
        }
    }

    /// Move the read position back to `at`, so the entries from there on are
    /// taken again. Every entry from `at` up to the old position must have
    /// been put back.
    pub(super) fn reset(&mut self, at: usize) {
        self.next = self.next.min(at);
    }

    /// The mark to hand [`Queue::truncate`].
    pub(super) fn mark(&self) -> usize {
        self.pushed
    }

    /// Take back the entries pushed after `mark` was read, taken or not. The
    /// read position never stays past the end.
    pub(super) fn truncate(&mut self, mark: usize) {
        while self.pushed > mark {
            self.pushed -= 1;
            match self.slots.get_mut(self.pushed) {
                Some(slot) => *slot = None,
                None => drop(self.spill.pop()),
            }
        }
        self.next = self.next.min(self.pushed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn entries_come_out_in_the_order_they_went_in_across_the_inline_edge() {
        let mut queue = Queue::default();
        for i in 0..(INLINE * 3) {
            queue.push(i);
        }
        let out: Vec<usize> = std::iter::from_fn(|| queue.pop()).collect();
        assert_eq!(out, (0..INLINE * 3).collect::<Vec<_>>());
    }

    #[test]
    fn the_first_entries_do_not_touch_the_heap() {
        let mut queue = Queue::default();
        for i in 0..INLINE {
            queue.push(i);
        }
        assert_eq!(queue.spill.capacity(), 0);
        queue.push(INLINE);
        assert!(queue.spill.capacity() > 0);
    }

    #[test]
    fn truncate_takes_back_only_what_was_pushed_after_the_mark() {
        let mut queue = Queue::default();
        queue.push(1);
        let mark = queue.mark();
        for i in 2..=8 {
            queue.push(i);
        }
        queue.truncate(mark);
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), None);
        queue.push(9);
        assert_eq!(queue.pop(), Some(9));
    }

    #[test]
    fn truncate_below_the_read_position_pulls_the_position_back_with_it() {
        let mut queue = Queue::default();
        queue.push(1);
        let mark = queue.mark();
        queue.push(2);
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        queue.truncate(mark);
        assert_eq!(queue.pop(), None);
        queue.push(3);
        assert_eq!(queue.pop(), Some(3));
    }

    #[test]
    fn a_put_back_entry_is_taken_again_ahead_of_the_ones_behind_it() {
        for len in [3, INLINE + 3] {
            let mut queue = Queue::default();
            (0..len).for_each(|i| queue.push(i));
            let (at, first) = queue.take().unwrap();
            assert_eq!((at, first), (0, 0));
            queue.put_back(at, first);
            queue.reset(at);
            let out: Vec<usize> = std::iter::from_fn(|| queue.pop()).collect();
            assert_eq!(out, (0..len).collect::<Vec<_>>());
        }
    }

    #[test]
    fn entries_taken_after_a_position_are_taken_again_once_put_back_and_reset() {
        for len in [5, INLINE + 5] {
            let mut queue = Queue::default();
            (0..len).for_each(|i| queue.push(i));
            let (at, held) = queue.take().unwrap();
            let behind: Vec<_> = (0..3).map(|_| queue.take().unwrap()).collect();
            for (position, entry) in behind {
                queue.put_back(position, entry);
            }
            queue.put_back(at, held);
            queue.reset(at);
            let out: Vec<usize> = std::iter::from_fn(|| queue.pop()).collect();
            assert_eq!(out, (0..len).collect::<Vec<_>>(), "len {len}");
        }
    }

    #[derive(Debug, Clone)]
    enum Op {
        Push(u32),
        Pop,
        Truncate(usize),
        /// Take one and put it straight back, then read from there again.
        Retry,
    }

    proptest! {
        /// The queue agrees with a plain list and a read position, whatever
        /// the mix of pushes, pops, rewinds and retries.
        #[test]
        fn the_queue_matches_a_list_and_a_read_position(
            ops in prop::collection::vec(
                prop_oneof![
                    5 => any::<u32>().prop_map(Op::Push),
                    4 => Just(Op::Pop),
                    2 => (0usize..20).prop_map(Op::Truncate),
                    2 => Just(Op::Retry),
                ],
                0..120,
            )
        ) {
            let mut queue = Queue::default();
            let mut all: Vec<u32> = Vec::new();
            let mut next = 0;
            for op in ops {
                match op {
                    Op::Push(x) => {
                        queue.push(x);
                        all.push(x);
                    }
                    Op::Pop => {
                        let expected = all.get(next).copied();
                        next += usize::from(expected.is_some());
                        prop_assert_eq!(queue.pop(), expected);
                    }
                    Op::Truncate(mark) => {
                        queue.truncate(mark);
                        all.truncate(mark);
                        next = next.min(all.len());
                    }
                    Op::Retry => {
                        if let Some((at, entry)) = queue.take() {
                            prop_assert_eq!(Some(&entry), all.get(at));
                            queue.put_back(at, entry);
                            queue.reset(at);
                        }
                    }
                }
                prop_assert_eq!(queue.mark(), all.len());
            }
            let rest: Vec<u32> = std::iter::from_fn(|| queue.pop()).collect();
            prop_assert_eq!(rest, all[next..].to_vec());
        }
    }
}
