// Render the dashboard headlessly and refuse a publish that would drop a
// family on the floor.
//
// The page reads run documents the benches produce. Nothing links the two,
// so a bench can change the shape of what it records and the section that
// draws it silently renders nothing. That is how `rss`, `binary_size` and
// `correctness` came to be collected on every run and shown on none: the
// job stayed green because a blank section is not an error.
//
// This runs the page's own code over the documents about to be published
// and fails when a family that was collected produced no card, so the
// failure lands in CI rather than on the site.
//
// Usage: node tools/perf-site/render_check.mjs <site dir>

import { readFileSync } from "node:fs";

const site = process.argv[2];
if (!site) {
  console.error("usage: render_check.mjs <site dir>");
  process.exit(2);
}

const html = readFileSync(`${site}/perf.html`, "utf8");
const script = html.slice(html.indexOf("<script>") + 8, html.lastIndexOf("</script>"));

// Enough of a <select> for the page's own boot path to run unchanged: a
// browser adopts the selected option's value the moment the options are
// written, and `boot` depends on that to pick the run it draws. Setting
// the values from here instead would test a path the browser never takes.
const nodes = {};
const node = (id) =>
  (nodes[id] ??= {
    _html: "",
    value: id === "#thresh" ? "3" : "",
    style: {},
    addEventListener() {},
    set innerHTML(v) {
      this._html = v;
      const opts = [...v.matchAll(/<option value="([^"]*)"([^>]*)>/g)];
      if (opts.length) this.value = (opts.find((o) => /\bselected\b/.test(o[2])) || opts[0])[1];
    },
    get innerHTML() {
      return this._html;
    },
  });

// The page also wires up interaction it cannot use here: a zoom overlay it
// builds and appends, and document-level listeners. None of it affects what
// gets rendered, but it runs during boot, so it has to find something rather
// than throw and take the whole check down with it. These stubs exist to let
// boot reach the end, not to stand in for a browser: nothing below is
// asserted on.
const detached = () => ({
  className: "",
  hidden: false,
  style: {},
  classList: { add() {}, remove() {}, contains: () => false },
  appendChild() {},
  addEventListener() {},
  closest: () => null,
});

globalThis.document = {
  querySelector: (sel) => node(sel),
  // Nothing in the check inspects elements the page creates or the nodes it
  // sweeps for a class, so an empty sweep and a detached element are the
  // honest answers.
  querySelectorAll: () => [],
  createElement: () => detached(),
  addEventListener() {},
  body: { appendChild() {} },
};
globalThis.fetch = async (path) => {
  try {
    const body = readFileSync(`${site}/${path}`, "utf8");
    return { ok: true, json: async () => JSON.parse(body) };
  } catch {
    return { ok: false, status: 404 };
  }
};

let index = { runs: [] };
try {
  index = JSON.parse(readFileSync(`${site}/runs/index.json`, "utf8"));
} catch {
  console.error("render check: no runs/index.json, nothing to verify");
  process.exit(1);
}
if (!index.runs?.length) {
  console.error("render check: runs/index.json lists no runs");
  process.exit(1);
}

new Function(script)();
await new Promise((r) => setTimeout(r, 250));
const out = node("#main").innerHTML;

// A family the collector marked anything other than absent has data, so it
// owes the page a section. `absent` is honest and is allowed to draw nothing.
const newest = JSON.parse(readFileSync(`${site}/runs/${index.runs[0].file}`, "utf8"));
const SECTION = {
  throughput: "Throughput",
  isolation: "Transaction isolation",
  correctness: "Transaction correctness",
  rss: "Resident memory",
  binary_size: "Binary size",
  allocs: "Allocations per operation",
};

const failures = [];

// A section that throws is not a section that is missing, and until this
// existed it was not a failure either. `section(name, build)` in perf.html
// catches every throw and emits `<h2>${name}</h2>` plus a `card broken`
// panel, using the very heading this check matches on, so the failure output
// satisfied the success assertion. Look for the panel first, and name it.
for (const m of out.matchAll(/<div class="card broken">\s*<h3>([^<]*)<\/h3>\s*<p class="note">([^<]*)<\/p>/g)) {
  failures.push(`${m[1]}: ${m[2]}`);
}
if (/class="card broken"/.test(out) && !failures.length) {
  failures.push("a section rendered as a broken card, in a shape this check could not name");
}

// Match the heading tag, not a bare substring. `emptyState` prints five of
// these six names as table cells, so a substring test is satisfied by a page
// that rendered no data at all.
for (const [family, heading] of Object.entries(SECTION)) {
  const m = newest.metrics?.[family];
  if (!m || m.trust === "absent") continue;
  if (!out.includes(`<h2>${heading}</h2>`)) {
    failures.push(`${family} was collected (trust=${m.trust ?? "per-entry"}) but "${heading}" is not on the page`);
  }
}
for (const [family, data] of Object.entries(newest.metrics?.timings || {})) {
  if (data.trust === "absent" || !data.rows?.length) continue;
  const heading = family.replaceAll("_", " ");
  if (!out.includes(`<h3>${heading}<span`) && !out.includes(`<h3>${heading}</h3>`)) {
    failures.push(`${family} timing rows were collected but have no card`);
  }
}
if (!out.includes("Trend across every recorded run")) {
  failures.push("the trend section is missing");
}
if (out.length < 2000) {
  failures.push(`the page rendered only ${out.length} bytes, which is not a populated dashboard`);
}

const cards = (out.match(/<h3>/g) || []).length;
console.log(`render check: ${out.length} bytes, ${cards} cards, ${index.runs.length} run(s) on file`);

if (failures.length) {
  for (const f of failures) console.error(`render check: ${f}`);
  process.exit(1);
}
