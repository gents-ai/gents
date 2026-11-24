"""Timing measurements in schema 2, with the schema 1 transaction adapter.

Rows identify a workload and state its unit, direction, median and observed
range. Missing bounds remain missing; a point estimate is not a range.
"""

import math


FAMILIES = ("transaction", "scan_commit", "append", "read_rules", "txn_callbacks", "sync")


def throughput_rows(workload):
    """Read the legacy point schema and the actual harness field names."""
    result = []
    for point in workload.get("points", workload.get("by_threads", workload.get("threads", []))):
        if "ops_per_sec_median" in point:
            median, low, high = (point.get("ops_per_sec_" + suffix)
                                 for suffix in ("median", "min", "max"))
        elif "ops_per_s" in point:
            median, low, high = point["ops_per_s"], None, None
        else:
            median, low, high = point.get("ops"), point.get("min"), point.get("max")
        row = dict(threads=point["threads"], median=median, min=low, max=high,
                   unit="ops/s", higher_is_better=True)
        if point.get("stability") not in (None, "stable"):
            row["invalid_reason"] = "the harness marked this measurement unstable"
        result.append(row)
    return result


def throughput_config(workload):
    context = {key: workload[key] for key in ("quick", "reps", "rep_ms", "keys",
               "value_bytes", "sync", "storage_root") if key in workload}
    return dict(workload, context=context)


def families(doc):
    result = dict(doc.get("metrics", {}).get("timings", {}))
    if doc.get("schema_version") == 1 and "transaction" not in result:
        source = doc.get("metrics", {}).get("correctness", {})
        rows = []
        for cell in source.get("matrix", []):
            throughput = cell.get("throughput", {})
            for shape in ("uncontended", "contended"):
                stats = throughput.get(shape + "_commits_per_s")
                if not isinstance(stats, dict):
                    continue
                key = f'{cell.get("flavor")}/{cell.get("isolation")}/{shape}'
                context = {"threads": 1 if shape == "uncontended" else
                           throughput.get("contended_threads")}
                if shape == "contended":
                    context["hot_keys"] = throughput.get("hot_keys")
                row = dict(stats, key=key, name=key, unit="commits/s",
                           higher_is_better=True, context=context)
                attempts = throughput.get("contended_attempts", {})
                if (cell.get("correctness", {}).get("survival_pct_min", 100) < 100 or
                        shape == "contended" and any(attempts.get(key, 0) > 0 for key in
                        ("abandoned", "engine_errors", "other_errors"))):
                    row["invalid_reason"] = "the workload lost updates or did not complete successfully"
                rows.append(row)
        result["transaction"] = {
            "trust": source.get("trust", "absent"),
            "context": {key: source[key] for key in ("quick", "reps", "durability")
                        if key in source},
            "rows": rows,
            "measurement": source.get("measurement"),
        }
        timing_trust = source.get("timing_trust", "clean")
        if timing_trust != "clean":
            result["transaction"]["trust"] = ("absent" if source.get("trust") == "absent"
                                                else timing_trust)
    return result


def reason(before, after, base, cur):
    for label, doc, family in (("baseline", base, before), ("current", cur, after)):
        if family is None:
            return f"{label} did not collect this timing family"
        if family.get("trust") != "clean":
            return f'{label} timing family is {family.get("trust", "untrusted")}'
        measurement = family.get("measurement") or {}
        if measurement and measurement.get("guard_passed") is not True:
            return f"{label} benchmark runner was not certified quiet"
        if measurement and not measurement.get("toolchain"):
            return f"{label} benchmark compiler was not recorded"
        if not measurement and doc.get("run", {}).get("loadguard", {}).get("passed") is not True:
            return f"{label} host was not certified quiet"
        reps = family.get("context", {}).get("reps")
        if isinstance(reps, (int, float)) and reps < 3:
            return f"{label} needs at least three repeated measurements"
    old_meta, new_meta = before.get("measurement") or {}, after.get("measurement") or {}
    old_host = old_meta.get("host", base.get("run", {}).get("host", {}))
    new_host = new_meta.get("host", cur.get("run", {}).get("host", {}))
    for key in ("cpu", "cores", "store", "hostname", "allowed_cpus"):
        old, new = old_host.get(key), new_host.get(key)
        if key in ("cpu", "cores") and (old is None or new is None):
            return f"host {key} was not recorded"
        if key == "store" and any(isinstance(value, str) and value.startswith("unrecorded")
                                  for value in (old, new)):
            return "storage configuration was not recorded"
        if old != new:
            return f"host {key} differs between runs"
    if old_meta.get("toolchain", base.get("run", {}).get("toolchain")) != new_meta.get(
            "toolchain", cur.get("run", {}).get("toolchain")):
        return "benchmark compiler differs between runs"
    if before.get("context", {}) != after.get("context", {}):
        return "benchmark configuration differs between runs"
    return ""


def indexed(family):
    result = {}
    for row in (family or {}).get("rows", []):
        key = row["key"]
        if key in result:
            raise ValueError(f"duplicate timing row {key!r}")
        result[key] = row
    return result


def row_reason(before, after):
    if before is None or after is None:
        return "only one run measured this workload"
    for key in ("unit", "higher_is_better", "context"):
        if before.get(key) != after.get(key):
            return f"workload {key} differs between runs"
    if not isinstance(before.get("higher_is_better"), bool):
        return "metric direction is not specified"
    for row in (before, after):
        if row.get("invalid_reason"):
            return row["invalid_reason"]
        values = [row.get(key) for key in ("median", "min", "max")]
        if not all(isinstance(v, (int, float)) and not isinstance(v, bool)
                   and math.isfinite(v) and v >= 0 for v in values):
            return "finite measurements and an observed range are required"
        median, low, high = values
        if not low <= median <= high:
            return "median is outside the observed range"
    return ""
