#!/usr/bin/env python3
"""Compile the applied layout bottom-up and classify every error.

Usage (from the repository root, after apply.py):

    scripts/crate-layout/census.py --out <dir> [--only CRATE ...] [--no-run]

For each new crate, bottom to top, runs

    cargo check -p <crate> --profile fast-dev --message-format=json

(library target only; --tests adds the test targets of crates that compile) unless a crate it depends on failed, in which case the
crate is marked blocked and gets the static estimate from
report/upward_refs.json instead. Errors are classified as

  upward              reference to an item in a higher (or independent) crate
  private             item, field or method that needs widening
  missing_dependency  crate not declared in the new Cargo.toml
  unresolved_path     a path the rewriter got wrong
  other               everything else, by error code

Private-member errors are appended to visibility-feedback.json beside the
manifest; apply.py widens those on its next run from a clean checkout.
CARGO, CARGO_WRAPPER (for example a build-slot wrapper) and the usual cargo
environment are honoured.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import tomllib
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))

RESOLVE_CODES = {"E0432", "E0433", "E0412", "E0425", "E0405", "E0423", "E0531", "E0574", "E0422", "E0429", "E0434", "E0583"}
PRIVATE_CODES = {"E0603", "E0616", "E0624", "E0451", "E0364", "E0365", "E0446", "E0449"}


def load(path, default=None):
    if not os.path.exists(path):
        return default
    with open(path) as f:
        return json.load(f)


def crate_deps(repo, crate, names):
    with open(os.path.join(repo, "crates", crate, "Cargo.toml"), "rb") as f:
        t = tomllib.load(f)
    return [d for d in t.get("dependencies", {}) if d in names]


def run_check(repo, crate, raw_path, tests=False):
    cmd = shlex.split(os.environ.get("CARGO_WRAPPER", "")) + [os.environ.get("CARGO", "cargo"), "check", "-p", crate, "--tests" if tests else "--lib", "--profile", "fast-dev", "--message-format=json"]
    with open(raw_path, "w") as out, open(raw_path + ".stderr", "w") as err:
        p = subprocess.run(cmd, cwd=repo, stdout=out, stderr=err)
    return p.returncode


def primary(msg):
    for s in msg.get("spans", []):
        if s.get("is_primary"):
            return s
    return (msg.get("spans") or [None])[0]


def all_spans(msg):
    for s in msg.get("spans", []):
        yield s
    for c in msg.get("children", []):
        for s in c.get("spans", []):
            yield s


def strip_ty(name):
    name = re.sub(r"<.*$", "", name)
    return name.split("::")[-1].strip("&' ")


def classify(msg, up_index, known_libs, pkg, foreign=()):
    code = (msg.get("code") or {}).get("code")
    text = msg.get("message", "")
    sp = primary(msg)
    file = sp["file_name"] if sp else None
    line = sp["line_start"] if sp else None
    entry = {"code": code, "message": text, "file": file, "line": line}
    feedback = []
    # upward: the error sits on a line the tool left pointing at a higher crate
    if file is not None and code in RESOLVE_CODES | {"E0603", "E0282", "E0599", "E0308", "E0277"}:
        cands = []
        for u in up_index.get(file, ()):
            if u["line"] <= line <= u.get("end_line", u["line"]):
                cands.append(u)
        if cands and code in RESOLVE_CODES:
            span_text = "".join(t.get("text", "") for t in sp.get("text", []))
            best = None
            for u in cands:
                last = u["written"].split("::")[-1]
                if last in text or last in span_text:
                    best = u
                    break
            best = best or cands[0]
            entry.update({"class": "upward", "from_crate": best["from_crate"], "to_crate": best["to_crate"], "item": best["item"], "defined_in": best["defined_in"], "test": best["test"]})
            return entry, feedback
    if code in ("E0599", "E0116", "E0117", "E0210"):
        mm = re.search(r"no (?:method|function or associated item|associated item|variant or associated item) named `([^`]+)` found for [^`]*`([^`]+)`", text)
        for fi in foreign:
            what = "inherent impl of %s" % fi["type"] if fi.get("kind", "inherent") == "inherent" else "impl %s for %s" % (fi["trait"], fi["type"])
            lower = [fi["type_crate"]] if fi.get("kind", "inherent") == "inherent" else fi["lower_crates"]
            if code != "E0599" and fi["file"] == file and abs(fi["line"] - line) <= 2:
                entry.update({"class": "upward", "from_crate": lower[0], "to_crate": fi["impl_crate"], "item": what, "defined_in": "%s at %s" % (what, fi["old_file"]), "test": fi["test"], "via": "impl outside the crate that may hold it"})
                return entry, feedback
            if code == "E0599" and mm and mm.group(1) in fi["methods"] and pkg in lower and (strip_ty(mm.group(2)) == fi["type"] or fi.get("kind") == "trait"):
                entry.update({"class": "upward", "from_crate": pkg, "to_crate": fi["impl_crate"], "item": "%s::%s" % (fi["type"], mm.group(1)), "defined_in": "%s at %s" % (what, fi["old_file"]), "test": fi["test"], "via": "method of an impl that sits in a higher crate"})
                return entry, feedback
    tm = re.match(r"(?:type|trait) `([^`]+)` is private", text)
    if tm:
        entry["class"] = "private"
        feedback.append({"kind": "type", "name": strip_ty(tm.group(1)), "user": "%s:%s" % (file, line), "from_crate": pkg})
        return entry, feedback
    if code in PRIVATE_CODES:
        entry["class"] = "private"
        m = re.search(r"field `([^`]+)` of (?:struct|union|type|variant) `([^`]+)` is private", text)
        if m:
            feedback.append({"kind": "field", "owner": strip_ty(m.group(2)), "name": m.group(1), "user": "%s:%s" % (file, line), "from_crate": pkg})
        elif code in ("E0624", "E0603", "E0364", "E0365"):
            m = re.search(r"`([^`]+)`", text)
            name = strip_ty(m.group(1)) if m else None
            kind = "method" if code == "E0624" else "item"
            deffile = None
            for c in msg.get("children", []):
                for s in c.get("spans", []):
                    if "defined here" in c.get("message", "") or "defined here" in (s.get("label") or ""):
                        deffile = s["file_name"]
            for s in msg.get("spans", []):
                if not s.get("is_primary") and "defined here" in (s.get("label") or ""):
                    deffile = s["file_name"]
            if "tuple struct constructor" in text and name:
                feedback.append({"kind": "field", "owner": name, "name": "0", "user": "%s:%s" % (file, line), "from_crate": pkg})
            elif name and deffile and not os.path.isabs(deffile):
                feedback.append({"kind": kind, "file": deffile, "name": name, "user": "%s:%s" % (file, line), "from_crate": pkg})
        return entry, feedback
    if code in ("E0432", "E0433") or (code is None and "can't find crate" in text):
        m = re.search(r"unlinked crate `([^`]+)`|maybe a missing crate `([^`]+)`|can't find crate for `([^`]+)`", text + " ".join(c.get("message", "") for c in msg.get("children", [])))
        if m:
            name = m.group(1) or m.group(2) or m.group(3)
            if name in known_libs:
                entry.update({"class": "missing_dependency", "crate_missing": name})
                return entry, feedback
    if code in RESOLVE_CODES:
        entry["class"] = "unresolved_path"
        return entry, feedback
    entry["class"] = "other"
    return entry, feedback


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", required=True)
    ap.add_argument("--manifest", default=os.path.join(HERE, "manifest.json"))
    ap.add_argument("--report-dir", default=os.path.join(HERE, "report"))
    ap.add_argument("--only", nargs="*", help="check just these crates (deps are assumed to compile)")
    ap.add_argument("--no-run", action="store_true", help="reclassify the raw output already in --out")
    ap.add_argument("--tests", action="store_true", help="for each crate whose library compiles, also check its test targets")
    ap.add_argument("--stop-on-fail", action="store_true", help="stop at the first crate that fails for any reason")
    args = ap.parse_args()
    repo = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=HERE, check=True, capture_output=True, text=True).stdout.strip()
    manifest = load(args.manifest)
    order = [c["name"] for c in manifest["crates"] if not c.get("existing")]
    names = set(order)
    up = load(os.path.join(args.report_dir, "upward_refs.json"))
    stats = load(os.path.join(args.report_dir, "stats.json"))
    wid = load(os.path.join(args.report_dir, "widenings.json"))
    up_index = defaultdict(list)
    for u in up["refs"]:
        up_index[u["file"]].append(u)
    with open(os.path.join(repo, manifest["source_crate"], "Cargo.toml"), "rb") as f:
        src_cargo = tomllib.load(f)
    known_libs = {k.replace("-", "_") for k in src_cargo.get("dependencies", {})} | set(manifest.get("lib_names", {}).values()) | {n.replace("-", "_") for n in names}
    os.makedirs(args.out, exist_ok=True)
    fb_path = os.path.join(os.path.dirname(args.manifest), "visibility-feedback.json")
    feedback = load(fb_path, [])
    fb_keys = {json.dumps({k: v for k, v in f.items() if k not in ("user", "from_crate")}, sort_keys=True) for f in feedback}
    new_fb = 0
    status = {}
    census = {"crates": {}, "order": order}
    for crate in order:
        if args.only and crate not in args.only:
            continue
        raw = os.path.join(args.out, crate + ".raw.jsonl")
        deps = crate_deps(repo, crate, names)
        blocked_by = [d for d in deps if status.get(d) in ("failed", "blocked")]
        c = {"depends_on": deps, "stats": stats.get(crate, {})}
        census["crates"][crate] = c
        static = [u for u in up["refs"] if u["from_crate"] == crate]
        c["static_upward"] = {"production": sum(1 for u in static if not u["test"]), "test": sum(1 for u in static if u["test"])}
        c["widenings"] = wid["by_crate"].get(crate, 0)
        if blocked_by and not args.only:
            status[crate] = "blocked"
            c["status"] = "blocked"
            c["blocked_by"] = blocked_by
            print("%-18s blocked by %s; static estimate: %d production upward references" % (crate, ", ".join(blocked_by), c["static_upward"]["production"]), flush=True)
            continue
        if not args.no_run:
            print("%-18s cargo check ..." % crate, flush=True)
            run_check(repo, crate, raw)
        if not os.path.exists(raw):
            continue
        errors = []
        dep_errors = defaultdict(int)
        ok = None
        seen = set()
        with open(raw) as f:
            for ln in f:
                ln = ln.strip()
                if not ln.startswith("{"):
                    continue
                try:
                    j = json.loads(ln)
                except ValueError:
                    continue
                if j.get("reason") == "build-finished":
                    ok = j.get("success")
                if j.get("reason") != "compiler-message":
                    continue
                msg = j["message"]
                if msg.get("level") != "error":
                    continue
                tname = j.get("target", {}).get("name", "")
                if tname.replace("-", "_") != crate.replace("-", "_"):
                    dep_errors[tname] += 1
                    continue
                if msg.get("message", "").startswith("aborting due to") or "could not compile" in msg.get("message", ""):
                    continue
                key = msg.get("rendered")
                if key in seen:
                    continue
                seen.add(key)
                entry, fbs = classify(msg, up_index, known_libs, crate, up.get("inherent_impls_in_another_crate", ()))
                errors.append(entry)
                for fb in fbs:
                    k = json.dumps({kk: v for kk, v in fb.items() if kk not in ("user", "from_crate")}, sort_keys=True)
                    if k not in fb_keys:
                        fb_keys.add(k)
                        feedback.append(fb)
                        new_fb += 1
        by_class = defaultdict(int)
        for e in errors:
            by_class[e["class"]] += 1
        c["compiles"] = bool(ok) and not errors
        c["errors_by_class"] = dict(by_class)
        c["errors"] = errors
        c["dependency_errors"] = dict(dep_errors)
        other_codes = defaultdict(int)
        for e in errors:
            if e["class"] == "other":
                other_codes[e["code"] or "none"] += 1
        c["other_by_code"] = dict(other_codes)
        if c["compiles"]:
            status[crate] = "ok"
            c["status"] = "compiles"
            if args.tests:
                traw = os.path.join(args.out, crate + ".tests.raw.jsonl")
                if not args.no_run:
                    print("%-18s cargo check --tests ..." % crate, flush=True)
                    run_check(repo, crate, traw, tests=True)
                terrs = []
                tok = None
                tseen = set()
                if os.path.exists(traw):
                    for ln in open(traw):
                        if not ln.startswith("{"):
                            continue
                        try:
                            j = json.loads(ln)
                        except ValueError:
                            continue
                        if j.get("reason") == "build-finished":
                            tok = j.get("success")
                        if j.get("reason") == "compiler-message" and j["message"].get("level") == "error" and not j["message"].get("message", "").startswith("aborting due to"):
                            k = j["message"].get("rendered")
                            if k in tseen:
                                continue
                            tseen.add(k)
                            e, _ = classify(j["message"], up_index, known_libs, crate, up.get("inherent_impls_in_another_crate", ()))
                            terrs.append(e)
                tb = defaultdict(int)
                for e in terrs:
                    tb[e["class"]] += 1
                c["tests_compile"] = bool(tok) and not terrs
                c["test_errors_by_class"] = dict(tb)
                c["test_errors"] = terrs
                print("%-18s tests %s %s" % (crate, "compile" if c["tests_compile"] else "fail", dict(tb)), flush=True)
        else:
            status[crate] = "failed"
            c["status"] = "failed"
            if ok is None and not errors:
                c["status"] = "failed (no compiler output; see %s.stderr)" % raw
        print("%-18s %s %s" % (crate, c["status"], dict(by_class) if errors else ""), flush=True)
        if args.stop_on_fail and status[crate] == "failed":
            break
    with open(fb_path, "w") as f:
        json.dump(feedback, f, indent=1, sort_keys=True)
        f.write("\n")
    # upward groups from the compiler
    groups = defaultdict(list)
    for crate, c in census["crates"].items():
        for e in c.get("errors", []):
            if e["class"] == "upward":
                groups[(e["from_crate"], e["to_crate"], e["defined_in"])].append(e)
    census["compiler_upward_groups"] = [
        {"from_crate": k[0], "to_crate": k[1], "item": k[2], "count": len(v), "example": "%s:%s" % (v[0]["file"], v[0]["line"])} for k, v in sorted(groups.items(), key=lambda kv: -len(kv[1]))
    ]
    census["new_visibility_feedback"] = new_fb
    name = "census.json" if not args.only else "census-partial.json"
    with open(os.path.join(args.out, name), "w") as f:
        json.dump(census, f, indent=1, sort_keys=True)
        f.write("\n")
    print("new visibility feedback entries: %d" % new_fb)


if __name__ == "__main__":
    main()
