"""Checks of the real WebAssembly build that the case files and the native tests cannot see,
run through `gents plugin run` against the installed pack (scripts/test-pack.sh runs it).

    GENTS=<gents binary> PACK_HOME=<home with the pack installed> python3 tools/wasm_check.py [rounds]

1. Damaged inputs: every small committed fixture is damaged (fixed seed) and queried; each outcome
   is a result or one plain sentence, never a trap, a panic or a memory-budget kill.
2. The output limit: 3 MB of rows beside a 1000-row Markdown table come back inside 4 MiB.
3. A table at the column cap answers `SELECT *` in well under the wall clock.
4. A folder of large workbooks beside a CSV answers a query on the CSV without reading them.
"""
import glob
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
import zipfile

GENTS = os.environ.get("GENTS", "gents")
HOME = os.environ["PACK_HOME"]
FIXTURES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tests", "fixtures")
OUTPUT_LIMIT = 4 * 1024 * 1024
failures = []


def run(folder, request, timeout=900):
    """One plugin call; (exit code, stdout, stderr, seconds)."""
    start = time.time()
    p = subprocess.run(
        [GENTS, "plugin", "run", "data_tables", "--home", HOME, "--bind-dir", folder,
         "--input", json.dumps(request)],
        capture_output=True, text=True, timeout=timeout)
    return p.returncode, p.stdout, p.stderr, time.time() - start


def fail(what):
    failures.append(what)
    print("FAIL", what, flush=True)


def damage(rng, data):
    b = bytearray(data)
    if not b:
        return bytes([rng.randrange(256)])
    kind = rng.randrange(5)
    if kind == 0:
        for _ in range(1 + rng.randrange(8)):
            b[rng.randrange(len(b))] ^= 1 << rng.randrange(8)
    elif kind == 1:
        del b[rng.randrange(len(b)):]
    elif kind == 2:
        i = rng.randrange(len(b))
        b[i:i] = bytes(rng.randrange(256) for _ in range(1 + rng.randrange(16)))
    elif kind == 3:
        i = rng.randrange(len(b))
        n = 1 + rng.randrange(32)
        b[i:i + n] = bytes([rng.choice([0, 255])]) * len(b[i:i + n])
    else:
        i = rng.randrange(len(b))
        chunk = b[i:i + 1 + rng.randrange(64)]
        at = rng.randrange(len(b))
        b[at:at] = chunk
    return bytes(b)


def plain_failure(code, err):
    """A failed call is acceptable when the plugin ended it with its own sentence (gents reports
    it as `<plugin> failed: data_tables: <sentence>`) and the host reports no crash."""
    bad = ("trap", "panick", "unreachable", "memory budget", "wall clock", "Traceback")
    return code != 0 and "failed: data_tables: " in err and not any(w in err for w in bad)


def fuzz(rounds):
    files = [f for pat in ("formats/*", "hostile/*") for f in sorted(glob.glob(os.path.join(FIXTURES, pat)))
             if os.path.isfile(f) and os.path.getsize(f) < 400_000]
    rng = random.Random(20261005)
    runs = 0
    for f in files:
        name = os.path.basename(f)
        stem = name.rsplit(".", 1)[0]
        data = open(f, "rb").read()
        for r in range(rounds):
            d = tempfile.mkdtemp()
            open(os.path.join(d, name), "wb").write(damage(rng, data))
            for request in ({"mode": "tables"}, {"sql": f"SELECT * FROM {stem} LIMIT 20"},
                            {"mode": "describe", "sample_rows": 100}):
                code, out, err, _ = run(d, request, timeout=300)
                runs += 1
                if code != 0 and not plain_failure(code, err):
                    fail(f"damaged {name} round {r} {request}: exit {code}: {err.strip()[-300:]}")
            shutil.rmtree(d)
    print(f"damaged inputs: {runs} runs", flush=True)


def output_limit():
    d = tempfile.mkdtemp()
    with open(os.path.join(d, "t.csv"), "w") as f:
        f.write(",".join(f"c{c}" for c in range(10)) + "\n")
        for r in range(7000):
            f.write(",".join(f"c{c}-{r:0>44}" for c in range(10)) + "\n")
    code, out, err, secs = run(d, {"sql": "SELECT * FROM t", "max_rows": 100000, "max_bytes": 3000000,
                                   "markdown_rows": 1000})
    # gents prints the result indented; the host's limit is on the plugin's own compact output.
    compact = 0
    if code != 0:
        fail(f"output limit: exit {code}: {err.strip()[-300:]}")
    else:
        r = json.loads(out)
        compact = len(json.dumps(r, separators=(",", ":"), ensure_ascii=False).encode())
        if compact > OUTPUT_LIMIT:
            fail(f"output limit: {compact} bytes")
        elif not r["rows"] or "next" not in r:
            fail("output limit: expected a full first page with a cursor")
    print(f"output limit: {compact} bytes in {secs:.1f}s", flush=True)
    shutil.rmtree(d)


def wide_table():
    code, out, err, secs = run(os.path.join(FIXTURES, "wide"), {"sql": "SELECT * FROM ok"})
    if code != 0:
        fail(f"wide table: exit {code}: {err.strip()[:200]}")
    elif len(json.loads(out)["columns"]) != 2000:
        fail("wide table: expected 2000 columns")
    elif secs > 120:
        fail(f"wide table: SELECT * took {secs:.0f}s")
    print(f"wide table: SELECT * over 2000 columns in {secs:.1f}s", flush=True)


def workbooks():
    """Eight hard links of one workbook with 240 MiB of shared strings, beside a small CSV."""
    d = tempfile.mkdtemp()
    open(os.path.join(d, "small.csv"), "w").write("x\n1\n2\n")
    book = os.path.join(d, "book00.xlsx")
    ns = 'xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"'
    with zipfile.ZipFile(book, "w", zipfile.ZIP_DEFLATED, compresslevel=1) as z:
        z.writestr("[Content_Types].xml", "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>")
        z.writestr("xl/workbook.xml", f'<workbook {ns} xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>')
        z.writestr("xl/_rels/workbook.xml.rels", '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="worksheet" Target="worksheets/sheet1.xml"/></Relationships>')
        z.writestr("xl/worksheets/sheet1.xml", f'<worksheet {ns}><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData></worksheet>')
        with z.open("xl/sharedStrings.xml", "w", force_zip64=True) as s:
            s.write(f'<sst {ns}>'.encode())
            for start in range(0, 3_200_000, 20_000):
                s.write("".join(f"<si><t>string number {i:010d} with some padding text</t></si>"
                                for i in range(start, start + 20_000)).encode())
            s.write(b"</sst>")
    for i in range(1, 8):
        os.link(book, os.path.join(d, f"book{i:02d}.xlsx"))
    code, out, err, secs = run(d, {"sql": "SELECT * FROM small"})
    if code != 0:
        fail(f"workbooks: exit {code}: {err.strip()[:200]}")
    elif json.loads(out)["rows"] != [[1], [2]]:
        fail("workbooks: wrong rows")
    elif secs > 60:
        fail(f"workbooks: a query on the CSV took {secs:.0f}s")
    print(f"workbooks: query beside 8 large workbooks in {secs:.1f}s", flush=True)
    shutil.rmtree(d)


fuzz(int(sys.argv[1]) if len(sys.argv) > 1 else 2)
output_limit()
wide_table()
workbooks()
print(f"{len(failures)} failures")
sys.exit(1 if failures else 0)
