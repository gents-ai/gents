//! Property checks a caller runs in its own test suite against its own
//! implementations of regolith's traits.
//!
//! Enable the `testing` feature in a dev-dependency to get this module:
//!
//! ```toml
//! [dev-dependencies]
//! regolith = { version = "0.2", features = ["testing"] }
//! ```
//!
//! The module re-exports [`proptest`], so the strategies a check takes are
//! built with the version it was compiled against.
//!
//! # Merge operators
//!
//! [`check_merge_operator`] and [`check_touches`] are the two checks, one for
//! each law a [`MergeOperator`] carries.
//!
//! [`check_merge_operator`] checks the law that lets compaction fold operands
//! early. For any run of operands, cut into contiguous groups, each group
//! reduced to one operand by exact [`MergeOperator::partial_merge`] steps in
//! any bracketing, [`MergeOperator::full_merge`] over the reduced operands
//! gives what folding the operands onto the base one at a time gives.
//!
//! ```
//! use regolith::MergeOperator;
//! use regolith::testing::check_merge_operator;
//! use regolith::testing::proptest::prelude::*;
//!
//! /// Sums big-endian `u64` counters, wrapping on overflow.
//! struct SumCounter;
//!
//! fn counter(bytes: &[u8]) -> Option<u64> {
//!     Some(u64::from_be_bytes(bytes.try_into().ok()?))
//! }
//!
//! impl MergeOperator for SumCounter {
//!     fn name(&self) -> &'static str {
//!         "sum-counter"
//!     }
//!
//!     fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
//!         let mut total = base.map_or(Some(0), counter)?;
//!         for operand in operands {
//!             total = total.wrapping_add(counter(operand)?);
//!         }
//!         Some(total.to_be_bytes().to_vec())
//!     }
//!
//!     fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
//!         Some(counter(left)?.wrapping_add(counter(right)?).to_be_bytes().to_vec())
//!     }
//! }
//!
//! let counters = || any::<u64>().prop_map(|n| n.to_be_bytes().to_vec());
//! let checked = check_merge_operator(&SumCounter, any::<Vec<u8>>(), counters(), counters())?;
//! assert!(checked.folds > 0, "the operator's partial_merge was never exercised");
//! # Ok::<(), String>(())
//! ```
//!
//! # Projected reads
//!
//! [`check_touches`] checks the law that lets a commit trust
//! [`MergeOperator::touches`] after compaction has folded operands: an
//! operand folded from two touches a part exactly when one of the two does.
//! Without it a transaction that read some parts with
//! [`Transaction::get_parts`](crate::Transaction::get_parts) could commit
//! over a part a folded operand changed.
//!
//! ```
//! use regolith::MergeOperator;
//! use regolith::testing::check_touches;
//! use regolith::testing::proptest::collection::vec;
//! use regolith::testing::proptest::prelude::*;
//!
//! /// A value of two one-byte parts. An operand is `[part, delta]`: it adds
//! /// `delta` to one part, and touches that part only.
//! struct Fields;
//!
//! impl MergeOperator for Fields {
//!     fn name(&self) -> &'static str {
//!         "fields"
//!     }
//!
//!     fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
//!         let mut fields: [u8; 2] = base.map_or(Some([0; 2]), |base| base.try_into().ok())?;
//!         for &operand in operands {
//!             let &[part, delta] = operand else { return None };
//!             let field = fields.get_mut(usize::from(part))?;
//!             *field = field.wrapping_add(delta);
//!         }
//!         Some(fields.to_vec())
//!     }
//!
//!     fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
//!         let (&[part, a], &[other, b]) = (left, right) else { return None };
//!         (part == other).then(|| vec![part, a.wrapping_add(b)])
//!     }
//!
//!     fn touches(&self, _key: &[u8], operand: &[u8], parts: &[u32]) -> bool {
//!         operand.first().is_none_or(|part| parts.contains(&u32::from(*part)))
//!     }
//! }
//!
//! let operands = || (0u8..2, any::<u8>()).prop_map(|(part, delta)| vec![part, delta]);
//! let checked = check_touches(&Fields, any::<Vec<u8>>(), operands(), vec(0u32..2, 0..=2))?;
//! assert!(checked.folds > 0, "no two operands folded, so the law was never tested");
//! # Ok::<(), String>(())
//! ```

use std::cell::Cell;

pub use proptest;
use proptest::collection::vec;
use proptest::option;
use proptest::prelude::{Strategy, TestCaseError, any};
use proptest::test_runner::{Config, TestError, TestRunner};

use crate::MergeOperator;

/// The most part sets in one generated case.
const MAX_PART_SETS: usize = 6;

/// The most operands in one generated run.
const MAX_OPERANDS: usize = 12;

/// What a passing [`check_merge_operator`] covered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct MergeCheck {
    /// The generated runs that passed.
    pub cases: u32,
    /// The `partial_merge` calls that folded two operands into one. Zero
    /// means the operator folds nothing early, so only the check of
    /// `full_merge` against operand-at-a-time application ran.
    pub folds: u64,
}

/// Check `operator` against the early-folding law, over runs of operands drawn
/// from `operands` onto a base drawn from `bases` (or none), for keys drawn
/// from `keys`.
///
/// `operands` and `bases` must generate the byte strings the operator accepts
/// as an operand and as a stored value. A run the operator declines when its
/// operands are applied one at a time is rejected as a generation fault, and a
/// strategy that makes too many of them fails the check.
///
/// `keys` generates the keys the operator is handed, exactly as generated:
/// the database hands an operator the key its caller wrote, with nothing in
/// front of it. An operator that dispatches on its key is checked one value
/// shape at a time, with `keys` generating the keys of the shape `bases` and
/// `operands` belong to.
///
/// Failures name the shrunk run that broke the law. The `PROPTEST_CASES`
/// environment variable sets the number of runs, as for any property test.
pub fn check_merge_operator<O: MergeOperator + ?Sized>(
    operator: &O,
    keys: impl Strategy<Value = Vec<u8>>,
    bases: impl Strategy<Value = Vec<u8>>,
    operands: impl Strategy<Value = Vec<u8>>,
) -> Result<MergeCheck, String> {
    let input = (
        keys,
        option::of(bases),
        vec(operands, 1..=MAX_OPERANDS),
        vec(any::<u8>(), 0..=2 * MAX_OPERANDS),
    );
    // No regression file: a library harness must not write into its caller's tree.
    let mut runner = TestRunner::new(Config {
        failure_persistence: None,
        ..Config::default()
    });
    let (cases, folds) = (Cell::new(0u32), Cell::new(0u64));
    runner
        .run(&input, |(key, base, operands, choices)| {
            let mut run_folds = 0;
            let result = check_run(
                operator,
                &key,
                base.as_deref(),
                &operands,
                &choices,
                &mut run_folds,
            );
            if result.is_ok() {
                cases.set(cases.get() + 1);
                folds.set(folds.get() + run_folds);
            }
            result
        })
        .map_err(|err| match err {
            TestError::Fail(reason, input) => {
                format!("{reason}\nkey, base, operands, grouping choices: {input:?}")
            }
            TestError::Abort(reason) => format!("the check gave up: {reason}"),
        })?;
    Ok(MergeCheck {
        cases: cases.get(),
        folds: folds.get(),
    })
}

/// What a passing [`check_touches`] covered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TouchesCheck {
    /// The generated cases that passed.
    pub cases: u32,
    /// The cases whose pair of operands `partial_merge` folded into one,
    /// which are the ones that tested the law. Zero means the operator folds
    /// nothing early, so nothing was checked.
    pub folds: u64,
}

/// Check `operator`'s [`MergeOperator::touches`] against its
/// [`MergeOperator::partial_merge`]: for adjacent operands `left` and `right`
/// that fold to one, `touches` of the folded operand over any set of parts is
/// `touches(left) || touches(right)`.
///
/// `parts` generates the part sets a reader names (they are sorted and
/// deduplicated before use), and `operands` the operands the operator
/// accepts. `keys` generates the keys the operator is handed, as for
/// [`check_merge_operator`]. Failures name the shrunk case. The
/// `PROPTEST_CASES` environment variable sets the number of cases.
pub fn check_touches<O: MergeOperator + ?Sized>(
    operator: &O,
    keys: impl Strategy<Value = Vec<u8>>,
    operands: impl Strategy<Value = Vec<u8>> + Clone,
    parts: impl Strategy<Value = Vec<u32>>,
) -> Result<TouchesCheck, String> {
    let sets = parts.prop_map(|mut set| {
        set.sort_unstable();
        set.dedup();
        set
    });
    let input = (
        keys,
        operands.clone(),
        operands,
        vec(sets, 1..=MAX_PART_SETS),
    );
    // No regression file: a library harness must not write into its caller's tree.
    let mut runner = TestRunner::new(Config {
        failure_persistence: None,
        ..Config::default()
    });
    let (cases, folds) = (Cell::new(0u32), Cell::new(0u64));
    runner
        .run(&input, |(key, left, right, sets)| {
            cases.set(cases.get() + 1);
            let Some(folded) = operator.partial_merge(&key, &left, &right) else {
                return Ok(());
            };
            for set in &sets {
                let expected =
                    operator.touches(&key, &left, set) || operator.touches(&key, &right, set);
                let got = operator.touches(&key, &folded, set);
                if got != expected {
                    return Err(TestCaseError::fail(format!(
                        "touches of the folded operand over parts {set:?} is {got}, but the \
                         operands it folds touch {expected}"
                    )));
                }
            }
            folds.set(folds.get() + 1);
            Ok(())
        })
        .map_err(|err| match err {
            TestError::Fail(reason, input) => {
                format!("{reason}\nkey, left, right, part sets: {input:?}")
            }
            TestError::Abort(reason) => format!("the check gave up: {reason}"),
        })?;
    Ok(TouchesCheck {
        cases: cases.get(),
        folds: folds.get(),
    })
}

/// Check one run of `operands` onto `base`.
///
/// `choices` decides the grouping and the bracketing, so the same input always
/// makes the same cut, and shrinking it towards zeros shrinks towards one group
/// folded left to right, which is how compaction folds a run.
fn check_run<O: MergeOperator + ?Sized>(
    operator: &O,
    key: &[u8],
    base: Option<&[u8]>,
    operands: &[Vec<u8>],
    choices: &[u8],
    folds: &mut u64,
) -> Result<(), TestCaseError> {
    let mut expected = base.map(<[u8]>::to_vec);
    for operand in operands {
        expected = Some(
            operator
                .full_merge(key, expected.as_deref(), &[operand])
                .ok_or_else(|| TestCaseError::reject("full_merge declined a generated operand"))?,
        );
    }
    let expected = expected.unwrap_or_default();

    let all: Vec<&[u8]> = operands.iter().map(Vec::as_slice).collect();
    let together = operator.full_merge(key, base, &all);
    if together.as_deref() != Some(expected.as_slice()) {
        return Err(TestCaseError::fail(format!(
            "full_merge over all {} operands gave {together:?}, but applying them one at a \
             time gives {expected:?}",
            operands.len()
        )));
    }

    let (cuts, brackets) = choices.split_at(choices.len().min(operands.len() - 1));
    let mut brackets = brackets.iter().copied();
    let mut folded: Vec<Vec<u8>> = Vec::new();
    let mut start = 0;
    let cut_after = |end: usize| cuts.get(end - 1).is_some_and(|cut| cut % 2 == 1);
    for end in (1..=operands.len()).filter(|end| *end == operands.len() || cut_after(*end)) {
        match reduce(operator, key, &operands[start..end], &mut brackets, folds) {
            Some(operand) => folded.push(operand),
            // The operator declines to fold this group, which it may.
            None => return Ok(()),
        }
        start = end;
    }

    let folded_refs: Vec<&[u8]> = folded.iter().map(Vec::as_slice).collect();
    let grouped = operator.full_merge(key, base, &folded_refs);
    if grouped.as_deref() != Some(expected.as_slice()) {
        return Err(TestCaseError::fail(format!(
            "full_merge over {} operands folded early by partial_merge gave {grouped:?}, but \
             applying the {} operands one at a time gives {expected:?}",
            folded.len(),
            operands.len()
        )));
    }
    Ok(())
}

/// Fold `run` into one operand with exact `partial_merge` steps, splitting it
/// where `brackets` says: a missing choice splits off the last operand, which
/// is the left-to-right fold. `None` when the operator declines a step.
fn reduce<O: MergeOperator + ?Sized>(
    operator: &O,
    key: &[u8],
    run: &[Vec<u8>],
    brackets: &mut impl Iterator<Item = u8>,
    folds: &mut u64,
) -> Option<Vec<u8>> {
    if run.len() <= 1 {
        return run.first().cloned();
    }
    let split = run.len() - 1 - usize::from(brackets.next().unwrap_or(0)) % (run.len() - 1);
    let left = reduce(operator, key, &run[..split], brackets, folds)?;
    let right = reduce(operator, key, &run[split..], brackets, folds)?;
    let folded = operator.partial_merge(key, &left, &right)?;
    *folds += 1;
    Some(folded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counter(bytes: &[u8]) -> Option<u64> {
        Some(u64::from_be_bytes(bytes.try_into().ok()?))
    }

    fn counters() -> impl Strategy<Value = Vec<u8>> + Clone {
        any::<u64>().prop_map(|n| n.to_be_bytes().to_vec())
    }

    /// A sum whose `partial_merge` can be made wrong or absent.
    struct Sum {
        partial: fn(u64, u64) -> Option<u64>,
    }

    impl MergeOperator for Sum {
        fn name(&self) -> &'static str {
            "sum"
        }

        fn full_merge(
            &self,
            _key: &[u8],
            base: Option<&[u8]>,
            operands: &[&[u8]],
        ) -> Option<Vec<u8>> {
            let mut total = base.map_or(Some(0), counter)?;
            for operand in operands {
                total = total.wrapping_add(counter(operand)?);
            }
            Some(total.to_be_bytes().to_vec())
        }

        fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
            let folded = (self.partial)(counter(left)?, counter(right)?)?;
            Some(folded.to_be_bytes().to_vec())
        }
    }

    fn check(op: &Sum) -> Result<MergeCheck, String> {
        check_merge_operator(op, any::<Vec<u8>>(), counters(), counters())
    }

    #[test]
    fn an_exact_operator_passes_and_its_folds_are_counted() {
        let checked = check(&Sum {
            partial: |l, r| Some(l.wrapping_add(r)),
        })
        .unwrap();
        assert_eq!(checked.cases, Config::default().cases);
        assert!(checked.folds > 0);
    }

    #[test]
    fn an_operator_with_no_partial_merge_passes_with_no_folds() {
        let checked = check(&Sum {
            partial: |_, _| None,
        })
        .unwrap();
        assert_eq!(checked.folds, 0);
    }

    #[test]
    fn a_partial_merge_that_is_not_associative_fails_with_the_run() {
        let err = check(&Sum {
            partial: |l, r| Some(l.wrapping_sub(r)),
        })
        .unwrap_err();
        assert!(err.contains("folded early by partial_merge"), "{err}");
        assert!(err.contains("grouping choices"), "{err}");
    }

    #[test]
    fn a_partial_merge_that_drops_an_operand_fails() {
        let err = check(&Sum {
            partial: |l, _| Some(l),
        })
        .unwrap_err();
        assert!(err.contains("folded early by partial_merge"), "{err}");
    }

    /// A `full_merge` that stops after two operands, so it agrees with
    /// applying them one at a time only for runs of one or two.
    struct SumTwo;

    impl MergeOperator for SumTwo {
        fn name(&self) -> &'static str {
            "sum-two"
        }

        fn full_merge(
            &self,
            _key: &[u8],
            base: Option<&[u8]>,
            operands: &[&[u8]],
        ) -> Option<Vec<u8>> {
            let mut total = base.map_or(Some(0), counter)?;
            for operand in operands.iter().take(2) {
                total = total.wrapping_add(counter(operand)?);
            }
            Some(total.to_be_bytes().to_vec())
        }
    }

    #[test]
    fn a_full_merge_that_disagrees_with_one_at_a_time_application_fails() {
        let err =
            check_merge_operator(&SumTwo, any::<Vec<u8>>(), counters(), counters()).unwrap_err();
        assert!(err.contains("one at a time"), "{err}");
    }

    #[test]
    fn an_operator_that_declines_every_generated_run_fails_the_check() {
        let declines = Sum {
            partial: |_, _| None,
        };
        let err = check_merge_operator(
            &declines,
            any::<Vec<u8>>(),
            counters(),
            vec(any::<u8>(), 0..7),
        )
        .unwrap_err();
        assert!(err.contains("gave up"), "{err}");
    }

    /// Operands are `(part, delta)` pairs and a value holds three parts. An
    /// operand touches its own part.
    struct Parted {
        /// A zero delta touches nothing, which a fold of two nonzero deltas
        /// that wrap to zero then breaks.
        zero_delta_touches_nothing: bool,
        override_touches: bool,
    }

    fn operand(part: u8, delta: u8) -> Vec<u8> {
        vec![part, delta]
    }

    fn operands() -> impl Strategy<Value = Vec<u8>> + Clone {
        (0u8..3, proptest::sample::select(vec![0u8, 128]))
            .prop_map(|(part, delta)| operand(part, delta))
    }

    impl MergeOperator for Parted {
        fn name(&self) -> &'static str {
            "parted"
        }

        fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
            let mut value = base.map_or(vec![0; 3], <[u8]>::to_vec);
            for operand in operands {
                let part = usize::from(operand[0]);
                value[part] = value[part].wrapping_add(operand[1]);
            }
            Some(value)
        }

        // Folds two operands on one part into one; operands on two parts do not fold.
        fn partial_merge(&self, _: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
            (left[0] == right[0]).then(|| operand(left[0], left[1].wrapping_add(right[1])))
        }

        fn touches(&self, _: &[u8], operand: &[u8], parts: &[u32]) -> bool {
            if !self.override_touches {
                return true;
            }
            parts.contains(&u32::from(operand[0]))
                && (operand[1] != 0 || !self.zero_delta_touches_nothing)
        }
    }

    fn part_sets() -> impl Strategy<Value = Vec<u32>> {
        vec(0u32..3, 0..=3)
    }

    fn check_parted(op: &Parted) -> Result<TouchesCheck, String> {
        check_touches(op, any::<Vec<u8>>(), operands(), part_sets())
    }

    #[test]
    fn the_default_touches_passes_and_counts_its_folds() {
        let checked = check_parted(&Parted {
            zero_delta_touches_nothing: false,
            override_touches: false,
        })
        .unwrap();
        assert_eq!(checked.cases, Config::default().cases);
        assert!(checked.folds > 0);
    }

    #[test]
    fn a_touches_that_survives_folding_passes() {
        let checked = check_parted(&Parted {
            zero_delta_touches_nothing: false,
            override_touches: true,
        })
        .unwrap();
        assert!(checked.folds > 0);
    }

    #[test]
    fn a_touches_that_a_fold_can_change_fails_with_the_case() {
        // Two nonzero deltas can fold to zero, which touches nothing, so the
        // fold stops touching a part both operands touched.
        let err = check_parted(&Parted {
            zero_delta_touches_nothing: true,
            override_touches: true,
        })
        .unwrap_err();
        assert!(err.contains("folded operand over parts"), "{err}");
        assert!(err.contains("part sets"), "{err}");
    }

    #[test]
    fn an_operator_that_folds_nothing_passes_with_no_folds() {
        let checked = check_touches(
            &Sum {
                partial: |_, _| None,
            },
            any::<Vec<u8>>(),
            counters(),
            part_sets(),
        )
        .unwrap();
        assert_eq!(checked.folds, 0);
    }

    /// Dispatches on the first byte of the key, as an operator that serves
    /// several value shapes does: a key under `c` holds a counter, and any
    /// other key is not its own, so it declines. Records every key handed
    /// to it.
    #[derive(Default)]
    struct ByKey {
        seen: std::sync::Mutex<Vec<Vec<u8>>>,
    }

    impl ByKey {
        fn serves(&self, key: &[u8]) -> Option<()> {
            self.seen.lock().unwrap().push(key.to_vec());
            key.starts_with(b"c").then_some(())
        }
    }

    impl MergeOperator for ByKey {
        fn name(&self) -> &'static str {
            "by-key"
        }

        fn full_merge(
            &self,
            key: &[u8],
            base: Option<&[u8]>,
            operands: &[&[u8]],
        ) -> Option<Vec<u8>> {
            self.serves(key)?;
            let mut total = base.map_or(Some(0), counter)?;
            for operand in operands {
                total = total.wrapping_add(counter(operand)?);
            }
            Some(total.to_be_bytes().to_vec())
        }

        fn partial_merge(&self, key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
            self.serves(key)?;
            let folded = counter(left)?.wrapping_add(counter(right)?);
            Some(folded.to_be_bytes().to_vec())
        }

        fn touches(&self, key: &[u8], _: &[u8], _: &[u32]) -> bool {
            self.serves(key);
            true
        }
    }

    /// Keys of `shape`: the byte, then up to three more.
    fn keys_under(shape: u8) -> impl Strategy<Value = Vec<u8>> {
        vec(any::<u8>(), 0..4).prop_map(move |rest| [&[shape][..], &rest].concat())
    }

    /// The checks hand the operator the keys their strategy generates and
    /// nothing in front of them, as the database hands it the keys its
    /// callers wrote. An operator that dispatches on its key is therefore
    /// checked on the keys it meets in the database.
    #[test]
    fn the_checks_hand_the_operator_the_keys_the_strategy_generates() {
        let op = ByKey::default();
        let merged = check_merge_operator(&op, keys_under(b'c'), counters(), counters()).unwrap();
        assert!(merged.folds > 0);
        let touched = check_touches(&op, keys_under(b'c'), counters(), part_sets()).unwrap();
        assert!(touched.folds > 0);
        let seen = op.seen.lock().unwrap();
        assert!(!seen.is_empty());
        for key in seen.iter() {
            assert!(key.starts_with(b"c") && key.len() <= 4, "{key:?}");
        }
    }

    /// Keys the operator does not serve are a fault of the strategy: the
    /// check fails instead of passing on runs that tested nothing.
    #[test]
    fn keys_the_operator_does_not_serve_fail_the_check() {
        let op = ByKey::default();
        let err = check_merge_operator(&op, keys_under(b'x'), counters(), counters()).unwrap_err();
        assert!(err.contains("gave up"), "{err}");
    }
}
