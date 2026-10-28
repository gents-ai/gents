use crate::tool::ToolEffects;

fn independent(earlier: &ToolEffects, later: &ToolEffects) -> bool {
    let (
        ToolEffects::Known {
            reads: earlier_reads,
            writes: earlier_writes,
        },
        ToolEffects::Known {
            reads: later_reads,
            writes: later_writes,
        },
    ) = (earlier, later)
    else {
        return false;
    };
    let intersects =
        |left: &[String], right: &[String]| left.iter().any(|resource| right.contains(resource));
    !intersects(earlier_writes, later_reads)
        && !intersects(earlier_writes, later_writes)
        && !intersects(earlier_reads, later_writes)
}

/// Return the length of the next contiguous batch without crossing an effect
/// or result-hook barrier. Resources must be supplied by their owning tools.
pub fn next_batch_len(effects: &[ToolEffects], result_hooks_cannot_terminate: bool) -> usize {
    let Some((_, remaining)) = effects.split_first() else {
        return 0;
    };
    let mut length = 1;
    for candidate in remaining {
        if !result_hooks_cannot_terminate
            || !effects[..length]
                .iter()
                .all(|earlier| independent(earlier, candidate))
        {
            break;
        }
        length += 1;
    }
    length
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_driven_batch_selection() {
        let raw = gents_lean_contract::run_prepared_generator(
            &gents_lean_contract::proofs_dir().unwrap(),
            "Proofs.Conformance.ToolFootprint:olean",
            "Proofs/Conformance/ToolFootprint.lean",
        )
        .unwrap();
        let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        assert_eq!(cases.len(), 250);
        for case in cases {
            let effects: Vec<_> = case["calls"]
                .as_array()
                .unwrap()
                .iter()
                .map(|sample| match sample.as_str().unwrap() {
                    "unknown" => ToolEffects::Unknown,
                    "read_a" => ToolEffects::read_only("a"),
                    "read_b" => ToolEffects::read_only("b"),
                    "write_a" => ToolEffects::Known {
                        reads: vec![],
                        writes: vec!["a".into()],
                    },
                    "write_b" => ToolEffects::Known {
                        reads: vec![],
                        writes: vec!["b".into()],
                    },
                    other => panic!("unknown modeled effect {other}"),
                })
                .collect();
            assert_eq!(
                next_batch_len(&effects, case["safeHooks"].as_bool().unwrap()),
                case["batchLength"].as_u64().unwrap() as usize,
                "{case}"
            );
        }
    }
}
