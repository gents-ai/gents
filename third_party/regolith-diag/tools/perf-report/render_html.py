#!/usr/bin/env python3
"""Render a schema v1 or v2 run file into one HTML page built for reading.

The page exists to answer one question first - did anything regress -
and to make the answer hard to misread. So the verdict is the largest
thing on it, the per-metric table sits above the charts, and a metric
that could not be compared is listed as unverified rather than being
dropped or rendered as a zero.

Self-contained by construction: the charts are inline SVG built here,
not drawn by a library at view time, so an artifact downloaded from CI
a year from now still renders with no network.
"""

import argparse
import html
import json
import math
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).parent))
from compare import compare  # noqa: E402
from timings import FAMILIES, families, throughput_rows  # noqa: E402

GREEN = "#06b250"
MUTED = "#4a504a"


def esc(s):
    return html.escape(str(s), quote=True)


def fmt_num(n):
    """Compact first, decimals only when they are the whole story."""
    if not isinstance(n, (int, float)) or isinstance(n, bool) or not math.isfinite(n):
        return "n/a"
    a = abs(n)
    if a >= 1_000_000:
        return f"{n/1_000_000:.2f}M"
    if a >= 10_000:
        return f"{n/1000:.1f}k"
    if isinstance(n, float) and abs(n - round(n)) > 1e-9:
        return f"{n:,.1f}"
    return f"{round(n):,}"


def pill(trust):
    t = trust or "clean"
    label = {"clean": "clean", "contaminated": "busy host", "absent": "not measured",
             "per-entry": "mixed"}.get(t, t)
    return f'<span class="pill p-{esc(t)}">{esc(label)}</span>'


CSS = """
@import url('https://fonts.googleapis.com/css2?family=Funnel+Display:wght@300;400;500;600;700;800&display=swap');
:root{
  --green:#06b250; --green-2:#0ad866; --green-dim:#064f28;
  --bg:#000; --panel:#0c0d0c; --panel-2:#111311; --line:#1e211e;
  --text:#f2f4f2; --muted:#7d857d; --dim:#4a504a;
  --bad:#e0503c; --bad-dim:#5c1f16; --warn:#e0a93a;
}
*{box-sizing:border-box;margin:0;padding:0}
html{-webkit-text-size-adjust:100%}
body{
  background:var(--bg); color:var(--text);
  font-family:'Funnel Display',ui-sans-serif,system-ui,-apple-system,sans-serif;
  font-size:15px; line-height:1.5; -webkit-font-smoothing:antialiased;
  font-variant-numeric:tabular-nums;
}
.wrap{width:100%;margin:0;padding:0 clamp(20px,3vw,44px) 100px}

/* ---- verdict hero: the first and largest thing on the page ---- */
.hero{padding:72px 0 40px;border-bottom:1px solid var(--line)}
.eyebrow{
  font-size:11px;letter-spacing:.22em;text-transform:uppercase;
  color:var(--muted);font-weight:600;margin-bottom:20px
}
.verdict{display:flex;align-items:baseline;gap:20px;flex-wrap:wrap}
.verdict h1{
  font-size:clamp(46px,8vw,86px);font-weight:800;letter-spacing:-.035em;
  line-height:.95
}
.v-pass h1{color:var(--green)}
.v-regressed h1{color:var(--bad)}
.v-unverified h1{color:var(--warn)}
.v-baseline h1{color:var(--text)}
.verdict .because{color:var(--muted);font-size:16px;max-width:46ch;font-weight:400}
.counts{display:flex;gap:36px;margin-top:34px;flex-wrap:wrap}
.count .n{font-size:34px;font-weight:700;letter-spacing:-.02em;line-height:1}
.count .l{font-size:11px;letter-spacing:.14em;text-transform:uppercase;color:var(--muted);margin-top:7px;font-weight:600}
.count.good .n{color:var(--green)}
.count.bad .n{color:var(--bad)}
.count.warn .n{color:var(--warn)}
.runmeta{display:flex;gap:8px 30px;flex-wrap:wrap;margin-top:34px;font-size:13px;color:var(--muted)}
.runmeta b{color:var(--text);font-weight:500}

h2{
  font-size:12px;font-weight:700;letter-spacing:.2em;text-transform:uppercase;
  color:var(--muted);margin:64px 0 18px;display:flex;align-items:center;gap:14px
}
h2::after{content:"";flex:1;height:1px;background:var(--line)}

/* ---- the regression table: the thing that gets read ---- */
table{width:100%;border-collapse:collapse;font-size:14px}
th{
  text-align:left;font-size:10.5px;font-weight:700;letter-spacing:.13em;
  text-transform:uppercase;color:var(--dim);padding:0 14px 12px 0;
  border-bottom:1px solid var(--line);white-space:nowrap
}
th.r,td.r{text-align:right}
td{padding:13px 14px 13px 0;border-bottom:1px solid var(--line);vertical-align:baseline}
tr:last-child td{border-bottom:0}
tbody tr{position:relative}
tbody tr:hover{background:var(--panel)}
td.metric{font-weight:500}
td.num{font-variant-numeric:tabular-nums;color:var(--muted)}
td.delta{font-weight:700;font-variant-numeric:tabular-nums;white-space:nowrap}
.d-regressed{color:var(--bad)} .d-improved{color:var(--green)}
.d-noise{color:var(--dim)} .d-unverified{color:var(--warn)}
.stripe{display:inline-block;width:3px;height:15px;border-radius:2px;margin-right:11px;vertical-align:-2px}
.s-regressed{background:var(--bad)} .s-improved{background:var(--green)}
.s-noise{background:var(--dim)} .s-unverified{background:var(--warn)}
.why{color:var(--dim);font-size:12.5px;font-weight:400}

/* ---- charts ---- */
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,420px),1fr));gap:14px;align-items:start}
.card{background:var(--panel);border:1px solid var(--line);border-radius:12px;padding:22px 24px 18px}
.card h3{font-size:15px;font-weight:600;margin-bottom:3px;display:flex;align-items:center;gap:10px;flex-wrap:wrap}
.card .note{color:var(--muted);font-size:12.5px;margin-bottom:18px;line-height:1.45}
.card .headline{font-size:26px;font-weight:700;letter-spacing:-.02em;margin:2px 0 14px}
svg{width:100%;height:auto;display:block;overflow:visible}
.tick{fill:var(--dim);font-size:10.5px;font-family:'Funnel Display',sans-serif}
.axis{fill:var(--dim);font-size:10.5px;letter-spacing:.1em;text-transform:uppercase}
.legend{display:flex;gap:20px;margin-top:14px;font-size:11.5px;color:var(--muted)}
.key i{display:inline-block;width:14px;height:3px;border-radius:2px;margin-right:8px;vertical-align:middle}
.pill{
  display:inline-block;padding:3px 10px;border-radius:999px;font-size:10px;
  font-weight:700;letter-spacing:.1em;text-transform:uppercase
}
.p-clean{color:var(--green);background:var(--green-dim)}
.p-contaminated{color:var(--warn);background:#3d2e0d}
.p-absent{color:var(--muted);background:#1a1c1a}
.bars{display:flex;flex-direction:column;gap:13px}
.barrow{display:grid;grid-template-columns:minmax(150px,1.4fr) 3fr minmax(118px,auto);
  align-items:center;gap:16px}
.barlab{font-size:13px;color:var(--muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.bartrack{position:relative;height:16px;display:flex;align-items:center}
.bar{position:absolute;height:5px;border-radius:3px}
.bar.ghost{background:#333833;top:2px}
.bar.now{background:var(--green);bottom:2px;box-shadow:0 0 12px rgba(6,178,80,.35)}
.barval{font-size:13px;text-align:right;white-space:nowrap;font-variant-numeric:tabular-nums}
.barval .unit{color:var(--dim);font-size:11.5px}
.empty{
  border:1px dashed var(--line);border-radius:10px;padding:26px;text-align:center;
  color:var(--dim);font-size:13px
}
footer{margin-top:80px;padding-top:26px;border-top:1px solid var(--line);color:var(--dim);font-size:12.5px;line-height:1.7}
code{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:12px;color:var(--muted)}
@media(max-width:640px){.wrap{padding:0 16px 70px}.grid{grid-template-columns:1fr}.hero{padding:44px 0 30px}}
"""


# --- charts -----------------------------------------------------------

def area_chart(series, width=520, height=240, x_label="threads"):
    """Line + area fill. `series` is [(name, colour, fill, [(x,y)...])]."""
    drawn = [s for s in series if s[3]]
    if not drawn:
        return '<div class="empty">no points recorded</div>'
    xs = [p[0] for *_, pts in drawn for p in pts]
    ys = [p[1] for *_, pts in drawn for p in pts]
    x0, x1, y1 = min(xs), max(xs), max(ys)
    if x1 == x0: x1 = x0 + 1
    if y1 <= 0: y1 = 1
    pl, pr, pt, pb = 58, 14, 14, 30
    iw, ih = width - pl - pr, height - pt - pb
    fx = lambda x: pl + (x - x0) / (x1 - x0) * iw
    fy = lambda y: pt + ih - y / y1 * ih

    o = [f'<svg viewBox="0 0 {width} {height}" preserveAspectRatio="none" role="img">']
    o.append('<defs>')
    for i, (_, colour, fill, _) in enumerate(drawn):
        if fill:
            o.append(f'<linearGradient id="g{i}" x1="0" y1="0" x2="0" y2="1">'
                     f'<stop offset="0" stop-color="{colour}" stop-opacity=".28"/>'
                     f'<stop offset="1" stop-color="{colour}" stop-opacity="0"/></linearGradient>')
    o.append('</defs>')
    for i in range(4):
        gy = pt + ih * i / 3
        o.append(f'<line x1="{pl}" y1="{gy:.1f}" x2="{width-pr}" y2="{gy:.1f}" '
                 f'stroke="#1e211e" stroke-width="1"/>')
        o.append(f'<text x="{pl-9}" y="{gy+3.5:.1f}" text-anchor="end" class="tick">'
                 f'{fmt_num(y1 - y1*i/3)}</text>')
    for i, (_, colour, fill, pts) in enumerate(drawn):
        pts = sorted(pts)
        d = " ".join(f"{'M' if k==0 else 'L'}{fx(x):.1f},{fy(y):.1f}"
                     for k, (x, y) in enumerate(pts))
        if fill:
            o.append(f'<path d="{d} L{fx(pts[-1][0]):.1f},{pt+ih:.1f} '
                     f'L{fx(pts[0][0]):.1f},{pt+ih:.1f} Z" fill="url(#g{i})"/>')
        o.append(f'<path d="{d}" fill="none" stroke="{colour}" stroke-width="2.25" '
                 f'stroke-linecap="round" stroke-linejoin="round"/>')
        lx, ly = pts[-1]
        o.append(f'<circle cx="{fx(lx):.1f}" cy="{fy(ly):.1f}" r="4.5" fill="{colour}"/>')
        o.append(f'<circle cx="{fx(lx):.1f}" cy="{fy(ly):.1f}" r="8" fill="{colour}" opacity=".18"/>')
    # At most six labels: a seconds axis carries hundreds of samples and
    # every one of them drawn is a grey smear, not an axis.
    uniq = sorted({p[0] for *_, pts in drawn for p in pts})
    if len(uniq) > 6:
        step = (len(uniq) - 1) / 5
        uniq = [uniq[round(i * step)] for i in range(6)]
    for x in uniq:
        o.append(f'<text x="{fx(x):.1f}" y="{height-11}" text-anchor="middle" '
                 f'class="tick">{esc(fmt_num(x) if x >= 1000 else x)}</text>')
    o.append(f'<text x="{pl}" y="{height-1}" class="axis">{esc(x_label)}</text></svg>')
    legend = " ".join(f'<span class="key"><i style="background:{c}"></i>{esc(n)}</span>'
                      for n, c, _, _ in drawn)
    return "".join(o) + f'<div class="legend">{legend}</div>'


def spark_bars(rows, higher_is_better=False, **_):
    """`(label, before, after, unit)` as HTML bars.

    Deliberately not SVG. An SVG scaled to its container scales its text
    with it, so the same chart rendered in a wide card grows 40px
    labels; HTML keeps type at the size it was set in.
    """
    rows = [r for r in rows if r[2] is not None]
    if not rows:
        return '<div class="empty">nothing to compare</div>'
    top = max(max(r[1] or 0, r[2]) for r in rows) or 1
    out = ['<div class="bars">']
    for label, before, after, unit in rows:
        delta = ""
        if before:
            pct = (after - before) / before * 100
            improved = pct >= 0 if higher_is_better else pct <= 0
            cls = "d-improved" if improved else "d-regressed"
            delta = f'<span class="{cls}">{pct:+.0f}%</span>'
        bw = f"{(before or 0) / top * 100:.1f}%"
        aw = f"{after / top * 100:.1f}%"
        ghost = (f'<div class="bar ghost" style="width:{bw}"></div>'
                 if before else "")
        out.append(
            f'<div class="barrow"><div class="barlab">{esc(label)}</div>'
            f'<div class="bartrack">{ghost}'
            f'<div class="bar now" style="width:{aw}"></div></div>'
            f'<div class="barval">{fmt_num(after)}<span class="unit"> {esc(unit)}</span>'
            f' {delta}</div></div>')
    out.append("</div>")
    return "".join(out)


# --- sections ---------------------------------------------------------

VERDICT_COPY = {
    "baseline": ("BASELINE",
                 "No earlier run to compare against, so this one is the record "
                 "future runs are measured from rather than a verdict."),
    "pass": ("NO REGRESSION",
             "Every metric that could be compared held or improved against the baseline."),
    "regressed": ("REGRESSION",
                  "At least one metric moved beyond the noise threshold with "
                  "non-overlapping measurement ranges."),
    "unverified": ("UNVERIFIED",
                   "At least one metric could not be verified. Check the missing "
                   "measurements, workload settings and host trust before drawing a conclusion."),
}


def hero(cur, base, rep):
    run = cur.get("run", {})
    host = run.get("host", {})
    v = rep.verdict if rep else "baseline"
    title, because = VERDICT_COPY.get(v, VERDICT_COPY["unverified"])
    counts = ""
    if rep:
        counts = (
            f'<div class="counts">'
            f'<div class="count bad"><div class="n">{len(rep.regressions)}</div>'
            f'<div class="l">regressed</div></div>'
            f'<div class="count good"><div class="n">{len(rep.improvements)}</div>'
            f'<div class="l">improved</div></div>'
            f'<div class="count warn"><div class="n">{len(rep.unverified)}</div>'
            f'<div class="l">unverified</div></div>'
            f'<div class="count"><div class="n">{len(rep.deltas)}</div>'
            f'<div class="l">compared</div></div></div>')
    meta = []
    if run.get("commit"): meta.append(f'<div>commit <b>{esc(run["commit"])}</b></div>')
    bsha = (base or {}).get("run", {}).get("commit")
    if bsha: meta.append(f'<div>baseline <b>{esc(bsha)}</b></div>')
    if host.get("cpu"): meta.append(f'<div>{esc(host["cpu"])}, <b>{esc(host.get("cores","?"))}</b> cores</div>')
    if host.get("store"): meta.append(f'<div>{esc(host["store"])}</div>')
    return (f'<div class="hero v-{v}"><div class="eyebrow">regolith performance</div>'
            f'<div class="verdict"><h1>{title}</h1>'
            f'<p class="because">{because}</p></div>{counts}'
            f'<div class="runmeta">{"".join(meta)}</div></div>')


def deltas_table(rep):
    if not rep or not rep.deltas:
        return ""
    order = {"regressed": 0, "improved": 1, "unverified": 2, "noise": 3}
    rows = []
    for d in sorted(rep.deltas, key=lambda d: (order.get(d.verdict, 9), d.name)):
        pct = f"{d.pct:+.1f}%" if d.pct is not None else "n/a"
        why = f'<div class="why">{esc(d.reason)}</div>' if d.reason else ""
        rows.append(
            f'<tr><td class="metric"><span class="stripe s-{d.verdict}"></span>'
            f'{esc(d.name)}{why}</td>'
            f'<td class="num r">{fmt_num(d.before)}</td>'
            f'<td class="num r">{fmt_num(d.after)}</td>'
            f'<td class="num r">{esc(d.unit)}</td>'
            f'<td class="delta r d-{d.verdict}">{pct}</td></tr>')
    return ('<h2>Every metric compared</h2><table><thead><tr>'
            '<th>Metric</th><th class="r">Baseline</th><th class="r">This run</th>'
            '<th class="r">Unit</th><th class="r">Change</th></tr></thead>'
            f'<tbody>{"".join(rows)}</tbody></table>')


def detail_sections(cur, base):
    out = []
    ct = cur.get("metrics", {}).get("throughput", {})
    bt = (base or {}).get("metrics", {}).get("throughput", {})
    cards = []
    for name, w in ct.get("workloads", {}).items():
        pts = [(p["threads"], p["median"]) for p in throughput_rows(w)
               if isinstance(p["median"], (int, float)) and math.isfinite(p["median"])]
        bpts = [(p["threads"], p["median"]) for p in
                throughput_rows(bt.get("workloads", {}).get(name, {}))
                if isinstance(p["median"], (int, float)) and math.isfinite(p["median"])]
        head = ""
        if pts:
            one = dict(pts).get(1)
            top = max(pts, key=lambda p: p[0])
            head = f'<div class="headline">{fmt_num(top[1])} <span style="font-size:14px;color:#7d857d">ops/s @ {top[0]}t</span></div>'
            if one:
                head = head.replace("</div>", f' <span style="font-size:13px;color:#7d857d">&nbsp;{top[1]/one:.2f}x scaling</span></div>')
        chart = area_chart([("baseline", "#333833", False, bpts),
                            ("this run", GREEN, True, pts)])
        cards.append(f'<div class="card"><h3>{esc(name.replace("_"," "))}'
                     f'{pill(w.get("trust"))}</h3>'
                     f'<p class="note">{esc(w.get("note","") or "")}</p>'
                     f'{head}{chart}</div>')
    if cards:
        out.append('<h2>Throughput</h2><div class="grid">' + "".join(cards) + "</div>")

    cr = cur.get("metrics", {}).get("rss", {})
    soaks = cr.get("soaks", [])
    if soaks:
        series = []
        for s in soaks:
            pts = downsample([(r[0], r[1]) for r in s.get("samples_t_rss_ops", [])
                              if isinstance(r, (list, tuple)) and len(r) >= 2])
            after = "after" in (s.get("variant") or "")
            series.append((s.get("label", s.get("variant", "")),
                           GREEN if after else "#333833", after, pts))
        rows = [(s.get("label", s.get("variant", "")), None, s.get("rss_peak_mib"), "MiB")
                for s in soaks if s.get("rss_peak_mib") is not None]
        out.append(
            f'<h2>Resident memory</h2><div class="grid">'
            f'<div class="card"><h3>RSS over a sustained workload{pill(cr.get("trust"))}</h3>'
            f'<p class="note">{esc(cr.get("workload",""))}</p>'
            f'{area_chart(series, x_label="seconds")}</div>'
            f'<div class="card"><h3>Peak RSS</h3>'
            f'<p class="note">The number a memory budget has to cover.</p>'
            f'{spark_bars(rows)}</div></div>')

    ci = cur.get("metrics", {}).get("isolation", {})
    cells = ci.get("matrix", [])
    if cells:
        bmap = {(c.get("flavor"), c.get("isolation")): c
                for c in (base or {}).get("metrics", {}).get("isolation", {}).get("matrix", [])}
        cards = []
        for metric, title, note in (
            ("uncontended_commits_per_s", "Uncontended commits/s",
             "One writer per key. What the isolation level costs when nothing is racing."),
            ("contended_commits_per_s", "Contended commits/s",
             "Every thread on one key. What the level costs when everything is racing."),
        ):
            rows = []
            for c in cells:
                label = f'{c.get("flavor","?")} / {c.get("isolation","?")}'
                prev = bmap.get((c.get("flavor"), c.get("isolation")), {})
                rows.append((label, prev.get(metric), c.get(metric), "ops/s"))
            cards.append(f'<div class="card"><h3>{title}{pill(ci.get("trust"))}</h3>'
                          f'<p class="note">{esc(note)}</p>{spark_bars(rows, higher_is_better=True)}</div>')
        rows = [(f'{c.get("flavor","?")} / {c.get("isolation","?")}',
                 bmap.get((c.get("flavor"), c.get("isolation")), {}).get("conflict_rate_pct"),
                 c.get("conflict_rate_pct"), "%") for c in cells]
        cards.append('<div class="card"><h3>Conflict rate</h3>'
                     '<p class="note">Share of attempts a level refused. A stricter level '
                     'refuses more, and that is the work it is doing, not waste.</p>'
                     f'{spark_bars(rows)}</div>')
        out.append('<h2>Transaction isolation</h2>'
                   f'<p class="note">{esc(ci.get("note",""))}</p>'
                   '<div class="grid">' + "".join(cards) + "</div>")

    cs = cur.get("metrics", {}).get("binary_size", {})
    if cs.get("rows"):
        bmap = {r.get("artifact"): r.get("regolith_cost_kib")
                for r in (base or {}).get("metrics", {}).get("binary_size", {}).get("rows", [])}
        rows = [(r.get("artifact", "?"), bmap.get(r.get("artifact")),
                 r.get("regolith_cost_kib"), "KiB") for r in cs["rows"]]
        out.append(f'<h2>Binary size</h2><div class="grid"><div class="card">'
                   f'<h3>regolith\'s own contribution{pill(cs.get("trust"))}</h3>'
                   f'<p class="note">{esc(cs.get("note",""))}</p>{spark_bars(rows)}</div></div>')
    return "".join(out)


def timing_sections(cur, base):
    current, baseline = families(cur), families(base or {})
    cards = []
    for name in sorted(set(FAMILIES) | set(current) | set(baseline)):
        family = current.get(name, {})
        old = {row["key"]: row for row in baseline.get(name, {}).get("rows", [])}
        rows = []
        for row in family.get("rows", []):
            before = old.get(row["key"], {})
            bounds = row.get("invalid_reason") or (f'{fmt_num(row["min"])} to {fmt_num(row["max"])}'
                      if row.get("min") is not None and row.get("max") is not None
                      else "not recorded")
            rows.append(f'<tr><td>{esc(row["name"])}</td>'
                        f'<td class="num">{fmt_num(before.get("median"))}</td>'
                        f'<td class="num">{fmt_num(row.get("median"))}</td>'
                        f'<td>{esc(row["unit"])}</td><td>{esc(bounds)}</td></tr>')
        body = ('<table><thead><tr><th>Workload</th><th>Baseline</th><th>This run</th>'
                '<th>Unit</th><th>Observed range</th></tr></thead>'
                f'<tbody>{"".join(rows)}</tbody></table>' if rows else
                '<div class="empty">not collected in this run</div>')
        cards.append(f'<div class="card"><h3>{esc(name.replace("_", " "))}'
                     f'{pill(family.get("trust", "absent"))}</h3>{body}</div>')
    return '<h2>Transaction and mechanism timings</h2><div class="grid">' + "".join(cards) + '</div>'


def downsample(pts, target=200):
    """Thin a curve for drawing, keeping the peak.

    A soak records thousands of samples; a 520px chart cannot show them
    and carrying them all makes the artifact megabytes. The peak is kept
    explicitly because it is the number the chart exists to show, and a
    stride can drop it.
    """
    if len(pts) <= target:
        return pts
    stride = len(pts) / target
    out = [pts[int(i * stride)] for i in range(target)]
    peak = max(pts, key=lambda p: p[1])
    if peak not in out:
        out.append(peak)
    return sorted(out)


def render(cur, base, out_path, threshold=3.0):
    for doc in [cur] + ([base] if base else []):
        if doc.get("schema_version") not in (1, 2):
            raise ValueError(f"unsupported schema_version {doc.get('schema_version')!r}")
    rep = compare(base, cur, threshold) if base else None
    retr = cur.get("retractions") or []
    retr_html = ""
    if retr:
        items = "".join(f"<li>{esc(r)}</li>" for r in retr)
        retr_html = (f'<h2>Retracted from this run</h2>'
                     f'<div class="card"><p class="note">Claims withdrawn rather than '
                     f'deleted, so a reader of an older artifact can see what changed.</p>'
                     f'<ul style="margin-left:18px;color:#7d857d;font-size:13px">{items}</ul></div>')

    page = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>regolith performance</title>
<style>{CSS}</style></head>
<body><div class="wrap">
{hero(cur, base, rep)}
{deltas_table(rep)}
{detail_sections(cur, base)}
{timing_sections(cur, base)}
{retr_html}
<footer>
Rendered from a schema v{cur.get("schema_version")} run file by <code>tools/perf-report/render_html.py</code>.
A metric that could not be compared is listed as unverified rather than dropped, and a
family measured on a busy host carries that on its heading. A delta counts as real only
when it clears the {threshold:g}% threshold <em>and</em> the two runs' measured ranges do
not overlap, because a percentage computed across overlapping ranges is a description of
the machine rather than of the code.
</footer>
</div></body></html>"""
    pathlib.Path(out_path).write_text(page)
    return page, rep


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--current", required=True)
    ap.add_argument("--baseline")
    ap.add_argument("--out", required=True)
    ap.add_argument("--threshold", type=float, default=3.0)
    ap.add_argument("--fail-on-regression", action="store_true")
    args = ap.parse_args()

    cur = json.loads(pathlib.Path(args.current).read_text())
    base = json.loads(pathlib.Path(args.baseline).read_text()) if args.baseline else None
    if cur.get("schema_version") not in (1, 2):
        sys.exit(f"unsupported schema_version {cur.get('schema_version')!r}")
    page, rep = render(cur, base, args.out, args.threshold)
    print(f"wrote {args.out} ({len(page)/1024:.1f} KiB)"
          + (f" verdict={rep.verdict}" if rep else ""))
    if args.fail_on_regression and rep and rep.regressions:
        sys.exit(1)


if __name__ == "__main__":
    main()
