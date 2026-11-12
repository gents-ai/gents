use super::canonical_execution::ExecutionFuture;
use crate::generated::*;

pub trait CanonicalCompactionAdapter {
    type Error: std::fmt::Display;

    fn observe<'a>(
        &'a mut self,
        input: &'a LeanCanonicalCompactionInput,
    ) -> ExecutionFuture<'a, Result<LeanCanonicalCompactionObservation, Self::Error>>;
}

pub async fn assert_canonical_compaction_cases<A>(
    cases: &[LeanCanonicalCompactionCase],
    adapter: &mut A,
) -> Result<(), String>
where
    A: CanonicalCompactionAdapter,
{
    for case in cases {
        let actual = adapter.observe(&case.input).await.map_err(|error| {
            format!("{}: native canonical compaction failed: {error}", case.name)
        })?;
        if actual != case.expected {
            return Err(format!(
                "{}: canonical compaction mismatch: expected {:?}, got {actual:?}",
                case.name, case.expected
            ));
        }
    }
    Ok(())
}

pub trait RepairedProjectionAdapter {
    type Error: std::fmt::Display;

    fn observe<'a>(
        &'a mut self,
        input: &'a LeanRepairedProjectionInput,
    ) -> ExecutionFuture<'a, Result<LeanRepairedProjectionObservation, Self::Error>>;
}

pub async fn assert_repaired_projection_cases<A>(
    cases: &[LeanRepairedProjectionCase],
    adapter: &mut A,
) -> Result<(), String>
where
    A: RepairedProjectionAdapter,
{
    for case in cases {
        let actual = adapter.observe(&case.input).await.map_err(|error| {
            format!("{}: native repaired projection failed: {error}", case.name)
        })?;
        if actual != case.expected {
            return Err(format!(
                "{}: repaired projection mismatch: expected {:?}, got {actual:?}",
                case.name, case.expected
            ));
        }
    }
    Ok(())
}
