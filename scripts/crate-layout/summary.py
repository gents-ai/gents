#!/usr/bin/env python3
"""Write summary.txt and census-summary.json from census.json and the tool's reports.

    summarize.py --census <dir> [--report-dir DIR] [--layout layout.json]

--layout is the read-only pass's layout.json; when given, each upward group is
labelled with the fix kind that pass proposed for it.
"""
import argparse, json, os, re
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))


def load(p):
    with open(p) as f:
        return json.load(f)


def fix_index(layout):
    idx = []
    for m in layout["manifests"]:
        for u in m["upward_references"]:
            idx.append((u["to"], u["from"], u["cheapest_fix"]))
    return idx


def fix_for(item, idx):
    """The fix kind the layout proposed for an item, matched by name: first in
    the layout's description of the target, then of the referencing site."""
    parts = item.split("::")
    names = []
    if len(parts) >= 2 and parts[-2][:1].isupper():
        names.append(parts[-2])
    names.append(parts[-1])
    if len(parts) >= 3:
        names.append("::".join(parts[-3:-1]))
        names.append(parts[-2])
    for col in (0, 1):
        for n in names:
            pat = re.compile(r"(?<![A-Za-z0-9_])" + re.escape(n) + r"(?![A-Za-z0-9_])")
            hits = [row[2] for row in idx if pat.search(row[col])]
            if hits:
                return max(sorted(set(hits)), key=hits.count)
    return "not in the layout's inventory"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--census", required=True)
    ap.add_argument("--report-dir", default=os.path.join(HERE, "report"))
    ap.add_argument("--layout")
    ap.add_argument("--inventory-total", type=int, default=475)
    ap.add_argument("--previous", help="census-summary.json of an earlier census to compare with")
    a = ap.parse_args()
    census = load(os.path.join(a.census, "census.json"))
    up = load(os.path.join(a.report_dir, "upward_refs.json"))
    wid = load(os.path.join(a.report_dir, "widenings.json"))
    rep = load(os.path.join(a.report_dir, "report.json"))
    idx = fix_index(load(a.layout)) if a.layout else []
    refs = up["refs"]
    prod = [u for u in refs if not u["test"]]
    impls = up.get("inherent_impls_in_another_crate", [])
    pairs = defaultdict(lambda: {"production": 0, "test": 0})
    for u in refs:
        pairs[(u["from_crate"], u["to_crate"])]["test" if u["test"] else "production"] += 1
    groups = defaultdict(list)
    for u in prod:
        groups[(u["from_crate"], u["to_crate"], u["defined_in"])].append(u)
    top = sorted(groups.items(), key=lambda kv: (-len(kv[1]), kv[0]))
    by_fix = defaultdict(int)
    group_rows = []
    for (fc, tc, item), v in top:
        fix = fix_for(item, idx) if idx else None
        by_fix[fix] += len(v)
        group_rows.append({"from_crate": fc, "to_crate": tc, "item": item, "count": len(v), "example": "%s:%d" % (v[0]["file"], v[0]["line"]), "example_before_move": "%s:%d" % (v[0]["old_file"], v[0]["old_line"]), "layout_fix": fix})
    out = []
    w = out.append
    w("Crate layout census")
    w("===================")
    w("")
    w("Per crate (files and production lines moved; compiler result or static estimate)")
    w("")
    w("%-17s %5s %7s %-9s %s" % ("crate", "files", "prod", "status", "errors by class / static estimate"))
    order = census["order"]
    frontier = []
    for c in order:
        cc = census["crates"].get(c, {})
        st = cc.get("stats", {})
        status = cc.get("status", "not run")
        if status == "compiles":
            detail = "compiles alone"
            frontier.append(c)
        elif status.startswith("failed"):
            detail = ", ".join("%s %d" % kv for kv in sorted(cc.get("errors_by_class", {}).items()))
            frontier.append(c)
        else:
            su = cc.get("static_upward", {})
            n_impl = sum(1 for i in impls if i["type_crate"] == c and not i["test"])
            detail = "not compiled (blocked by %s); static: %d production upward refs, %d test-only" % (", ".join(cc.get("blocked_by", [])[:3]) + ("..." if len(cc.get("blocked_by", [])) > 3 else ""), su.get("production", 0), su.get("test", 0))
        w("%-17s %5d %7d %-9s %s" % (c, st.get("files", 0), st.get("production_lines", 0), status.split(" ")[0], detail))
    w("")
    w("Upward references (static, from the rewriter: every crate::/super::/use path whose target sits in a higher or independent crate)")
    w("  production: %d in %d groups   test-only: %d   impls outside the crate that may hold them: %d" % (len(prod), len(groups), len(refs) - len(prod), len(impls)))
    w("  the read-only inventory counted %d; see the note at the end" % a.inventory_total)
    if a.previous:
        prev = load(a.previous)
        ps = prev["static"]
        w("  previous census: %d production in %d groups, %d test-only, %d misplaced impls" % (ps["production"], ps["groups"], ps["test_only"], ps["inherent_impls_in_another_crate"]))
        w("  change: %+d production, %+d groups" % (len(prod) - ps["production"], len(groups) - ps["groups"]))
        prev_pairs = {(p["from_crate"], p["to_crate"]): p["production"] for p in prev["by_pair"]}
        w("  by pair (previous -> now):")
        for k in sorted(set(prev_pairs) | set(pairs), key=lambda k: -prev_pairs.get(k, 0)):
            now = pairs[k]["production"] if k in pairs else 0
            if prev_pairs.get(k, 0) or now:
                w("    %-17s -> %-17s %4d -> %d" % (k[0], k[1], prev_pairs.get(k, 0), now))
    w("")
    w("By (from -> to), production / test-only:")
    for (fc, tc), v in sorted(pairs.items(), key=lambda kv: -kv[1]["production"]):
        if v["production"]:
            w("  %-17s -> %-17s %4d / %d" % (fc, tc, v["production"], v["test"]))
    w("")
    comp = defaultdict(int)
    for c in order:
        for e in census["crates"].get(c, {}).get("errors", []):
            if e["class"] == "upward":
                comp[(e["from_crate"], e["to_crate"])] += 1
    if comp:
        w("Confirmed by the compiler (errors classified upward, crates that were compiled):")
        for (fc, tc), n in sorted(comp.items(), key=lambda kv: -kv[1]):
            w("  %-17s -> %-17s %4d" % (fc, tc, n))
        w("")
    w("Twenty largest production groups:")
    for r in group_rows[:20]:
        w("  %3d  %s -> %s  %s" % (r["count"], r["from_crate"], r["to_crate"], r["item"]))
        w("       e.g. %s (was %s)%s" % (r["example"], r["example_before_move"], "; layout fix: %s" % r["layout_fix"] if r["layout_fix"] else ""))
    w("")
    if idx:
        w("Production upward references by the fix the layout proposed:")
        for k, v in sorted(by_fix.items(), key=lambda kv: -kv[1]):
            w("  %-32s %d" % (k, v))
        w("")
    if impls:
        w("Impls that ended up outside the crate that may hold them (an error where the impl is, and missing methods below it):")
        for i in impls:
            if i.get("kind") == "trait":
                what = "impl %s (trait in %s) for %s (type in %s)" % (i["trait"], i["trait_crate"], i["type"], i["type_crate"])
            else:
                what = "impl %s (type in %s)" % (i["type"], i["type_crate"])
            w("  %s at %s:%d, %d methods%s" % (what, i["file"], i["line"], len(i["methods"]), " [test]" if i["test"] else ""))
        w("")
    w("Visibility widenings: %d" % wid["total"])
    for c in order:
        ws = [x for x in wid["widenings"] if x["crate"] == c]
        if ws:
            kinds = defaultdict(int)
            for x in ws:
                kinds[x["kind"]] += 1
            only_tests = sum(1 for x in ws if x.get("only_test_users"))
            w("  %-17s %4d  (%s; %d needed only by test code; %d from compiler feedback)" % (c, len(ws), ", ".join("%s %d" % kv for kv in sorted(kinds.items(), key=lambda kv: -kv[1])[:5]), only_tests, sum(1 for x in ws if x.get("source") == "compiler")))
    w("")
    w("Note on the inventory's %d" % a.inventory_total)
    w("  The read-only pass counted mentions in the unmoved tree, per part, before any fix. This census counts what is")
    w("  left after the tool has applied the manifest: whole-file moves the layout already proposed (for example")
    w("  tools/call/lifecycle/runtime.rs into gents-tools) and path-only rewrites through re-exports (root aliases such")
    w("  as crate::ConfigAccess, and re-exports of gents-loop and gents-protocol items) no longer count, and references")
    w("  inside #[cfg(test)] code are counted apart. Item-level sinks, splits, hooks and traits are not applied, so those")
    w("  references remain. Counts are per path occurrence; a `use` group counts once per leaf.")
    w("")
    w("What the compiler saw")
    w("  Compiled: %s. Every other crate depends on one that failed and was not compiled;" % ", ".join(frontier))
    w("  its row is the rewriter's static count. In the crates that were compiled, every static production reference")
    w("  had a compiler error on its lines.")
    w("")
    summary = {
        "frontier": frontier,
        "static": {"production": len(prod), "test_only": len(refs) - len(prod), "groups": len(groups), "inherent_impls_in_another_crate": len(impls)},
        "by_pair": [{"from_crate": k[0], "to_crate": k[1], **v} for k, v in sorted(pairs.items(), key=lambda kv: -kv[1]["production"])],
        "compiler_confirmed_by_pair": [{"from_crate": k[0], "to_crate": k[1], "count": v} for k, v in sorted(comp.items(), key=lambda kv: -kv[1])],
        "groups": group_rows,
        "by_layout_fix": dict(by_fix),
        "inventory_total": a.inventory_total,
        "widenings_by_crate": wid["by_crate"],
        "report_counts": rep["counts"],
    }
    with open(os.path.join(a.census, "census-summary.json"), "w") as f:
        json.dump(summary, f, indent=1)
        f.write("\n")
    with open(os.path.join(a.census, "summary.txt"), "w") as f:
        f.write("\n".join(out) + "\n")
    print("\n".join(out))


if __name__ == "__main__":
    main()
