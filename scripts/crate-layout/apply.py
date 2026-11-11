#!/usr/bin/env python3
"""Apply the crate layout in manifest.json to a clean checkout.

Usage (from the repository root, on a clean tree):

    scripts/crate-layout/apply.py             # move, rewrite, write reports
    scripts/crate-layout/apply.py --dry-run   # reports only, tree untouched
    scripts/crate-layout/apply.py --repo DIR --dry-run --report-dir OUT
                                              # try the manifest on another base

Every module reachable from crates/gents/src/lib.rs is assigned to one target
crate and module path by manifest.json. A rule names a module by its source
path and is inherited down the module tree until a deeper rule overrides it.
The tool then

  * creates the new crates (Cargo.toml, lib.rs, workspace membership),
  * moves files with `git mv` and deletes the re-export shims the manifest
    marks for deletion,
  * rewrites `crate::`, `self::`, `super::`, `gents::` paths, `use` groups,
    `pub(in ..)` scopes, `#[path]` attributes, relative `include*!` paths and
    paths inside serde/schemars attribute strings, in the moved files and in
    every workspace crate that depends on `gents`,
  * removes the root crate's re-exports of items that left it (no aliases),
  * widens visibility where an in-crate use became a cross-crate one,
  * leaves references that point at a HIGHER crate as written and lists them,
  * writes report/{mapping,report,widenings,upward_refs,stats}.json.

The real application, on a clean checkout of the commit to split, with this
directory committed on it:

    export RUSTC_WRAPPER="" RUST_MIN_STACK=16777216      # the usual cargo environment
    CARGO_WRAPPER=<build-slot wrapper, optional> \\
        scripts/crate-layout/apply.py --converge --fmt

--converge is the visibility-feedback loop. Fields, methods and types that
appear only in signatures cannot be seen by a text-level reference scan, so
each round applies the layout, runs census.py (`cargo check -p <crate> --lib`
bottom-up, with --tests also the test targets), lets it append the compiler's
private-member errors to visibility-feedback.json beside the manifest, resets
the checkout (`git reset --hard`, `git clean -fd crates apps`; hence the clean
tree) and applies again with those widened. It stops when a round finds no new
one. --fmt then runs `cargo fmt --all`, so the tree left staged is formatted
whatever the rewriter's line lengths were. Review report/census/census.json:
a crate that still fails lists its errors by class, and `upward` errors are
references the layout does not allow, to be fixed in the unsplit tree first.
visibility-feedback.json is left modified; commit it with the manifest so the
next run converges in one round. Entries that no longer match anything are
listed in report.json and can be deleted.

Before that, `--dry-run` must report nothing under
`unassigned_top_level_modules` and `skipped_manifest_entries`: a manifest path
that no longer exists is skipped with a warning, and a top-level module with
no rule stays in the root crate. Without --fmt the line numbers in the reports
are those of the files written, which is what census.py uses to recognise
upward references; with --fmt they are not, so classify from an unformatted
run.

This is a text-level rewriter: it reads Rust with regular expressions over a
comment- and string-masked copy of each file, not with a parser. report.json
lists what that cannot see. It refuses to run on a tree it has already been
applied to.
"""

from __future__ import annotations

import argparse
import bisect
import json
import os
import re
import shlex
import subprocess
import sys
import tomllib
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))

# --------------------------------------------------------------------------
# Lexing: a copy of the source with comments and literals blanked, so the
# patterns below only ever see code and offsets match the original text.

_TOK = re.compile(
    r'''//[^\n]*|/\*|\bb?r(#*)"|b?"(?:[^"\\]|\\.)*"|b?'(?:\\[^\n]{1,10}?|[^\\'\n])\'''',
    re.S,
)
_NONNL = re.compile(r"[^\n]")


def mask(src: str) -> str:
    out = []
    pos = 0
    n = len(src)
    while True:
        m = _TOK.search(src, pos)
        if not m:
            out.append(src[pos:])
            break
        s = m.start()
        out.append(src[pos:s])
        tok = m.group(0)
        if tok == "/*":
            depth = 1
            i = s + 2
            while depth and i < n:
                a = src.find("/*", i)
                b = src.find("*/", i)
                if b < 0:
                    i = n
                    break
                if 0 <= a < b:
                    depth += 1
                    i = a + 2
                else:
                    depth -= 1
                    i = b + 2
            e = i
        elif m.group(1) is not None:
            close = '"' + m.group(1)
            e = src.find(close, m.end())
            e = n if e < 0 else e + len(close)
        else:
            e = m.end()
        out.append(_NONNL.sub(" ", src[s:e]))
        pos = e
    return "".join(out)


WS = " \t\r\n"


def skip_ws(s: str, pos: int) -> int:
    n = len(s)
    while pos < n and s[pos] in WS:
        pos += 1
    return pos


# --------------------------------------------------------------------------
# Patterns

VIS = r"pub(?:\s*\([^)]*\))?"
MOD_RE = re.compile(r"(?<![\w$])(" + VIS + r"\s+)?mod\s+([A-Za-z_]\w*)\s*([;{])")
ITEM_RE = re.compile(
    r"(?<![\w$'])(" + VIS + r"\s+)?((?:(?:async|const|unsafe|default|extern)\s+)*)"
    r"(fn|struct|enum|union|trait|type|const|static|macro_rules!)\s+(?:mut\s+)?([A-Za-z_]\w*)"
)
USE_RE = re.compile(r"(?<![\w$])(" + VIS + r"\s+)?use\s+(?=[A-Za-z_:{*$])")
IDENT_RE = re.compile(r"(?:r#)?[A-Za-z_$][\w]*")
AS_RE = re.compile(r"\s+as\s+([A-Za-z_]\w*)")
PUBIN_RE = re.compile(
    r"(?<![\w$])pub\s*\(\s*in\s+((?:crate|self|super)(?:\s*::\s*[A-Za-z_]\w*)*)\s*\)"
)
PUBSUPER_RE = re.compile(r"(?<![\w$])pub\s*\(\s*super\s*\)")
INCLUDE_RE = re.compile(r"(?<![\w$])(include|include_str|include_bytes)!\s*\(")
TEST_ATTR_RE = re.compile(
    r"#\[\s*(?:cfg\s*\(\s*(?:test\s*\)|any\s*\(\s*test\s*,|all\s*\(\s*test\s*,)"
    r"|(?:tokio\s*::\s*)?test\b)"
)
CFG_TEST_TEXT_RE = re.compile(r"cfg\s*\(\s*(?:test\s*\)|any\s*\(\s*test\s*,|all\s*\(\s*test\s*,)")
PATH_ATTR_RE = re.compile(r'path\s*=\s*"([^"]*)"')
ATTR_PATH_STRING_RE = re.compile(
    r'\b(?:with|serialize_with|deserialize_with|schema_with|default|getter|try_from|from|into|remote|skip_serializing_if)\s*=\s*"((?:crate|self|super)(?:::[A-Za-z_]\w*)+)"'
)
IMPL_RE = re.compile(r"(?<![\w$])impl(?:\s*<[^{;]*?>)?\s+((?:[A-Za-z_]\w*\s*::\s*)*[A-Za-z_]\w*)\s*(?:<[^{;]*?>)?\s*(?:where\b[^{;]*)?\{")
IMPL_FOR_RE = re.compile(
    r"(?<![\w$])impl(?:\s*<[^{;]*?>)?\s+((?:[A-Za-z_]\w*\s*::\s*)*[A-Za-z_]\w*)\s*(<[^{;]*?>)?\s+for\s+&?\s*(?:dyn\s+)?((?:[A-Za-z_]\w*\s*::\s*)*[A-Za-z_]\w*)[^{;]*\{"
)
OLD_PATH_STRING_RE = re.compile(r'"(gents(?:::[a-z_][a-z0-9_]*)+)[":]')

VIS_RANK = {"": 0, "scoped": 1, "pub(crate)": 2, "pub": 3}


def norm_vis(text: str) -> str:
    t = re.sub(r"\s+", "", text or "")
    if t in ("", "pub", "pub(crate)", "pub(super)", "pub(self)"):
        return t
    if t.startswith("pub(in"):
        return "pub(in " + t[len("pub(in") : -1] + ")"
    return t


def vis_rank(v: str) -> int:
    return VIS_RANK.get(v, 1)


def crate_ident(name: str) -> str:
    return name.replace("-", "_")


# --------------------------------------------------------------------------
# Model


class Scope:
    __slots__ = ("file", "name", "start", "end", "parent", "module", "depth", "vis", "attrs", "decl_start", "kw_start", "vis_span")

    def __init__(self, file, name, start, end, parent, depth):
        self.file = file
        self.name = name
        self.start = start
        self.end = end
        self.parent = parent
        self.depth = depth
        self.module = None
        self.vis = ""
        self.attrs = []
        self.decl_start = 0
        self.kw_start = 0
        self.vis_span = None


class ModDecl:
    __slots__ = ("file", "scope", "name", "vis", "vis_span", "attrs", "start", "kw_start", "end", "path_attr", "child")

    def __init__(self):
        self.child = None
        self.path_attr = None


class Item:
    __slots__ = ("file", "module", "name", "kind", "vis", "vis_span", "offset", "test")


class Leaf:
    __slots__ = ("segs", "spans", "alias", "glob", "start", "end", "selfleaf", "stmt", "new_vis", "bind")

    def __init__(self, segs, spans, alias, glob, start, end):
        self.segs = segs
        self.spans = spans
        self.alias = alias
        self.glob = glob
        self.start = start
        self.end = end
        self.selfleaf = False
        self.stmt = None
        self.new_vis = None
        self.bind = None

    @property
    def vis(self):
        return self.stmt.vis


class GlobLeaf:
    """A name reached through `use path::*`."""

    __slots__ = ("leaf", "name")

    def __init__(self, leaf, name):
        self.leaf = leaf
        self.name = name


class UseStmt:
    __slots__ = ("file", "scope", "start", "kw_start", "end", "vis", "vis_span", "leaves", "stem_n", "module_level", "test", "tree_start")


class Module:
    def __init__(self, path, parent, name, file, is_file):
        self.path = path
        self.parent = parent
        self.name = name
        self.file = file
        self.is_file = is_file
        self.children = {}
        self.alts = []
        self.items = defaultdict(list)
        self.uses = {}
        self.globs = []
        self.decl = None  # ModDecl (file module) or Scope (inline module)
        self.test = False
        self.cfgs = []
        self.crate = None
        self.newpath = ()
        self.deleted = False
        self.rule = None
        self.seam = False
        self.new_vis = None

    def old_str(self):
        return "::".join(("crate",) + self.path)


class SrcFile:
    def __init__(self, rel, text, kind):
        self.rel = rel
        self.kind = kind  # 'tree' | 'consumer'
        self.text = text
        self.masked = mask(text)
        self.line_starts = [0] + [m.end() for m in re.finditer("\n", text)]
        self.module = None
        self.fragment = False
        self.includer = None
        self.path_loaded = False
        self.new_path_loaded = False
        self.new_rel = rel
        self.scopes = []
        self.decls = []
        self.items = []
        self.uses = []
        self.test_regions = []
        self.includes = []
        self.edits = []
        self.vis_edits = {}
        self.crate = None
        self.outside_src = False
        self.tails = []
        self._braces()

    def _braces(self):
        pos = []
        dep = []
        match = {}
        stack = []
        d = 0
        for m in re.finditer(r"[{}]", self.masked):
            o = m.start()
            if m.group() == "{":
                stack.append(o)
                d += 1
            else:
                if stack:
                    match[stack.pop()] = o
                d = max(0, d - 1)
            pos.append(o)
            dep.append(d)
        self.bpos = pos
        self.bdep = dep
        self.bmatch = match

    def depth(self, off):
        i = bisect.bisect_left(self.bpos, off)
        return self.bdep[i - 1] if i else 0

    def line(self, off):
        return bisect.bisect_right(self.line_starts, off)

    def scope_at(self, off):
        best = self.scopes[0]
        for sc in self.scopes[1:]:
            if sc.start < off < sc.end and sc.start > best.start:
                best = sc
        return best

    def in_test(self, off):
        if self.module is not None and self.module.test:
            return True
        for s, e in self.test_regions:
            if s <= off < e:
                return True
        return False

    def attrs_before(self, off):
        m = self.masked
        i = off
        attrs = []
        while True:
            j = i - 1
            while j >= 0 and m[j] in WS:
                j -= 1
            if j >= 0 and m[j] == "]":
                d = 0
                k = j
                while k >= 0:
                    if m[k] == "]":
                        d += 1
                    elif m[k] == "[":
                        d -= 1
                        if d == 0:
                            break
                    k -= 1
                if k > 0 and m[k - 1] == "#":
                    attrs.append((k - 1, j + 1))
                    i = k - 1
                    continue
            break
        attrs.reverse()
        return i, attrs

    def decl_block_start(self, start):
        """Extend a declaration start over its own indentation and the
        comment lines directly above it."""
        t = self.text
        ls = t.rfind("\n", 0, start) + 1
        if t[ls:start].strip():
            return start
        start = ls
        while start > 0:
            pls = t.rfind("\n", 0, start - 1) + 1
            line = t[pls : start - 1].strip()
            if line.startswith("//") and not line.startswith("//!"):
                start = pls
            else:
                break
        return start


class ParseError(Exception):
    pass


def parse_use_tree(s, pos, segs, spans, leaves, top):
    pos = skip_ws(s, pos)
    segs = list(segs)
    spans = list(spans)
    own_start = pos
    if s.startswith("::", pos):
        pos = skip_ws(s, pos + 2)
    while True:
        if pos >= len(s):
            raise ParseError("eof")
        c = s[pos]
        if c == "{":
            if top is not None and top[0] is None:
                top[0] = len(segs)
            pos += 1
            while True:
                pos = skip_ws(s, pos)
                if s[pos] == "}":
                    pos += 1
                    break
                pos = parse_use_tree(s, pos, segs, spans, leaves, None)
                pos = skip_ws(s, pos)
                if s[pos] == ",":
                    pos += 1
                elif s[pos] != "}":
                    raise ParseError("group")
            return pos
        if c == "*":
            leaves.append(Leaf(segs, spans, None, True, own_start, pos + 1))
            return pos + 1
        m = IDENT_RE.match(s, pos)
        if not m:
            raise ParseError("ident at %d" % pos)
        segs.append(m.group(0))
        spans.append((m.start(), m.end()))
        pos = m.end()
        p2 = skip_ws(s, pos)
        if s.startswith("::", p2):
            pos = skip_ws(s, p2 + 2)
            continue
        alias = None
        m = AS_RE.match(s, pos)
        if m:
            alias = m.group(1)
            pos = m.end()
        leaf = Leaf(segs, spans, alias, False, own_start, pos)
        if segs[-1] == "self" and len(segs) > 1:
            leaf.selfleaf = True
        leaves.append(leaf)
        return pos


# --------------------------------------------------------------------------


class Tool:
    def __init__(self, repo, manifest_path, report_dir, dry_run):
        self.repo = repo
        self.dry = dry_run
        self.report_dir = report_dir
        with open(manifest_path) as f:
            self.manifest = json.load(f)
        mf = self.manifest
        self.src_crate_dir = mf["source_crate"]
        self.SRC = self.src_crate_dir + "/src"
        self.root_crate = mf["root_crate"]
        self.order = [c["name"] for c in mf["crates"]]
        self.rank = {n: i for i, n in enumerate(self.order)}
        self.existing = {c["name"] for c in mf["crates"] if c.get("existing")}
        self.descriptions = {c["name"]: c.get("description", "") for c in mf["crates"]}
        self.sibling_sets = [set(s) for s in mf.get("independent", [])]
        self.rules = {}
        for r in mf["rules"]:
            self.rules[r["path"]] = r
        self.used_rules = set()
        self.lib_names = mf.get("lib_names", {})
        self.files = {}  # rel -> SrcFile (tree)
        self.consumer_files = []
        self.root = None
        self.warnings = []
        self.unhandled = []
        self.unresolved = []
        self.upward = []
        self.widenings = []
        self.removed_reexports = []
        self.deps = defaultdict(set)
        self.ext_deps = defaultdict(set)
        self.foreign_impls = []
        self.needs = []
        self.synth = {}  # (crate, newpath) -> list of decl lines
        self.existing_lib_appends = defaultdict(list)
        self.new_index = {}
        self._memo = {}
        self._ext_names = {}
        self.feedback = []
        fb = os.path.join(os.path.dirname(manifest_path), "visibility-feedback.json")
        if os.path.exists(fb):
            with open(fb) as f:
                self.feedback = json.load(f)
        self.feedback_applied = []
        self.feedback_missed = []
        self.assets = {}
        self.string_paths = []
        self.glob_split = []
        src_cargo = tomllib.loads(self.read(self.src_crate_dir + "/Cargo.toml"))
        roots = {"std", "core", "alloc"}
        for sec, val in src_cargo.items():
            if sec.endswith("dependencies"):
                roots |= {self.lib_names.get(k, k.replace("-", "_")) for k in val}
        for tv in src_cargo.get("target", {}).values():
            for sec, val in tv.items():
                roots |= {self.lib_names.get(k, k.replace("-", "_")) for k in val}
        self.extern_roots = roots

    # ---- io helpers

    def abspath(self, rel):
        return os.path.join(self.repo, rel)

    def read(self, rel):
        with open(self.abspath(rel), encoding="utf-8") as f:
            return f.read()

    def warn(self, msg):
        self.warnings.append(msg)
        print("warning: " + msg, file=sys.stderr)

    def git(self, *args):
        subprocess.run(["git", *args], cwd=self.repo, check=True, stdout=subprocess.DEVNULL)

    # ---- loading

    def load_tree(self):
        lib = self.SRC + "/lib.rs"
        root = Module((), None, "crate", None, True)
        self.root = root
        self.queue = []
        self._load_file(lib, root, False, None)
        while self.queue:
            fn = self.queue.pop(0)
            fn()

    def _load_file(self, rel, module, path_loaded, includer):
        if rel in self.files:
            self.warn("file loaded twice: %s" % rel)
            return None
        sf = SrcFile(rel, self.read(rel), "tree")
        sf.path_loaded = path_loaded
        sf.outside_src = not rel.startswith(self.SRC + "/")
        self.files[rel] = sf
        if includer is not None:
            sf.fragment = True
            sf.includer = includer
        else:
            module.file = sf
        sf.module = module
        fs = Scope(sf, None, -1, len(sf.text) + 1, None, 0)
        fs.module = module
        sf.scopes.append(fs)
        self._analyze(sf)
        return sf

    def _moddir(self, sf, new=False):
        rel = sf.new_rel if new else sf.rel
        d, b = os.path.split(rel)
        loaded = sf.new_path_loaded if new else sf.path_loaded
        if b in ("mod.rs", "lib.rs", "main.rs") or loaded:
            return d
        return os.path.join(d, b[:-3])

    def _scope_names(self, scope):
        names = []
        while scope is not None and scope.name is not None:
            names.append(scope.name)
            scope = scope.parent
        names.reverse()
        return names

    def _analyze(self, sf):
        masked = sf.masked
        # test regions
        for m in TEST_ATTR_RE.finditer(masked):
            # find the end of the attribute
            i = m.start() + 1
            d = 0
            while i < len(masked):
                if masked[i] == "[":
                    d += 1
                elif masked[i] == "]":
                    d -= 1
                    if d == 0:
                        break
                i += 1
            e = self._item_end(sf, i + 1)
            sf.test_regions.append((m.start(), e))
        # modules
        for m in MOD_RE.finditer(masked):
            kw = m.start()
            scope = sf.scope_at(kw)
            start, attrs = sf.attrs_before(kw)
            attr_texts = [sf.text[a:b] for a, b in attrs]
            name = m.group(2)
            vis = norm_vis(m.group(1) or "")
            vis_span = (m.start(1), m.start(1) + len(m.group(1).rstrip())) if m.group(1) else (kw, kw)
            if m.group(3) == "{":
                ob = m.end() - 1
                cb = sf.bmatch.get(ob, len(masked))
                sc = Scope(sf, name, ob, cb, scope, sf.depth(ob) + 1)
                sc.vis = vis
                sc.vis_span = vis_span
                sc.attrs = attr_texts
                sc.decl_start = start
                sc.kw_start = kw
                child = Module(scope.module.path + (name,), scope.module, name, sf, False)
                child.decl = sc
                sc.module = child
                self._attach(scope.module, child, attr_texts)
                sf.scopes.append(sc)
            else:
                d = ModDecl()
                d.file = sf
                d.scope = scope
                d.name = name
                d.vis = vis
                d.vis_span = vis_span
                d.attrs = attr_texts
                d.start = start
                d.kw_start = kw
                d.end = m.end()
                for a in attr_texts:
                    pm = PATH_ATTR_RE.search(a)
                    if pm and "cfg_attr" not in a:
                        d.path_attr = pm.group(1)
                sf.decls.append(d)
                self._resolve_decl(sf, d)
        # items
        for m in ITEM_RE.finditer(masked):
            off = m.start()
            scope = sf.scope_at(off)
            if sf.depth(off) != scope.depth:
                continue
            it = Item()
            it.file = sf
            it.module = scope.module
            it.name = m.group(4)
            it.kind = m.group(3)
            it.vis = norm_vis(m.group(1) or "")
            if m.group(1):
                it.vis_span = (m.start(1), m.start(1) + len(m.group(1).rstrip()))
            else:
                it.vis_span = (off, off)
            it.offset = off
            it.test = sf.in_test(off)
            sf.items.append(it)
            scope.module.items[it.name].append(it)
        # use statements
        for m in USE_RE.finditer(masked):
            kw = m.start()
            leaves = []
            top = [None]
            try:
                end = parse_use_tree(masked, m.end(), [], [], leaves, top)
                end = skip_ws(masked, end)
                if end >= len(masked) or masked[end] != ";":
                    raise ParseError("no semicolon")
            except (ParseError, IndexError) as e:
                self.unhandled.append({"file": sf.rel, "line": sf.line(kw), "construct": "use statement not parsed (%s)" % e})
                continue
            st = UseStmt()
            st.file = sf
            st.scope = sf.scope_at(kw)
            st.start, _ = sf.attrs_before(kw)
            st.kw_start = kw
            st.tree_start = m.end()
            st.end = end + 1
            st.vis = norm_vis(m.group(1) or "")
            st.vis_span = (m.start(1), m.end(1)) if m.group(1) else (kw, kw)
            st.leaves = leaves
            st.stem_n = top[0]
            st.module_level = sf.depth(kw) == st.scope.depth
            st.test = sf.in_test(kw)
            for lf in leaves:
                lf.stmt = st
            sf.uses.append(st)
            if st.module_level:
                mod = st.scope.module
                for lf in leaves:
                    if lf.glob:
                        mod.globs.append(lf)
                        continue
                    name = lf.alias or (lf.segs[-2] if lf.selfleaf else lf.segs[-1])
                    if name == "_":
                        continue
                    mod.uses.setdefault(name, lf)
        # includes
        for m in INCLUDE_RE.finditer(masked):
            am = re.compile(r'\s*"([^"\\]*)"\s*,?\s*\)').match(sf.text, m.end())
            if not am:
                self.unhandled.append({"file": sf.rel, "line": sf.line(m.start()), "construct": "%s! with a computed path" % m.group(1)})
                continue
            target = os.path.normpath(os.path.join(os.path.dirname(sf.rel), am.group(1)))
            sf.includes.append((am.start(1), am.end(1), target, m.group(1)))
            if m.group(1) == "include" and target.endswith(".rs") and os.path.exists(self.abspath(target)):
                scope = sf.scope_at(m.start())
                if target not in self.files:
                    self._load_file(target, scope.module, False, sf)

    def _item_end(self, sf, pos):
        m = sf.masked
        n = len(m)
        i = pos
        d = 0
        while i < n:
            c = m[i]
            if c in "([":
                d += 1
            elif c in ")]":
                d -= 1
            elif c == ";" and d <= 0:
                return i + 1
            elif c == "{":
                cb = sf.bmatch.get(i)
                if cb is None:
                    return n
                j = skip_ws(m, cb + 1)
                if j < n and m[j] == ";":
                    return j + 1
                if j < n and m[j] == ":" and m[j : j + 2] == "::":
                    i = j
                    continue
                return cb + 1
            elif c == "}":
                return i
            i += 1
        return n

    def _attach(self, parent, child, attr_texts):
        child.test = parent.test or any(CFG_TEST_TEXT_RE.search(a) for a in attr_texts)
        child.cfgs = parent.cfgs + [a for a in attr_texts if re.match(r"#\[\s*cfg\s*\(", a)]
        if child.name in parent.children:
            parent.alts.append(child)
        else:
            parent.children[child.name] = child

    def _resolve_decl(self, sf, d):
        names = self._scope_names(d.scope)
        if d.path_attr is not None:
            base = os.path.dirname(sf.rel) if not names else os.path.join(self._moddir(sf), *names)
            cands = [os.path.normpath(os.path.join(base, d.path_attr))]
        else:
            base = os.path.join(self._moddir(sf), *names)
            cands = [os.path.join(base, d.name + ".rs"), os.path.join(base, d.name, "mod.rs")]
        target = None
        for c in cands:
            if os.path.exists(self.abspath(c)):
                target = c
                break
        parent = d.scope.module
        if target is None:
            self.unhandled.append({"file": sf.rel, "line": sf.line(d.kw_start), "construct": "mod %s; has no file" % d.name})
            return
        child = Module(parent.path + (d.name,), parent, d.name, None, True)
        child.decl = d
        d.child = child
        self._attach(parent, child, d.attrs)
        path_loaded = d.path_attr is not None
        self.queue.append(lambda: self._load_file(target, child, path_loaded, None))

    # ---- assignment

    def rule_key(self, rel):
        if not rel.startswith(self.SRC + "/"):
            return None
        k = rel[len(self.SRC) + 1 : -3]
        if k.endswith("/mod"):
            k = k[:-4]
        return k

    def assign(self):
        self.unassigned_top = []

        def walk(m):
            rule = None
            if m.is_file and m.file is not None:
                key = self.rule_key(m.file.rel)
                rule = self.rules.get(key) if key else None
            if m is self.root:
                m.crate = self.root_crate
                m.newpath = ()
                self.used_rules.add("lib")
            elif rule is not None:
                self.used_rules.add(rule["path"])
                m.rule = rule
                if rule.get("action") == "delete":
                    m.deleted = True
                    m.crate = m.parent.crate
                    m.newpath = m.parent.newpath + (m.name,)
                else:
                    m.crate = rule["crate"]
                    m.newpath = tuple(x for x in rule.get("module", "").split("::") if x)
            else:
                m.crate = m.parent.crate
                m.newpath = m.parent.newpath + (m.name,)
                if m.parent is self.root and m.is_file and m.file is not None and not m.file.outside_src:
                    self.unassigned_top.append(m.file.rel)
            if m.is_file and m is not self.root and m.file is not None:
                p = m.parent
                m.seam = m.deleted or m.crate != p.crate or m.newpath != p.newpath + (m.name,)
            if m.file is not None and m.is_file:
                m.file.crate = m.crate
            for c in list(m.children.values()) + m.alts:
                walk(c)

        walk(self.root)
        for sf in self.files.values():
            if sf.fragment:
                sf.crate = sf.module.crate
        self.skipped_rules = sorted(set(self.rules) - self.used_rules)
        for k in self.skipped_rules:
            self.warn("manifest path no longer exists, skipped: %s" % k)

    def all_modules(self):
        out = []

        def walk(m):
            out.append(m)
            for c in list(m.children.values()) + m.alts:
                walk(c)

        walk(self.root)
        return out

    def place(self):
        claimed = {}

        def claim(sf):
            if sf.new_rel in claimed and claimed[sf.new_rel] is not sf:
                self.warn("destination collision: %s <- %s and %s" % (sf.new_rel, sf.rel, claimed[sf.new_rel].rel))
            claimed[sf.new_rel] = sf

        def walk(m):
            if m.is_file and m.file is not None:
                sf = m.file
                if m is self.root or sf.outside_src:
                    sf.new_rel = sf.rel
                    sf.new_path_loaded = sf.path_loaded
                elif m.deleted:
                    sf.new_rel = None
                elif m.seam:
                    base = "crates/%s/src" % m.crate
                    if not m.newpath:
                        sf.new_rel = base + "/lib.rs"
                    elif os.path.basename(sf.rel) == "mod.rs":
                        sf.new_rel = base + "/" + "/".join(m.newpath) + "/mod.rs"
                    else:
                        sf.new_rel = base + "/" + "/".join(m.newpath) + ".rs"
                    sf.new_path_loaded = False
                else:
                    d = m.decl
                    pf = d.file
                    names = self._scope_names(d.scope)
                    if d.path_attr is not None:
                        if names:
                            old_base = os.path.join(self._moddir(pf), *names)
                            new_base = os.path.join(self._moddir(pf, True), *names)
                        else:
                            old_base = os.path.dirname(pf.rel)
                            new_base = os.path.dirname(pf.new_rel)
                        rel = os.path.relpath(sf.rel, old_base)
                        sf.new_rel = os.path.normpath(os.path.join(new_base, rel))
                        sf.new_path_loaded = True
                    else:
                        base = os.path.join(self._moddir(pf, True), *names)
                        if os.path.basename(sf.rel) == "mod.rs":
                            sf.new_rel = os.path.join(base, m.name, "mod.rs")
                        else:
                            sf.new_rel = os.path.join(base, m.name + ".rs")
                        sf.new_path_loaded = False
                if sf.new_rel:
                    claim(sf)
            if not m.deleted:
                self.new_index.setdefault((m.crate, m.newpath), m)
            for c in list(m.children.values()) + m.alts:
                walk(c)

        walk(self.root)
        # fragments follow their includer
        for sf in self.files.values():
            if sf.fragment:
                inc = sf.includer
                while inc.fragment:
                    inc = inc.includer
                if sf.outside_src or inc.new_rel is None:
                    sf.new_rel = sf.rel
                else:
                    rel = os.path.relpath(sf.rel, os.path.dirname(inc.rel))
                    sf.new_rel = os.path.normpath(os.path.join(os.path.dirname(inc.new_rel), rel))
                claim(sf)
        # assets: non-Rust files and Rust files nobody declares
        tree = set(self.files)
        includers = {}
        for sf in self.files.values():
            for _, _, target, _ in sf.includes:
                includers.setdefault(target, sf)
        self.orphans = []
        moddirs = []
        for sf in self.files.values():
            if sf.fragment or sf.new_rel is None or sf.outside_src:
                continue
            moddirs.append((self._moddir(sf), self._moddir(sf, True)))
            moddirs.append((os.path.dirname(sf.rel), os.path.dirname(sf.new_rel)))
        moddirs.sort(key=lambda x: -len(x[0]))
        for dp, _, fns in os.walk(self.abspath(self.SRC)):
            for fn in fns:
                rel = os.path.relpath(os.path.join(dp, fn), self.repo)
                if rel in tree:
                    continue
                if fn.endswith(".rs"):
                    self.orphans.append(rel)
                inc = includers.get(rel)
                new = None
                if inc is not None and inc.new_rel:
                    new = os.path.normpath(os.path.join(os.path.dirname(inc.new_rel), os.path.relpath(rel, os.path.dirname(inc.rel))))
                else:
                    d = os.path.dirname(rel)
                    for old, newd in moddirs:
                        if d == old or d.startswith(old + "/"):
                            new = os.path.normpath(os.path.join(newd, os.path.relpath(rel, old)))
                            break
                if new is None:
                    new = rel
                self.assets[rel] = new

    # ---- resolution

    def locate(self, abs_):
        m = self.root
        i = 0
        n = len(abs_)
        while i < n and abs_[i] in m.children:
            m = m.children[abs_[i]]
            i += 1
        return m, abs_[i:]

    def absolutize(self, segs, module, kind, depth=0):
        if not segs:
            return None
        h = segs[0]
        if h in ("crate", "$crate"):
            return tuple(segs[1:]) if kind == "tree" else None
        if h == "gents":
            return tuple(segs[1:])
        if module is None:
            return None
        if h == "self":
            return module.path + tuple(segs[1:])
        if h == "super":
            k = 0
            while k < len(segs) and segs[k] == "super":
                k += 1
            if k > len(module.path):
                return None
            return module.path[: len(module.path) - k] + tuple(segs[k:])
        if h in module.children:
            return module.path + tuple(segs)
        if depth < 3 and h in module.uses:
            lf = module.uses[h]
            base = lf.segs[:-1] if lf.selfleaf else lf.segs
            if base and base[0] != h:
                return self.absolutize(list(base) + list(segs[1:]), module, kind, depth + 1)
        return None

    def has_name(self, m, name, seen):
        if id(m) in seen:
            return False
        seen.add(id(m))
        if name in m.items or name in m.uses or name in m.children:
            return True
        if m.deleted and any(g.segs[0] in self.extern_roots for g in m.globs):
            return True
        for g in m.globs:
            t = self.absolutize(g.segs, m, "tree")
            if t is None:
                continue
            gm, rest = self.locate(t)
            if not rest and self.has_name(gm, name, seen):
                return True
        return False

    def lookup(self, m, name):
        if name in m.items:
            if name in m.uses and all(it.kind == "macro_rules!" for it in m.items[name]):
                return ("use", m.uses[name])
            return ("item", m.items[name])
        if name in m.uses:
            return ("use", m.uses[name])
        ext = []
        for g in m.globs:
            t = self.absolutize(g.segs, m, "tree")
            if t is None:
                ext.append(g)
                continue
            gm, rest = self.locate(t)
            if not rest and self.has_name(gm, name, set()):
                return ("use", GlobLeaf(g, name))
        if m.deleted and len(ext) == 1:
            return ("use", GlobLeaf(ext[0], name))
        for g in ext:
            if self.ext_has_name(g.segs, name):
                return ("use", GlobLeaf(g, name))
        return None

    def ext_has_name(self, segs, name):
        """Whether a glob over a module of an existing workspace crate (for
        example `gents_loop::compaction::*`) brings in `name`."""
        key = (tuple(segs), name)
        if key in self._ext_names:
            return self._ext_names[key]
        found = False
        crate = segs[0].replace("_", "-")
        base = "crates/%s/src" % crate
        if crate in self.existing and os.path.isdir(self.abspath(base)):
            sub = "/".join(segs[1:])
            cands = [base + "/lib.rs"] if not sub else [base + "/" + sub + ".rs", base + "/" + sub + "/mod.rs"]
            defn = re.compile(r"\b(?:struct|enum|fn|type|const|static|trait|union|mod)\s+" + re.escape(name) + r"\b")
            for c in cands:
                if not os.path.exists(self.abspath(c)):
                    continue
                text = self.read(c)
                if defn.search(text) or re.search(r"pub use [^;]*\b" + re.escape(name) + r"\b[^;]*;", text):
                    found = True
                elif re.search(r"pub use [^;]*\*\s*;", text):
                    for dp, _, fns in os.walk(self.abspath(base)):
                        for fn in fns:
                            if fn.endswith(".rs") and defn.search(open(os.path.join(dp, fn), encoding="utf-8").read()):
                                found = True
                                break
                        if found:
                            break
        self._ext_names[key] = found
        return found

    def resolve(self, abs_, depth=0):
        key = abs_
        if key in self._memo:
            return self._memo[key]
        m, rest = self.locate(abs_)
        R = {"mod": m, "rest": rest, "entity": None, "sub": None, "ext": None, "abs": abs_}
        self._memo[key] = R
        if rest:
            ent = self.lookup(m, rest[0])
            R["entity"] = ent
            if ent and ent[0] == "use" and depth < 16:
                lf = ent[1]
                if isinstance(lf, GlobLeaf):
                    tsegs = list(lf.leaf.segs) + [lf.name]
                    owner = lf.leaf.stmt.scope.module
                else:
                    tsegs = list(lf.segs[:-1] if lf.selfleaf else lf.segs)
                    owner = lf.stmt.scope.module
                tabs = self.absolutize(tsegs, owner, "tree")
                if tabs is None:
                    if tsegs and tsegs[0] in self.extern_roots:
                        R["ext"] = tuple(tsegs) + tuple(rest[1:])
                else:
                    full = tabs + tuple(rest[1:])
                    if full != abs_:
                        R["sub"] = self.resolve(full, depth + 1)
        return R

    def final_def(self, R):
        seen = 0
        while R["sub"] is not None and seen < 20:
            R = R["sub"]
            seen += 1
        if R["ext"] is not None:
            return ("ext", "::".join(R["ext"]))
        return (R["mod"].crate, "::".join(("crate",) + R["abs"]))

    def where_defined(self, segs, mod):
        """(crate | 'ext', definition path) for a type or trait name as written
        in `mod`, or None when the name cannot be traced."""
        if len(segs) == 1:
            name = segs[0]
            if name in mod.items:
                return (mod.crate, mod.old_str() + "::" + name)
            if name not in mod.uses:
                return None
        abs_ = self.absolutize(segs, mod, "tree")
        if abs_ is None:
            head = segs[0]
            if len(segs) == 1:
                lf = mod.uses[head]
                head = lf.segs[0]
            return ("ext", "::".join(segs)) if head in self.extern_roots else None
        fd = self.final_def(self.resolve(abs_))
        return fd if fd[0] is not None else None

    def inside(self, fc, fpath, m):
        return fc == m.crate and fpath[: len(m.newpath)] == m.newpath

    def chain_pub(self, R, needs, site):
        n = 0
        while R is not None and n < 20:
            ent = R["entity"]
            if ent is None:
                return
            if ent[0] == "item":
                needs.append(("item", ent[1], "\0other", (), site))
                return
            needs.append(("use", ent[1], "\0other", (), site))
            R = R["sub"]
            n += 1

    def decide(self, R, fc, fpath, needs, site, depth=0, prefer_follow=False):
        m = R["mod"]
        ent = R["entity"]
        if R["rest"] and ent is not None and ent[0] == "use":
            lf = ent[1]
            vis = lf.leaf.stmt.vis if isinstance(lf, GlobLeaf) else lf.stmt.vis
            if R["sub"] is not None and depth < 12:
                sub_needs = []
                subd = self.decide(R["sub"], fc, fpath, sub_needs, site, depth + 1, prefer_follow)
                follow = prefer_follow or m.deleted or subd is None or subd[0] == "ext" or subd[1] != m.crate
                if subd is None:
                    follow = False
                if not follow and vis == "":
                    follow = not self.inside(fc, fpath, m)
                if follow:
                    needs.extend(sub_needs)
                    return subd
                if fc != m.crate:
                    needs.append(("use", lf, fc, fpath, site))
                    self.chain_pub(R["sub"], needs, site)
                elif vis not in ("pub", "pub(crate)", ""):
                    needs.append(("use", lf, fc, fpath, site))
            elif R["ext"] is None:
                # a re-export of a name declared beside it (a macro_rules! macro)
                if fc != m.crate and vis != "pub":
                    needs.append(("use", lf, fc, fpath, site))
            elif R["ext"] is not None:
                if prefer_follow or m.deleted or (fc != m.crate and vis != "pub") or (vis == "" and not self.inside(fc, fpath, m)):
                    return ("ext", None, R["ext"])
        elif R["rest"] and ent is not None and ent[0] == "item":
            needs.append(("item", ent[1], fc, fpath, site))
        if m.deleted:
            if not R["rest"] and not m.uses and len(m.globs) == 1 and m.globs[0].segs[0] in self.extern_roots:
                return ("ext", None, tuple(m.globs[0].segs))
            return None
        x = m
        while x is not None and x.parent is not None:
            needs.append(("mod", x, fc, fpath, site))
            x = x.parent
        return ("int", m.crate, m.newpath + tuple(R["rest"]))

    def allowed(self, fc, tc):
        if fc == tc:
            return True
        if fc not in self.rank:
            return True  # consumers sit above everything
        if tc not in self.rank:
            return False
        if self.rank[tc] >= self.rank[fc]:
            return False
        for s in self.sibling_sets:
            if fc in s and tc in s:
                return False
        return True

    # ---- reference rewriting

    def ctx(self, sf, off):
        if sf.kind == "consumer":
            return None, sf.crate, ()
        mod = sf.scope_at(off).module
        return mod, mod.crate, mod.newpath

    def rewrite_ref(self, sf, off, segs, is_use):
        """Return (new_segs or None, info). new_segs None means leave as written."""
        mod, fc, fpath = self.ctx(sf, off)
        abs_ = self.absolutize(segs, mod, sf.kind)
        if abs_ is None:
            return None, None
        R = self.resolve(abs_)
        site = (sf, off)
        needs = []
        d = self.decide(R, fc, fpath, needs, site)
        if d is None:
            self.unresolved.append({"file": sf.rel, "line": sf.line(off), "path": "::".join(segs), "why": "names a deleted shim with no matching re-export"})
            return None, None
        if d[0] == "ext":
            self.needs.extend(needs)
            self.ext_deps[fc].add(d[2][0])
            return list(d[2]), {"tc": None}
        if d[0] == "int" and not self.allowed(fc, d[1]):
            # the path as written goes through a higher crate; a re-export there
            # may only be forwarding something this crate is allowed to name
            needs2 = []
            d2 = self.decide(R, fc, fpath, needs2, site, 0, True)
            if d2 is not None and (d2[0] == "ext" or self.allowed(fc, d2[1])):
                d, needs = d2, needs2
                if d[0] == "ext":
                    self.needs.extend(needs)
                    self.ext_deps[fc].add(d[2][0])
                    return list(d[2]), {"tc": None}
        tc, newsegs = d[1], d[2]
        m, rest = R["mod"], R["rest"]
        if not self.allowed(fc, tc):
            fdef = self.final_def(R)
            self.upward.append(
                {
                    "from_crate": fc,
                    "to_crate": tc,
                    "item": "::".join(("crate",) + abs_),
                    "defined_in": fdef[1],
                    "new_path": crate_ident(tc) + "::" + "::".join(newsegs) if newsegs else crate_ident(tc),
                    "file": sf.new_rel,
                    "old_file": sf.rel,
                    "line": sf.line(off),
                    "test": sf.in_test(off),
                    "written": "::".join(segs),
                    "_pos": (sf, off, off),
                }
            )
            return None, {"tc": tc, "upward": True}
        self.needs.extend(needs)
        h = segs[0]
        if tc == fc:
            if h in ("self", "super") or (h not in ("crate", "$crate", "gents")):
                # relative path: keep it when it still means the same thing
                if h == "self":
                    exp = fpath + tuple(segs[1:])
                elif h == "super":
                    k = 0
                    while k < len(segs) and segs[k] == "super":
                        k += 1
                    exp = fpath[: len(fpath) - k] + tuple(segs[k:]) if k <= len(fpath) else None
                else:
                    exp = fpath + tuple(segs)
                if exp == newsegs:
                    return None, {"tc": tc}
            if sf.kind == "consumer" or sf.outside_src:
                head = "gents"
            elif h == "$crate":
                head = "$crate"
            else:
                head = "crate"
        else:
            head = crate_ident(tc)
            self.deps[fc].add(tc)
        out = [head] + list(newsegs)
        if out == list(segs):
            return None, {"tc": tc}
        return out, {"tc": tc}

    def process_file(self, sf):
        masked = sf.masked
        use_spans = [(st.kw_start, st.end) for st in sf.uses]
        use_starts = [s for s, _ in use_spans]
        skip_spans = []

        def in_use(off):
            i = bisect.bisect_right(use_starts, off) - 1
            return i >= 0 and use_spans[i][0] <= off < use_spans[i][1]

        is_tree = sf.kind == "tree"
        # pub(in path)
        if is_tree:
            for m in PUBIN_RE.finditer(masked):
                segs = re.split(r"\s*::\s*", m.group(1).strip())
                mod = sf.scope_at(m.start()).module
                abs_ = self.absolutize(segs, mod, "tree")
                skip_spans.append((m.start(), m.end()))
                ok = False
                if abs_ is not None:
                    sm, rest = self.locate(abs_)
                    if not rest and sm.crate == mod.crate and mod.newpath[: len(sm.newpath)] == sm.newpath and not sm.deleted:
                        ok = True
                        new = "pub(in crate::%s)" % "::".join(sm.newpath) if sm.newpath else "pub(crate)"
                        if new != re.sub(r"\s+", " ", m.group(0)):
                            sf.vis_edits[(m.start(), m.end())] = new
                if not ok:
                    sf.vis_edits[(m.start(), m.end())] = "pub(crate)"
                    self.widenings.append(
                        {"crate": mod.crate, "file": sf.new_rel, "line": sf.line(m.start()), "kind": "restricted-scope", "name": self._name_after(sf, m.end()), "from": norm_vis(m.group(0)), "to": "pub(crate)", "reason": "the named scope is no longer an ancestor in the same crate"}
                    )
            fm = sf.module
            if not sf.fragment and fm.seam and not fm.deleted and fm.parent is not None:
                old_parent = fm.parent
                to = "pub(crate)" if old_parent.crate == fm.crate else "pub"
                for m in PUBSUPER_RE.finditer(masked):
                    if sf.scope_at(m.start()) is not sf.scopes[0]:
                        continue
                    sf.vis_edits[(m.start(), m.end())] = to
                    self.widenings.append(
                        {"crate": fm.crate, "file": sf.new_rel, "line": sf.line(m.start()), "kind": "restricted-scope", "name": self._name_after(sf, m.end()), "from": "pub(super)", "to": to, "reason": "the module left its parent %s" % old_parent.old_str()}
                    )

        def skipped(off):
            for s, e in skip_spans:
                if s <= off < e:
                    return True
            return False

        # plain paths
        heads = ["crate", "self", "super", "gents"] if is_tree else ["gents"]
        seam_children = set()
        if is_tree:
            for sc in sf.scopes:
                for name, ch in sc.module.children.items():
                    if ch.is_file and ch.seam:
                        seam_children.add(name)
        alt = "|".join(heads + sorted(seam_children))
        pat = re.compile(r"(?<![\w:$.#'])(?:(\$crate)|(" + alt + r"))((?:\s*::\s*[A-Za-z_]\w*)+)")
        for m in pat.finditer(masked):
            off = m.start()
            if in_use(off) or skipped(off):
                continue
            head = m.group(1) or m.group(2)
            if head in seam_children and head not in heads:
                mod = sf.scope_at(off).module
                ch = mod.children.get(head)
                if ch is None or not ch.seam:
                    continue
            segs = [head] + re.findall(r"[A-Za-z_]\w*", m.group(3))
            new, _info = self.rewrite_ref(sf, off, segs, False)
            if new is not None:
                sf.edits.append((off, m.end(), "::".join(new)))
        # paths inside attribute strings (serde, schemars)
        if is_tree:
            for m in ATTR_PATH_STRING_RE.finditer(sf.text):
                if masked[m.start(1)] != " ":
                    continue
                segs = m.group(1).split("::")
                new, _info = self.rewrite_ref(sf, m.start(1), segs, False)
                if new is not None:
                    sf.edits.append((m.start(1), m.end(1), "::".join(new)))
            # inherent impls for a type that now lives in another crate
            for m in IMPL_RE.finditer(masked):
                tsegs = re.split(r"\s*::\s*", m.group(1))
                mod = sf.scope_at(m.start()).module
                if len(tsegs) == 1 and tsegs[0] in mod.items:
                    continue
                abs_ = self.absolutize(tsegs, mod, "tree")
                if abs_ is None:
                    continue
                R = self.resolve(abs_)
                fd = self.final_def(R)
                if fd[0] in ("ext", mod.crate) or fd[0] is None:
                    continue
                ob = m.end() - 1
                cb = sf.bmatch.get(ob, len(masked))
                methods = [x.group(1) for x in re.finditer(r"\bfn\s+([A-Za-z_]\w*)", masked[ob:cb])]
                self.foreign_impls.append({"kind": "inherent", "type": fd[1].split("::")[-1], "written_type": tsegs[-1], "type_defined_in": fd[1], "type_crate": fd[0], "impl_crate": mod.crate, "file": sf.new_rel, "old_file": sf.rel, "line": sf.line(m.start()), "test": sf.in_test(m.start()), "methods": methods, "_pos": (sf, m.start(), m.start())})
            for m in IMPL_FOR_RE.finditer(masked):
                mod = sf.scope_at(m.start()).module
                tr = self.where_defined(re.split(r"\s*::\s*", m.group(1)), mod)
                ty = self.where_defined(re.split(r"\s*::\s*", m.group(3)), mod)
                if tr is None or ty is None or mod.crate in (tr[0], ty[0]):
                    continue
                if tr[0] == "ext" and ty[0] == "ext":
                    continue
                local_arg = False
                for g in re.findall(r"[A-Za-z_]\w*", m.group(2) or ""):
                    d = self.where_defined([g], mod)
                    if d is not None and d[0] == mod.crate:
                        local_arg = True
                if local_arg:
                    continue
                ob = m.end() - 1
                cb = sf.bmatch.get(ob, len(masked))
                methods = [x.group(1) for x in re.finditer(r"\bfn\s+([A-Za-z_]\w*)", masked[ob:cb])]
                lower = [c for c in (tr[0], ty[0]) if c != "ext"]
                self.foreign_impls.append({"kind": "trait", "trait": tr[1].split("::")[-1], "trait_crate": tr[0], "type": ty[1].split("::")[-1], "type_crate": ty[0], "type_defined_in": ty[1], "impl_crate": mod.crate, "lower_crates": lower, "file": sf.new_rel, "old_file": sf.rel, "line": sf.line(m.start()), "test": sf.in_test(m.start()), "methods": methods, "_pos": (sf, m.start(), m.start())})
        # use statements
        for st in sf.uses:
            self.process_use(sf, st)
        # includes and string literals that name old module paths
        for s, e, target, kind in sf.includes:
            tsf = self.files.get(target)
            if tsf is not None:
                newt = tsf.new_rel
            else:
                newt = self.assets.get(target, target)
            if sf.new_rel is None or newt is None:
                continue
            newrel = os.path.relpath(newt, os.path.dirname(sf.new_rel))
            if newrel != sf.text[s:e]:
                sf.edits.append((s, e, newrel))
        if not is_tree:
            # #[path] and include*! in files outside the moved tree
            spots = [(m.start(1), m.end(1)) for m in re.finditer(r'#\[\s*path\s*=\s*"([^"]*)"', sf.text)]
            for m in INCLUDE_RE.finditer(masked):
                am = re.compile(r'\s*"([^"\\]*)"\s*,?\s*\)').match(sf.text, m.end())
                if am:
                    spots.append((am.start(1), am.end(1)))
            for s0, e0 in spots:
                target = os.path.normpath(os.path.join(os.path.dirname(sf.rel), sf.text[s0:e0]))
                tsf = self.files.get(target)
                newt = tsf.new_rel if tsf is not None else self.assets.get(target)
                if newt and newt != target:
                    sf.edits.append((s0, e0, os.path.relpath(newt, os.path.dirname(sf.rel))))
        for m in OLD_PATH_STRING_RE.finditer(sf.text):
            if sf.masked[m.start() + 1] == " ":
                abs_ = tuple(m.group(1).split("::")[1:])
                mod, rest = self.locate(abs_)
                if mod is not self.root and mod.crate != self.root_crate:
                    self.string_paths.append({"file": sf.new_rel or sf.rel, "line": sf.line(m.start()), "string": m.group(1), "now": crate_ident(mod.crate) + "".join("::" + s for s in mod.newpath + tuple(rest))})
        if "CARGO_MANIFEST_DIR" in sf.text and is_tree and sf.crate != self.root_crate:
            for m in re.finditer(r"CARGO_MANIFEST_DIR", sf.text):
                self.unhandled.append({"file": sf.new_rel or sf.rel, "line": sf.line(m.start()), "construct": "CARGO_MANIFEST_DIR in a file that changed crate"})

    def _name_after(self, sf, off):
        m = re.compile(r"\s*(?:(?:async|const|unsafe|static|fn|struct|enum|type|trait|mod|use|mut)\s+)*([A-Za-z_]\w*)").match(sf.masked, off)
        return m.group(1) if m else "?"

    def process_use(self, sf, st):
        is_root_file = sf.kind == "tree" and sf.module is self.root and not sf.fragment
        results = []
        changed = False
        for lf in st.leaves:
            segs = lf.segs[:-1] if lf.selfleaf else lf.segs
            bind = lf.alias or (segs[-1] if segs else None)
            new = None
            info = None
            first = segs[0] if segs else None
            if first is not None:
                rel_ok = sf.kind == "tree"
                if first in ("crate", "$crate", "self", "super", "gents") or (rel_ok and sf.scope_at(st.kw_start).module.children.get(first) is not None):
                    if not (sf.kind == "consumer" and first != "gents"):
                        n_up = len(self.upward)
                        new, info = self.rewrite_ref(sf, st.kw_start, list(segs), True)
                        for u in self.upward[n_up:]:
                            u["_pos"] = (sf, st.kw_start, st.end - 1)
            drop = False
            if is_root_file and st.vis != "" and info is not None and info.get("tc") not in (None, self.root_crate) and not info.get("upward") and not lf.glob:
                drop = True
                self.removed_reexports.append({"line": sf.line(st.kw_start), "name": bind, "was": "::".join(lf.segs), "now": "::".join(new) if new else "::".join(segs)})
            if new is not None or drop:
                changed = True
            lf.bind = bind
            results.append((lf, new, drop))
        self._use_results = getattr(self, "_use_results", {})
        self._use_results[id(st)] = (results, changed)

    def emit_use(self, sf, st):
        results, changed = self._use_results.get(id(st), ([], False))
        vis_changed = any(lf.new_vis is not None and lf.new_vis != st.vis for lf in st.leaves)
        if not changed and not vis_changed:
            return
        t = sf.text
        # uniform stem replacement
        if not vis_changed and not any(d for _, _, d in results):
            if len(results) == 1 and st.stem_n is None:
                lf, new, _ = results[0]
                if new is not None and not lf.glob and not lf.selfleaf:
                    txt = "::".join(new)
                    if lf.alias is None and new[-1] != lf.segs[-1]:
                        txt += " as " + lf.segs[-1]
                    sf.edits.append((lf.spans[0][0], lf.spans[-1][1], txt))
                    return
                if new is not None and lf.glob:
                    sf.edits.append((lf.spans[0][0], lf.spans[-1][1], "::".join(new)))
                    return
            elif st.stem_n:
                n = st.stem_n
                stem_new = None
                ok = True
                for lf, new, _ in results:
                    old = lf.segs[:-1] if lf.selfleaf else lf.segs
                    if new is None:
                        ok = False
                        break
                    tail = old[n:]
                    if tail and new[len(new) - len(tail) :] != tail:
                        ok = False
                        break
                    cand = new[: len(new) - len(tail)]
                    if not tail and not lf.glob and lf.alias is None and cand[-1] != old[-1]:
                        ok = False
                        break
                    if stem_new is None:
                        stem_new = cand
                    elif stem_new != cand:
                        ok = False
                        break
                if ok and stem_new:
                    lf0 = results[0][0]
                    sf.edits.append((lf0.spans[0][0], lf0.spans[n - 1][1], "::".join(stem_new)))
                    return
        # flat re-emit
        attrs = t[st.start : st.kw_start]
        ls = t.rfind("\n", 0, st.start) + 1
        indent = t[ls : st.start] if not t[ls : st.start].strip() else ""
        groups = {}
        order = []
        for lf, new, drop in results:
            if drop:
                continue
            old = lf.segs[:-1] if lf.selfleaf else lf.segs
            segs = list(new) if new is not None else list(old)
            vis = lf.new_vis if lf.new_vis is not None else st.vis
            if lf.glob:
                tail = segs[1:] + ["*"]
                txt = "::".join(tail)
            else:
                name = lf.alias
                if name is None and segs[-1] != old[-1]:
                    name = old[-1]
                if name is None and segs[-1] in ("crate", "self", "super"):
                    name = None
                txt = "::".join(segs[1:])
                if name is not None:
                    txt = (txt + " as " + name) if txt else ("\0as " + name)
                elif not txt:
                    txt = "\0"
            key = (vis, segs[0])
            if key not in groups:
                groups[key] = []
                order.append(key)
            groups[key].append(txt)
        stmts = []
        for vis, root in order:
            items = groups[(vis, root)]
            bare = [i for i in items if i.startswith("\0")]
            rest = [i for i in items if not i.startswith("\0")]
            v = vis + " " if vis else ""
            for b in bare:
                stmts.append("%s%suse %s%s;" % (attrs, v, root, (" " + b[1:]) if len(b) > 1 else ""))
            if len(rest) == 1:
                stmts.append("%s%suse %s::%s;" % (attrs, v, root, rest[0]))
            elif rest:
                stmts.append("%s%suse %s::{%s};" % (attrs, v, root, ", ".join(rest)))
        if not stmts:
            # remove the whole statement and its line
            s = sf.decl_block_start(st.start)
            e = st.end
            if t[e : e + 1] == "\n":
                e += 1
            sf.edits.append((s, e, ""))
            return
        sf.edits.append((st.start, st.end, ("\n" + indent).join(stmts)))

    # ---- visibility

    def scope_module(self, vis, owner):
        if vis in ("", "pub(self)"):
            return owner
        if vis == "pub(super)":
            return owner.parent
        if vis.startswith("pub(in "):
            segs = vis[len("pub(in ") : -1].split("::")
            abs_ = self.absolutize(segs, owner, "tree")
            if abs_ is None:
                return None
            m, rest = self.locate(abs_)
            return None if rest else m
        return None

    def required(self, vis, owner, fc, fpath):
        if fc != owner.crate:
            return None if vis == "pub" else "pub"
        if vis in ("pub", "pub(crate)"):
            return None
        sm = self.scope_module(vis, owner)
        if sm is not None and sm.crate == owner.crate and fpath[: len(sm.newpath)] == sm.newpath:
            return None
        return "pub(crate)"

    def settle_visibility(self):
        want = {}

        def bump(key, ent, level, site, fc):
            cur = want.get(key)
            is_test = site[0].kind == "tree" and site[0].in_test(site[1])
            is_test = is_test or (site[0].kind == "consumer" and "/tests/" in site[0].rel)
            if cur is None:
                want[key] = [ent, level, site, defaultdict(int), is_test]
                cur = want[key]
            elif vis_rank(level) > vis_rank(cur[1]) or (cur[4] and not is_test and vis_rank(level) == vis_rank(cur[1])):
                cur[1] = level
                cur[2] = site
            if not is_test:
                cur[4] = False
            who = fc if fc != "\0other" else "(re-export chain)"
            who = "gents (tests)" if who == "\0roottests" else who
            cur[3][who + (" [test code]" if is_test else "")] += 1

        for kind, ent, fc, fpath, site in self.needs:
            if kind == "item":
                for it in ent:
                    lvl = self.required(it.vis, it.module, fc, fpath)
                    if lvl:
                        bump(("item", id(it)), it, lvl, site, fc)
            elif kind == "use":
                lf = ent.leaf if isinstance(ent, GlobLeaf) else ent
                st = lf.stmt
                lvl = self.required(st.vis, st.scope.module, fc, fpath)
                if lvl:
                    bump(("use", id(lf)), lf, lvl, site, fc)
            elif kind == "mod":
                m = ent
                d = m.decl
                vis = d.vis
                owner = m.parent
                if m.is_file and m.seam:
                    # declared afresh in its new parent
                    if vis not in ("", "pub", "pub(crate)"):
                        vis = "pub(crate)"
                    if fc != m.crate:
                        lvl = "pub" if vis != "pub" else None
                    elif vis in ("pub", "pub(crate)"):
                        lvl = None
                    elif fpath[: len(m.newpath) - 1] == m.newpath[:-1]:
                        lvl = None
                    else:
                        lvl = "pub(crate)"
                else:
                    lvl = self.required(vis, owner, fc, fpath)
                if lvl:
                    bump(("mod", id(m)), m, lvl, site, fc)

        for key, (ent, level, site, counts, only_tests) in want.items():
            ssf, soff = site
            why = "%s:%d" % (ssf.new_rel or ssf.rel, ssf.line(soff))
            users = dict(counts)
            if key[0] == "item" and ent.kind == "macro_rules!":
                if level == "pub":
                    self.unhandled.append({"file": ent.file.new_rel, "line": ent.file.line(ent.offset), "construct": "macro_rules! %s is used from another crate (%s); it needs #[macro_export] and a root path" % (ent.name, why)})
                continue
            if key[0] == "use" and not ent.glob and any(it.kind == "macro_rules!" for it in ent.stmt.scope.module.items.get(ent.alias or ent.segs[-1], ())):
                if level == "pub":
                    # a by-path macro crosses crates only when exported; the
                    # `pub use` beside it keeps its module path
                    st = ent.stmt
                    for it in st.scope.module.items[ent.alias or ent.segs[-1]]:
                        ls = it.file.text.rfind("\n", 0, it.offset) + 1
                        indent = it.file.text[ls : it.offset]
                        if "macro_export" not in it.file.text[max(0, it.offset - 200) : it.offset]:
                            it.file.edits.append((it.offset, it.offset, "#[macro_export]\n" + (indent if not indent.strip() else "")))
                    ent.new_vis = "pub"
                    self.widenings.append({"crate": st.scope.module.crate, "file": st.file.new_rel, "line": st.file.line(st.kw_start), "kind": "macro", "name": ent.alias or ent.segs[-1], "from": st.vis or "private", "to": "#[macro_export] + pub use", "example_user": why, "users": users, "test_only_item": st.test, "only_test_users": only_tests})
                continue
            if key[0] == "item":
                it = ent
                sf = it.file
                cur = sf.vis_edits.get(it.vis_span)
                if cur is None or vis_rank(level) > vis_rank(cur):
                    sf.vis_edits[it.vis_span] = level
                self.widenings.append({"crate": it.module.crate, "file": sf.new_rel, "line": sf.line(it.offset), "kind": it.kind, "name": it.name, "from": it.vis or "private", "to": level, "example_user": why, "users": users, "test_only_item": it.test, "only_test_users": only_tests})
            elif key[0] == "use":
                lf = ent
                st = lf.stmt
                lf.new_vis = level
                self.widenings.append({"crate": st.scope.module.crate, "file": st.file.new_rel, "line": st.file.line(st.kw_start), "kind": "use", "name": "*" if lf.glob else (lf.alias or lf.segs[-1]), "from": st.vis or "private", "to": level, "example_user": why, "users": users, "test_only_item": st.test, "only_test_users": only_tests})
            else:
                m = ent
                d = m.decl
                m.new_vis = level
                sf = d.file
                if not (m.is_file and m.seam):
                    cur = sf.vis_edits.get(d.vis_span)
                    if cur is None or vis_rank(level) > vis_rank(cur):
                        sf.vis_edits[d.vis_span] = level
                self.widenings.append({"crate": m.crate, "file": (m.file.new_rel if m.is_file and m.file else sf.new_rel), "line": sf.line(d.kw_start), "kind": "mod", "name": m.name, "from": d.vis or "private", "to": level, "example_user": why, "users": users, "test_only_item": m.test, "only_test_users": only_tests})

    def apply_feedback(self):
        """Widen members the compiler reported as private (fields and methods
        cannot be seen by a text-level reference scan)."""
        by_new = {sf.new_rel: sf for sf in self.files.values() if sf.new_rel}
        structs = defaultdict(list)
        for sf in self.files.values():
            for it in sf.items:
                if it.kind in ("struct", "union", "enum"):
                    structs[it.name].append(it)
        for fb in self.feedback:
            kind = fb.get("kind")
            hit = 0
            if kind == "field":
                for it in structs.get(fb["owner"], []):
                    sf = it.file
                    mm = sf.masked
                    n = len(mm)
                    head = re.compile(r"(?:struct|union|enum)\s+" + re.escape(it.name) + r"\b").search(mm, it.offset)
                    i = head.end() if head else n
                    while i < n and mm[i] not in "{(;":
                        i += 1
                    if i >= n or mm[i] == ";":
                        continue
                    if mm[i] == "{":
                        end = sf.bmatch.get(i, n)
                        pat = re.compile(r"(?:(?<=[{,])|^)(\s*)((?:#\[[^\]]*\]\s*)*)(" + VIS + r"\s+)?" + re.escape(fb["name"]) + r"\s*:", re.M)
                        for m in pat.finditer(mm, i, end):
                            if sf.depth(m.end() - 1) != sf.depth(i) + 1:
                                continue
                            if m.group(3):
                                span = (m.start(3), m.start(3) + len(m.group(3).rstrip()))
                                old = norm_vis(m.group(3))
                            else:
                                p = m.end() - len(re.search(re.escape(fb["name"]) + r"\s*:$", m.group(0)).group(0))
                                span = (p, p)
                                old = ""
                            if old == "pub":
                                continue
                            sf.vis_edits[span] = "pub"
                            hit += 1
                            self.widenings.append({"crate": it.module.crate, "file": sf.new_rel, "line": sf.line(span[0]), "kind": "field", "name": "%s.%s" % (fb["owner"], fb["name"]), "from": old or "private", "to": "pub", "example_user": fb.get("user", "compiler feedback"), "users": {fb.get("from_crate", "?"): 1}, "test_only_item": False, "source": "compiler"})
                    else:
                        # tuple struct: widen every positional field
                        d = 0
                        j = i
                        while j < n:
                            if mm[j] == "(":
                                d += 1
                            elif mm[j] == ")":
                                d -= 1
                                if d == 0:
                                    break
                            j += 1
                        pat = re.compile(r"(?<=[(,])(\s*)(" + VIS + r"\s+)?(?=\S)")
                        for m in pat.finditer(mm, i, j):
                            if m.group(2):
                                span = (m.start(2), m.start(2) + len(m.group(2).rstrip()))
                                if norm_vis(m.group(2)) == "pub":
                                    continue
                            else:
                                span = (m.end(), m.end())
                            sf.vis_edits[span] = "pub"
                            hit += 1
                        if hit:
                            self.widenings.append({"crate": it.module.crate, "file": sf.new_rel, "line": sf.line(i), "kind": "field", "name": "%s.%s" % (fb["owner"], fb["name"]), "from": "restricted", "to": "pub", "example_user": fb.get("user", "compiler feedback"), "users": {fb.get("from_crate", "?"): 1}, "test_only_item": False, "source": "compiler"})
            elif kind == "type":
                for sf2 in self.files.values():
                    for it in sf2.items:
                        if it.name == fb["name"] and it.kind in ("struct", "enum", "union", "trait", "type") and it.vis != "pub" and not it.test:
                            sf2.vis_edits[it.vis_span] = "pub"
                            hit += 1
                            self.widenings.append({"crate": it.module.crate, "file": sf2.new_rel, "line": sf2.line(it.offset), "kind": it.kind, "name": it.name, "from": it.vis or "private", "to": "pub", "example_user": fb.get("user", "compiler feedback"), "users": {fb.get("from_crate", "?"): 1}, "test_only_item": False, "source": "compiler"})
            elif kind in ("method", "item"):
                sf = by_new.get(fb.get("file"))
                if sf is None:
                    self.feedback_missed.append(fb)
                    continue
                mm = sf.masked
                if kind == "method":
                    pat = re.compile(r"(?<![\w$])(" + VIS + r"\s+)?((?:(?:async|const|unsafe|default|extern)\s+)*)(?:fn|const|type)\s+" + re.escape(fb["name"]) + r"\b")
                else:
                    pat = re.compile(r"(?<![\w$])(" + VIS + r"\s+)?((?:(?:async|const|unsafe|default|extern)\s+)*)(?:fn|struct|enum|union|trait|type|const|static|mod)\s+(?:mut\s+)?" + re.escape(fb["name"]) + r"\b")
                for m in pat.finditer(mm):
                    if kind == "method" and sf.depth(m.start()) == sf.scope_at(m.start()).depth:
                        continue
                    if kind == "item" and sf.depth(m.start()) != sf.scope_at(m.start()).depth:
                        continue
                    if m.group(1):
                        span = (m.start(1), m.start(1) + len(m.group(1).rstrip()))
                        old = norm_vis(m.group(1))
                    else:
                        if kind == "method" and self._in_trait_impl(sf, m.start()):
                            continue
                        span = (m.start(), m.start())
                        old = ""
                    if old == "pub":
                        continue
                    sf.vis_edits[span] = "pub"
                    hit += 1
                    self.widenings.append({"crate": sf.crate, "file": sf.new_rel, "line": sf.line(m.start()), "kind": kind, "name": fb["name"], "from": old or "private", "to": "pub", "example_user": fb.get("user", "compiler feedback"), "users": {fb.get("from_crate", "?"): 1}, "test_only_item": sf.in_test(m.start()), "source": "compiler"})
                if kind == "item" and not hit:
                    # a re-export the reference scan did not connect
                    for st in sf.uses:
                        for lf in st.leaves:
                            nm = lf.alias or (lf.segs[-1] if lf.segs else None)
                            if nm == fb["name"] and st.vis != "pub" and st.module_level:
                                lf.new_vis = "pub"
                                hit += 1
                                self.widenings.append({"crate": sf.crate, "file": sf.new_rel, "line": sf.line(st.kw_start), "kind": "use", "name": fb["name"], "from": st.vis or "private", "to": "pub", "example_user": fb.get("user", "compiler feedback"), "users": {fb.get("from_crate", "?"): 1}, "test_only_item": st.test, "source": "compiler"})
            (self.feedback_applied if hit else self.feedback_missed).append(fb)

    def _in_trait_impl(self, sf, off):
        """True when `off` sits in `impl Trait for T { .. }` or a trait body."""
        mm = sf.masked
        target = sf.depth(off) - 1
        i = bisect.bisect_left(sf.bpos, off) - 1
        while i >= 0:
            p = sf.bpos[i]
            if mm[p] == "{" and sf.bdep[i] - 1 == target and sf.bmatch.get(p, 0) > off:
                head_start = max(mm.rfind(";", 0, p), mm.rfind("}", 0, p)) + 1
                head = mm[head_start:p]
                if re.search(r"\btrait\b", head):
                    return True
                if re.search(r"\bimpl\b", head) and re.search(r"\bfor\b", head):
                    return True
                return False
            i -= 1
        return False

    # ---- module declarations

    def plan_decls(self):
        for sf in list(self.files.values()):
            for d in sf.decls:
                m = d.child
                if m is None:
                    continue
                if m.seam:
                    s = sf.decl_block_start(d.start)
                    e = d.end
                    if sf.text[e : e + 1] == "\n":
                        e += 1
                    sf.edits.append((s, e, ""))
                    if not m.deleted:
                        self._declare(m)
                elif d.path_attr is not None and m.file is not None and sf.new_rel is not None:
                    names = self._scope_names(d.scope)
                    base = os.path.dirname(sf.new_rel) if not names else os.path.join(self._moddir(sf, True), *names)
                    newrel = os.path.relpath(m.file.new_rel, base)
                    if newrel != d.path_attr:
                        for a, b in sf.attrs_before(d.kw_start)[1]:
                            pm = PATH_ATTR_RE.search(sf.text, a, b)
                            if pm:
                                sf.edits.append((pm.start(1), pm.end(1), newrel))

    def _declare(self, m):
        d = m.decl
        vis = m.new_vis or (d.vis if d.vis in ("", "pub", "pub(crate)") else "pub(crate)")
        attrs = list(dict.fromkeys(m.cfgs + [a for a in d.attrs if not PATH_ATTR_RE.search(a) or "cfg_attr" in a]))
        if not m.newpath:
            return
        line = "".join(a + "\n" for a in attrs) + (vis + " " if vis else "") + "mod %s;" % m.newpath[-1]
        self._add_decl(m.crate, m.newpath[:-1], m.newpath[-1] if m.newpath else None, line)

    def _add_decl(self, crate, parent_path, name, line):
        if name is None:
            return
        parent = self.new_index.get((crate, parent_path))
        if parent is not None and not parent.deleted:
            sf = parent.file
            if parent.is_file:
                sf.tails.append(line)
            else:
                sc = parent.decl
                sf.edits.append((sc.end, sc.end, "    " + line.replace("\n", "\n    ") + "\n"))
            if name in parent.children and not parent.children[name].seam:
                self.warn("module name collision in %s: %s" % (crate, "::".join(parent_path + (name,))))
            return
        if crate in self.existing and not parent_path:
            self.existing_lib_appends[crate].append(line)
            return
        key = (crate, parent_path)
        if key not in self.synth:
            self.synth[key] = []
            if parent_path:
                self._add_decl(crate, parent_path[:-1], parent_path[-1], "pub mod %s;" % parent_path[-1])
        if line not in self.synth[key]:
            self.synth[key].append(line)

    # ---- output

    def render(self, sf):
        edits = list(sf.edits)
        t = sf.text
        for (s, e), level in sf.vis_edits.items():
            if s == e:
                edits.append((s, e, level + " "))
            else:
                edits.append((s, e, level))
        edits.sort(key=lambda x: (x[0], -(x[1] - x[0])))
        out = []
        pos = 0
        applied = []
        for s, e, txt in edits:
            if s < pos:
                # contained in a wider edit already applied
                continue
            out.append(t[pos:s])
            out.append(txt)
            applied.append((s, e, len(txt)))
            pos = e
        out.append(t[pos:])
        res = "".join(out)
        sf.applied = applied
        sf.applied_starts = [a[0] for a in applied]
        cum = []
        d = 0
        for s, e, n in applied:
            d += n - (e - s)
            cum.append(d)
        sf.applied_cum = cum
        if sf.tails:
            if not res.endswith("\n"):
                res += "\n"
            res += "\n".join(sf.tails) + "\n"
        return res

    def new_line(self, sf, off):
        """Line in the rewritten file of an offset in the original."""
        text = self.rendered.get(sf)
        if text is None or not hasattr(sf, "applied"):
            return sf.line(off)
        i = bisect.bisect_right(sf.applied_starts, off) - 1
        if i < 0:
            noff = off
        else:
            s, e, n = sf.applied[i]
            if off < e:
                noff = s + (sf.applied_cum[i] - (n - (e - s)))
            else:
                noff = off + sf.applied_cum[i]
        if not hasattr(sf, "new_line_starts"):
            sf.new_line_starts = [0] + [m.end() for m in re.finditer("\n", text)]
        return bisect.bisect_right(sf.new_line_starts, noff)

    def fix_lines(self):
        for u in self.upward:
            sf, a, b = u.pop("_pos")
            u["old_line"] = u["line"]
            u["line"] = self.new_line(sf, a)
            u["end_line"] = self.new_line(sf, b)
        for u in self.foreign_impls:
            sf, a, _b = u.pop("_pos")
            u["line"] = self.new_line(sf, a)
        for w in self.widenings:
            pos = w.pop("_pos", None)
            if pos:
                w["line"] = self.new_line(pos[0], pos[1])

    def run(self):
        if not self.dry:
            for c in self.order:
                if c not in self.existing and c != self.root_crate and os.path.exists(self.abspath("crates/%s" % c)):
                    sys.exit("crates/%s already exists: this tree already has the layout applied. Reset to the base first." % c)
        print("loading module tree", file=sys.stderr)
        self.load_tree()
        self.assign()
        self.place()
        self.load_consumers()
        print("rewriting %d tree files, %d consumer files" % (len(self.files), len(self.consumer_files)), file=sys.stderr)
        for sf in self.files.values():
            if sf.new_rel is None:
                continue
            self.process_file(sf)
        for sf in self.consumer_files:
            self.process_file(sf)
        self.settle_visibility()
        self.apply_feedback()
        for sf in list(self.files.values()) + self.consumer_files:
            if sf.new_rel is None:
                continue
            for st in sf.uses:
                self.emit_use(sf, st)
        self.plan_decls()
        self.rendered = {}
        for sf in list(self.files.values()) + self.consumer_files:
            if sf.new_rel is None:
                continue
            new = self.render(sf)
            if new != sf.text or sf.new_rel != sf.rel:
                self.rendered[sf] = new
        self.fix_lines()
        self.cargo_plan()
        self.write_reports()
        if not self.dry:
            self.write_tree()

    # ---- consumers

    def load_consumers(self):
        ws = tomllib.loads(self.read("Cargo.toml"))
        members = ws["workspace"]["members"]
        self.consumer_crates = {}
        for mdir in members:
            if mdir == self.src_crate_dir:
                continue
            cpath = os.path.join(mdir, "Cargo.toml")
            if not os.path.exists(self.abspath(cpath)):
                continue
            ct = tomllib.loads(self.read(cpath))
            name = ct["package"]["name"]
            if name in self.rank:
                continue
            has = False
            for sec in ("dependencies", "dev-dependencies", "build-dependencies"):
                if "gents" in ct.get(sec, {}):
                    has = True
            if not has:
                continue
            self.consumer_crates[name] = mdir
            self._scan_consumer_dir(mdir, name)
        # the root crate's own tests, examples and benches
        for sub in ("tests", "examples", "benches"):
            d = os.path.join(self.src_crate_dir, sub)
            if os.path.isdir(self.abspath(d)):
                self._scan_consumer_dir(d, "\0roottests")
        # any other workspace file that loads a moved source file by path
        have = {sf.rel for sf in self.consumer_files}
        for mdir in members:
            for dp, dns, fns in os.walk(self.abspath(mdir)):
                dns[:] = [d for d in dns if d not in ("target", "node_modules", ".git", "gen")]
                for fn in fns:
                    if not fn.endswith(".rs"):
                        continue
                    rel = os.path.relpath(os.path.join(dp, fn), self.repo)
                    if rel in self.files or rel in have:
                        continue
                    text = self.read(rel)
                    if "#[path" not in text and "include" not in text:
                        continue
                    if "/src/" not in text and "gents/" not in text:
                        continue
                    sf = SrcFile(rel, text, "consumer")
                    sf.crate = "\0other"
                    sf.scopes.append(Scope(sf, None, -1, len(text) + 1, None, 0))
                    self.consumer_files.append(sf)

    def _scan_consumer_dir(self, mdir, name):
        for dp, dns, fns in os.walk(self.abspath(mdir)):
            dns[:] = [d for d in dns if d not in ("target", "node_modules", ".git", "gen")]
            for fn in fns:
                if not fn.endswith(".rs"):
                    continue
                rel = os.path.relpath(os.path.join(dp, fn), self.repo)
                if rel in self.files:
                    continue
                text = self.read(rel)
                if "gents::" not in text and "gents ::" not in text and "use gents" not in text:
                    continue
                sf = SrcFile(rel, text, "consumer")
                sf.crate = name
                fs = Scope(sf, None, -1, len(text) + 1, None, 0)
                sf.scopes.append(fs)
                self._analyze_consumer(sf)
                self.consumer_files.append(sf)

    def _analyze_consumer(self, sf):
        masked = sf.masked
        for m in USE_RE.finditer(masked):
            kw = m.start()
            leaves = []
            top = [None]
            try:
                end = parse_use_tree(masked, m.end(), [], [], leaves, top)
                end = skip_ws(masked, end)
                if end >= len(masked) or masked[end] != ";":
                    raise ParseError("no semicolon")
            except (ParseError, IndexError):
                continue
            if not any(lf.segs and lf.segs[0] == "gents" for lf in leaves):
                continue
            st = UseStmt()
            st.file = sf
            st.scope = sf.scopes[0]
            st.start, _ = sf.attrs_before(kw)
            st.kw_start = kw
            st.tree_start = m.end()
            st.end = end + 1
            st.vis = norm_vis(m.group(1) or "")
            st.vis_span = (m.start(1), m.end(1)) if m.group(1) else (kw, kw)
            st.leaves = leaves
            st.stem_n = top[0]
            st.module_level = False
            st.test = False
            for lf in leaves:
                lf.stmt = st
            sf.uses.append(st)

    # ---- cargo

    def cargo_sections(self, text):
        """[(header, [(key, raw_entry_text)])] for a Cargo.toml, comments dropped."""
        secs = []
        cur = None
        lines = text.split("\n")
        i = 0
        while i < len(lines):
            ln = lines[i]
            hm = re.match(r"^\[(.+)\]\s*$", ln)
            if hm:
                cur = (hm.group(1), [])
                secs.append(cur)
                i += 1
                continue
            em = re.match(r"^([A-Za-z0-9_-]+)(\.[A-Za-z0-9_.-]+)?\s*=", ln)
            if em and cur is not None:
                buf = [ln]
                bal = ln.count("[") + ln.count("{") - ln.count("]") - ln.count("}")
                while bal > 0 and i + 1 < len(lines):
                    i += 1
                    buf.append(lines[i])
                    bal += lines[i].count("[") + lines[i].count("{") - lines[i].count("]") - lines[i].count("}")
                cur[1].append((em.group(1), "\n".join(buf)))
            i += 1
        return secs

    def lib_name(self, key, raw):
        if key in self.lib_names:
            return self.lib_names[key]
        return key.replace("-", "_")

    def uses_lib(self, lib, texts):
        pat = re.compile(r"(?<![\w.])" + re.escape(lib) + r"\s*::|\buse\s+" + re.escape(lib) + r"\s*(?:;|as\b)|extern\s+crate\s+" + re.escape(lib) + r"\b")
        return any(pat.search(t) for t in texts)

    def cargo_plan(self):
        src_cargo_rel = self.src_crate_dir + "/Cargo.toml"
        src_text = self.read(src_cargo_rel)
        secs = self.cargo_sections(src_text)
        parsed = tomllib.loads(src_text)
        features = parsed.get("features", {})
        new_crates = [c for c in self.order if c not in self.existing and c != self.root_crate]
        texts = defaultdict(list)
        for sf in self.files.values():
            if sf.new_rel is None or sf.outside_src:
                continue
            texts[sf.crate].append(self.rendered.get(sf, sf.text))
        self.cargo_files = {}
        self.crate_external = {}
        dep_secs = [(h, e) for h, e in secs if h.endswith("dependencies")]
        optional = {k for k, v in parsed.get("dependencies", {}).items() if isinstance(v, dict) and v.get("optional")}
        for c in new_crates:
            out = []
            out.append("[package]")
            out.append('name = "%s"' % c)
            out.append("version.workspace = true")
            out.append("edition.workspace = true")
            out.append("license.workspace = true")
            out.append("repository.workspace = true")
            out.append('description = "%s"' % self.descriptions.get(c, ""))
            out.append("")
            lower = sorted((n for n in self.deps.get(c, ()) if n not in self.existing), key=lambda n: self.rank[n])
            lower_new = lower
            used = {}
            for h, entries in dep_secs:
                for key, raw in entries:
                    if key in self.rank and key not in self.existing:
                        continue
                    if self.uses_lib(self.lib_name(key, raw), texts[c]):
                        used.setdefault(h, []).append((key, raw))
            have = {k for es in used.values() for k, _ in es} | set(lower)
            feat_lines = []
            for fname, fvals in features.items():
                vals = []
                for v in fvals:
                    if v.startswith("dep:"):
                        if v[4:] in have:
                            vals.append(v)
                    elif "/" in v:
                        if v.split("/")[0] in have:
                            vals.append(v)
                    elif v in features:
                        vals.append(v)
                if fname != "default":
                    vals += ["%s/%s" % (n, fname) for n in lower_new]
                feat_lines.append("%s = [%s]" % (fname, ", ".join('"%s"' % v for v in vals)))
            out.append("[features]")
            out.extend(feat_lines)
            out.append("")
            main = used.get("dependencies", [])
            out.append("[dependencies]")
            for n in lower:
                out.append("%s.workspace = true" % n)
            for key, raw in main:
                out.append(raw)
            out.append("")
            if lower and "test-fixtures" in features:
                used.setdefault("dev-dependencies", [])
            for h, entries in used.items():
                if h == "dependencies":
                    continue
                if h == "build-dependencies":
                    continue
                out.append("[%s]" % h)
                if h == "dev-dependencies" and "test-fixtures" in features:
                    # fixtures of the crates below are behind their feature
                    for n in lower:
                        out.append('%s = { workspace = true, features = ["test-fixtures"] }' % n)
                seen = set()
                for key, raw in entries:
                    if h == "dev-dependencies" and key in {k for k, _ in main} and key not in ("axum",):
                        continue
                    if key in seen:
                        continue
                    seen.add(key)
                    out.append(raw)
                out.append("")
            self.crate_external[c] = sorted(have)
            self.cargo_files["crates/%s/Cargo.toml" % c] = "\n".join(out)
        # build script: follows the sources it compiles
        self.build_move = None
        brel = self.src_crate_dir + "/build.rs"
        if os.path.exists(self.abspath(brel)):
            btext = self.read(brel)
            targets = set()
            new_b = btext
            for m in re.finditer(r"src/[\w./-]+", btext):
                old = self.src_crate_dir + "/" + m.group(0)
                new = self.assets.get(old)
                if new is None and old in self.files:
                    new = self.files[old].new_rel
                if new is None:
                    continue
                mm = re.match(r"crates/([^/]+)/(.*)$", new)
                targets.add(mm.group(1))
                new_b = new_b.replace(m.group(0), mm.group(2))
            if len(targets) == 1 and next(iter(targets)) != self.root_crate:
                tc = next(iter(targets))
                self.build_move = (brel, "crates/%s/build.rs" % tc, new_b)
                key = "crates/%s/Cargo.toml" % tc
                bsec = [raw for h, es in secs if h == "build-dependencies" for _, raw in es]
                self.cargo_files[key] += "\n[build-dependencies]\n" + "\n".join(bsec) + "\n"
            elif len(targets) > 1:
                self.unhandled.append({"file": brel, "line": 1, "construct": "build.rs compiles sources that land in several crates"})
        # root crate manifest
        root_deps = sorted([n for n in self.deps.get(self.root_crate, ()) if n not in self.existing], key=lambda n: self.rank[n])
        for n in new_crates:
            if n not in root_deps:
                root_deps.append(n)
        t = src_text
        add = "".join("%s.workspace = true\n" % n for n in root_deps)
        t = re.sub(r"(?m)^\[dependencies\]\n", lambda m: m.group(0) + add, t, count=1)
        for fname in features:
            if fname == "default":
                continue
            fw = ", ".join('"%s/%s"' % (n, fname) for n in root_deps)

            def repl(m, fw=fw):
                inner = m.group(2).strip()
                return m.group(1) + (inner + ", " if inner else "") + fw + "]"

            t = re.sub(r"(?m)^(" + re.escape(fname) + r"\s*=\s*\[)([^\]]*)\]", repl, t, count=1)
        self.cargo_files[src_cargo_rel] = t
        # workspace manifest
        w = self.read("Cargo.toml")
        mline = '    "%s",\n' % self.src_crate_dir
        addm = "".join('    "crates/%s",\n' % n for n in new_crates)
        w = w.replace(mline, mline + addm)
        addw = "".join('%s = { path = "crates/%s" }\n' % (n, n) for n in new_crates)
        w = re.sub(r"(?m)^gents-protocol = \{ path = [^\n]*\n", lambda m: m.group(0) + addw, w, count=1)
        self.cargo_files["Cargo.toml"] = w
        # consumers
        cons_deps = defaultdict(set)
        for sf in self.consumer_files:
            for tc in self.deps.get(sf.crate, ()):
                cons_deps[sf.crate].add(tc)
        ws_deps = tomllib.loads(w).get("workspace", {}).get("dependencies", {})
        for name, mdir in self.consumer_crates.items():
            need = sorted(cons_deps.get(name, ()), key=lambda n: self.rank[n])
            for lib in sorted(self.ext_deps.get(name, ())):
                key = lib.replace("_", "-")
                if key in ws_deps and key not in need:
                    need.append(key)
            if not need:
                continue
            rel = os.path.join(mdir, "Cargo.toml")
            ct = self.read(rel)
            have = tomllib.loads(ct)
            lines = ct.split("\n")
            out = []
            sec = None
            for ln in lines:
                out.append(ln)
                hm = re.match(r"^\[(.+)\]\s*$", ln)
                if hm:
                    sec = hm.group(1)
                    continue
                if sec in ("dependencies", "dev-dependencies") and re.match(r"^gents(\.workspace)?\s*=", ln):
                    present = have.get(sec, {})
                    if sec == "dev-dependencies" and "gents" in have.get("dependencies", {}):
                        continue
                    for n in need:
                        if n not in present:
                            out.append("%s = { workspace = true }" % n)
            self.cargo_files[rel] = "\n".join(out)
        # existing crates that receive files
        for c in self.existing:
            got = [sf for sf in self.files.values() if sf.crate == c and sf.new_rel and not sf.outside_src]
            if not got:
                continue
            rel = "crates/%s/Cargo.toml" % c
            ct = self.read(rel)
            have = tomllib.loads(ct)
            present = set(have.get("dependencies", {}))
            add = []
            tx = [self.rendered.get(sf, sf.text) for sf in got]
            for h, entries in dep_secs:
                if h != "dependencies":
                    continue
                for key, raw in entries:
                    if key in present or key in self.rank:
                        continue
                    if self.uses_lib(self.lib_name(key, raw), tx):
                        add.append(raw)
            if add:
                ct = re.sub(r"(?m)^\[dependencies\]\n", lambda m: m.group(0) + "\n".join(add) + "\n", ct, count=1)
                self.cargo_files[rel] = ct
                self.warnings.append("%s gains dependencies for sunk files: %s" % (c, ", ".join(a.split("=")[0].strip() for a in add)))

    # ---- reports

    def stats(self):
        st = {}
        for c in self.order:
            st[c] = {"files": 0, "lines": 0, "production_lines": 0, "test_files": 0}
        for sf in self.files.values():
            if sf.new_rel is None or sf.outside_src:
                continue
            s = st[sf.crate]
            s["files"] += 1
            nlines = sf.text.count("\n") + 1
            s["lines"] += nlines
            if sf.module.test:
                s["test_files"] += 1
                continue
            test_lines = 0
            for a, b in self._merged(sf.test_regions):
                test_lines += sf.text.count("\n", a, b) + 1
            s["production_lines"] += max(0, nlines - test_lines)
        return st

    @staticmethod
    def _merged(regions):
        out = []
        for a, b in sorted(regions):
            if out and a <= out[-1][1]:
                out[-1] = (out[-1][0], max(out[-1][1], b))
            else:
                out.append((a, b))
        return out

    def write_reports(self):
        os.makedirs(self.report_dir, exist_ok=True)

        def dump(name, obj):
            with open(os.path.join(self.report_dir, name), "w") as f:
                json.dump(obj, f, indent=1, sort_keys=True)
                f.write("\n")

        mapping = []
        for m in self.all_modules():
            if m is self.root or m.deleted:
                continue
            if m.file is not None and m.file.outside_src:
                continue
            entry = {
                "old": "gents::" + "::".join(m.path),
                "new_crate": m.crate,
                "new": crate_ident(m.crate) + "".join("::" + s for s in m.newpath),
                "inline": not m.is_file,
                "test_only": m.test,
            }
            if m.is_file and m.file is not None:
                entry["old_file"] = m.file.rel
                entry["new_file"] = m.file.new_rel
            mapping.append(entry)
        dump("mapping.json", {"modules": mapping, "files": {sf.rel: sf.new_rel for sf in self.files.values() if not sf.outside_src}, "assets": self.assets})
        up = self.upward
        pairs = defaultdict(lambda: {"production": 0, "test": 0})
        for u in up:
            pairs["%s -> %s" % (u["from_crate"], u["to_crate"])]["test" if u["test"] else "production"] += 1
        dump("upward_refs.json", {"total": len(up), "production": sum(1 for u in up if not u["test"]), "by_pair": pairs, "refs": up, "inherent_impls_in_another_crate": self.foreign_impls})
        wid = defaultdict(int)
        for w in self.widenings:
            wid[w["crate"]] += 1
        dump("widenings.json", {"total": len(self.widenings), "by_crate": wid, "widenings": self.widenings})
        dump("stats.json", self.stats())
        dump(
            "report.json",
            {
                "unassigned_top_level_modules": self.unassigned_top,
                "files_not_reachable_from_lib_rs": self.orphans,
                "skipped_manifest_entries": self.skipped_rules,
                "deleted_shims": [m.file.rel for m in self.all_modules() if m.deleted and m.file is not None],
                "constructs_not_rewritten": self.unhandled,
                "references_not_resolved": self.unresolved,
                "removed_root_reexports": self.removed_reexports,
                "string_literals_naming_moved_modules": self.string_paths,
                "synthesized_modules": {"%s::%s" % (crate_ident(c), "::".join(p)) if p else crate_ident(c): v for (c, p), v in self.synth.items()},
                "outstanding_splits": self.manifest.get("outstanding_splits", []),
                "warnings": self.warnings,
                "visibility_feedback_applied": len(self.feedback_applied),
                "visibility_feedback_not_located": self.feedback_missed,
                "dependencies_between_new_crates": {k: sorted(v, key=lambda n: self.rank.get(n, 99)) for k, v in self.deps.items() if not k.startswith("\0")},
                "counts": {
                    "tree_files": len(self.files),
                    "consumer_files_touched": sum(1 for sf in self.consumer_files if sf in self.rendered),
                    "files_rewritten": len(self.rendered),
                    "upward_refs": len(up),
                    "widenings": len(self.widenings),
                },
            },
        )

    # ---- writing

    def write_tree(self):
        print("moving files", file=sys.stderr)
        made = set()

        def ensure(rel):
            d = os.path.dirname(self.abspath(rel))
            if d not in made:
                os.makedirs(d, exist_ok=True)
                made.add(d)

        moves = []
        for sf in self.files.values():
            if sf.outside_src:
                continue
            if sf.new_rel is None:
                self.git("rm", "-q", sf.rel)
            elif sf.new_rel != sf.rel:
                moves.append((sf.rel, sf.new_rel))
        for old, new in self.assets.items():
            if old != new:
                moves.append((old, new))
        if self.build_move:
            moves.append((self.build_move[0], self.build_move[1]))
        # two phases so a destination that is another file's source is free
        for old, new in moves:
            if os.path.exists(self.abspath(new)):
                sys.exit("destination exists: %s (from %s)" % (new, old))
            ensure(new)
            self.git("mv", old, new)
        for sf, text in self.rendered.items():
            ensure(sf.new_rel)
            with open(self.abspath(sf.new_rel), "w", encoding="utf-8") as f:
                f.write(text)
        if self.build_move:
            with open(self.abspath(self.build_move[1]), "w") as f:
                f.write(self.build_move[2])
        for (crate, path), lines in self.synth.items():
            rel = "crates/%s/src/%s" % (crate, "/".join(path) + ".rs" if path else "lib.rs")
            ensure(rel)
            with open(self.abspath(rel), "w") as f:
                f.write("\n".join(lines) + "\n")
        for crate, lines in self.existing_lib_appends.items():
            rel = "crates/%s/src/lib.rs" % crate
            with open(self.abspath(rel), "a") as f:
                f.write("\n".join(lines) + "\n")
        for rel, text in self.cargo_files.items():
            ensure(rel)
            with open(self.abspath(rel), "w") as f:
                f.write(text if text.endswith("\n") else text + "\n")
        self.git("add", "-A", "crates", "apps", "Cargo.toml")


def git_out(repo, *args):
    return subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True, text=True).stdout


def run_fmt(repo):
    cmd = shlex.split(os.environ.get("CARGO_WRAPPER", "")) + [os.environ.get("CARGO", "cargo"), "fmt", "--all"]
    print("formatting: " + " ".join(cmd), file=sys.stderr)
    # rustfmt does not always reach its fixed point in one pass
    for _ in range(4):
        subprocess.run(cmd, cwd=repo, check=True)
        if subprocess.run(cmd + ["--", "--check"], cwd=repo, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
            break
    subprocess.run(["git", "add", "-A", "crates", "apps", "Cargo.toml"], cwd=repo, check=True)


def converge(repo, args):
    """apply, compile, feed private-member errors back, start over; until the
    compiler reports no new one."""
    dirty = [ln for ln in git_out(repo, "status", "--porcelain").splitlines() if ln.strip()]
    if dirty:
        sys.exit("--converge resets the checkout between rounds and needs a clean tree; commit or stash first:\n" + "\n".join(dirty[:10]))
    fb_path = os.path.join(os.path.dirname(args.manifest), "visibility-feedback.json")
    base = [sys.executable, os.path.abspath(__file__), "--manifest", args.manifest, "--report-dir", args.report_dir, "--repo", repo]
    census = [sys.executable, os.path.join(HERE, "census.py"), "--manifest", args.manifest, "--report-dir", args.report_dir, "--out", args.census_out]
    if args.tests:
        census.append("--tests")
    for round_no in range(1, args.max_rounds + 1):
        print("round %d: apply" % round_no, file=sys.stderr)
        subprocess.run(base, check=True)
        print("round %d: census" % round_no, file=sys.stderr)
        out = subprocess.run(census, cwd=repo, check=True, capture_output=True, text=True).stdout
        sys.stderr.write(out)
        m = re.search(r"new visibility feedback entries: (\d+)", out)
        if not m or int(m.group(1)) == 0:
            break
        with open(fb_path) as f:
            feedback = f.read()
        subprocess.run(["git", "reset", "-q", "--hard", "HEAD"], cwd=repo, check=True)
        subprocess.run(["git", "clean", "-qfd", "crates", "apps"], cwd=repo, check=True)
        with open(fb_path, "w") as f:
            f.write(feedback)
    else:
        print("warning: still finding private members after %d rounds" % args.max_rounds, file=sys.stderr)
    if args.fmt:
        run_fmt(repo)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--manifest", default=os.path.join(HERE, "manifest.json"))
    ap.add_argument("--report-dir", default=os.path.join(HERE, "report"))
    ap.add_argument("--dry-run", action="store_true", help="write reports only")
    ap.add_argument("--repo", help="checkout to apply to (default: the one holding this script)")
    ap.add_argument("--fmt", action="store_true", help="run `cargo fmt --all` over the result and stage it (for the commit that is kept)")
    ap.add_argument("--converge", action="store_true", help="apply, run census.py, reset and re-apply until the compiler reports no new private member")
    ap.add_argument("--census-out", default=os.path.join(HERE, "report", "census"), help="where --converge writes the census (default: report/census)")
    ap.add_argument("--tests", action="store_true", help="with --converge: also check the test targets of crates that compile")
    ap.add_argument("--max-rounds", type=int, default=6, help="with --converge: give up after this many rounds")
    args = ap.parse_args()
    repo = args.repo or subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=HERE, check=True, capture_output=True, text=True).stdout.strip()
    args.manifest = os.path.abspath(args.manifest)
    args.report_dir = os.path.abspath(args.report_dir)
    args.census_out = os.path.abspath(args.census_out)
    if args.converge:
        converge(repo, args)
        return
    tool = Tool(repo, args.manifest, args.report_dir, args.dry_run)
    tool.run()
    c = tool
    print(
        "done: %d files rewritten, %d upward references (%d production), %d widenings, %d constructs not rewritten"
        % (len(c.rendered), len(c.upward), sum(1 for u in c.upward if not u["test"]), len(c.widenings), len(c.unhandled)),
        file=sys.stderr,
    )
    if args.fmt and not args.dry_run:
        run_fmt(repo)


if __name__ == "__main__":
    main()
