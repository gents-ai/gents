# Perf report

Renders one self-contained HTML page from a schema v1 or v2 run file, or from
two of them as a before/after comparison. CI publishes it as an artifact
keyed by the `main` commit it was built from.

```
python3 tools/perf-report/render_html.py \
    --current runs/<sha>.json [--baseline runs/<sha>.json] \
    --out perf-<sha>.html
```

The page is one file: no CDN, no external stylesheet, no fonts fetched
at view time beyond the one Google Fonts link, so an artifact downloaded
from CI months later still renders.

## What it will not do

A metric family the run file marks `absent` renders as an explicit gap,
never as a zero. A run recorded on a contaminated host renders with the
contamination stated on the page rather than dropped. Both rules exist
because a performance page is read as evidence, and a plausible-looking
zero is worse than a blank.

## Timing comparisons

Schema 2 adds `metrics.timings` for transaction, scan-commit, append and
allocation, read-rule, callback and synchronization measurements. Each family
preserves its source record and supplies rows with a workload key, unit,
direction, median and observed minimum/maximum. Workload configuration is
recorded separately so quick and full runs cannot be compared accidentally.
Schema 1 transaction ranges are read from the original correctness matrix.

```
python3 tools/perf-report/compare.py \
    --baseline runs/before.json --current runs/after.json \
    --fail-on-regression --require-complete
```

A regression exits 1. With `--require-complete`, missing or untrusted data
exits 2. A missing family, workload or observed range makes the overall
verdict unverified unless another measurement already proves a regression.
Point estimates remain visible in both reports; they cannot establish a
performance change. In particular, the existing read-rule, scan-commit,
callback and Criterion synchronization outputs still need repeated ranges
before those rows can produce a verdict. The collector applies the host
load check to timings separately from deterministic correctness results.
Known runs with fewer than three repetitions and workloads that lost updates,
abandoned operations or reported engine errors are unverified. A benchmark
that returned sooner because work failed is not a throughput improvement.

Criterion's `--baseline` flags do not save custom harness results. Preserve
the paired collected JSON run files for those comparisons.

The comparator also requires matching units, directions, workload settings
and recorded host configuration. A percentage must clear the threshold and
the observed ranges must separate. These checks do not establish that a
measurement protocol was sound: record commands, CPU affinity and every
sample with paired runs on an otherwise quiet host.
CI stamps each timing artifact with that benchmark runner's guard result,
CPU, hostname, allowed CPUs, filesystem, compiler and command before upload.
A quiet collector cannot clear a busy benchmark runner, and a busy collector
does not invalidate a measurement made on a certified runner. Timing trust
stays separate from the transaction correctness results.

Run the synthetic regression and report checks with `just perf-tools`.
The collector's executable checks also run in CI; locally, pass the JSON
build log from `cargo build --bench collect --message-format=json` to
`python3 tools/perf-report/test_collect.py --build-log <log> -v`.
