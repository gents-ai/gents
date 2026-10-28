/* The PR Explorer page (packs repo: packs/gents/pr_explorer_ux) through the
   real runtime pipeline: allowlist, its own frame over the real port
   protocol, the pack's declared gate, and ctx.data into the pr_explorer
   surface. The plugin lives in the packs checkout, so this suite runs only
   when GENTS_PACKS_ROOT points at it (a spike: see the pack's README). */
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fireEvent, render, screen } from "@testing-library/react";
import { createElement, Fragment } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  loadRuntimeUxPlugin,
  retireRuntimeUxPlugin,
  unsupportedImports,
} from "@/contrib/loader";
import { installUxHost, type UxHost } from "@/contrib/plugin";
import {
  resetUxPluginStore,
  setUxPluginEnabled,
  uxPluginRecords,
} from "@/contrib/plugins-store";
import { registry } from "@/contrib/registry";
import {
  NAV_AREA,
  PLUGIN_PAGE_AREA,
  TRANSCRIPT_DIRECTIVE_AREA,
  type TranscriptDirectiveContribution,
} from "@/contrib/types";
import { ALLOWED_SPECIFIERS, resetSdkImportMap } from "@/sdk/runtime";

import {
  frameMount,
  installInProcessSandbox,
  settle,
  uninstallInProcessSandbox,
} from "./sandbox-fixture";

const root = process.env.GENTS_PACKS_ROOT ?? "";
const UX = "packs/gents/pr_explorer_ux";
const DOCS = "packs/gents/pr_explorer";
const present = root !== "" && existsSync(join(root, UX, "manifest.json"));
const read = (rel: string) => readFileSync(join(root, rel), "utf8");
const LISTING = "gents/pr_explorer_ux/pr_explorer";
const DECLARED = {
  areas: [NAV_AREA, PLUGIN_PAGE_AREA, TRANSCRIPT_DIRECTIVE_AREA],
  directives: ["pr-explorer"],
};

type Row = Record<string, unknown>;
type Call = [string, string, string, string, Record<string, unknown>];

const A_OLD = "aaaaaaaaaaaa-20261001000000";
const A_NEW = "bbbbbbbbbbbb-20261002000000";
const A_PACKS = "cccccccccccc-20261003000000";
const analyses: Row[] = [
  {
    analysis_id: A_OLD,
    repository: "gents",
    pr_ref: "main...old",
    base_sha: "1111111111",
    head_sha: "aaaaaaaaaaaa",
    file_count: 1,
    unit_count: 1,
    created_at: "2026-10-01T00:00:00Z",
  },
  {
    analysis_id: A_NEW,
    repository: "gents",
    pr_ref: "main...feature",
    base_sha: "2222222222",
    head_sha: "bbbbbbbbbbbb",
    file_count: 4,
    unit_count: 2,
    created_at: "2026-10-02T00:00:00Z",
  },
  {
    analysis_id: A_PACKS,
    repository: "packs",
    pr_ref: "main...x",
    base_sha: "3333333333",
    head_sha: "cccccccccccc",
    file_count: 1,
    unit_count: 1,
    created_at: "2026-10-03T00:00:00Z",
  },
];
const DIFF_SCHEMA =
  "diff --git a/src/schema.rs b/src/schema.rs\nindex 1..2 100644\n--- a/src/schema.rs\n+++ b/src/schema.rs\n@@ -10,3 +10,3 @@ struct S {\n     a: u8,\n-    b: u8,\n+    b: u16,\n     c: <img src=x onerror=alert(1)>,\n";
const files: Row[] = [
  {
    analysis_id: A_NEW,
    ordinal: 0,
    path: "src/schema.rs",
    old_path: "src/schema.rs",
    change: "M",
    binary: false,
    additions: 1,
    deletions: 1,
    diff_bytes: DIFF_SCHEMA.length,
    truncated: false,
    diff: DIFF_SCHEMA,
  },
  {
    analysis_id: A_NEW,
    ordinal: 1,
    path: "migrations/003.sql",
    old_path: "migrations/003.sql",
    change: "A",
    binary: false,
    additions: 2,
    deletions: 0,
    diff_bytes: 900_000,
    truncated: true,
    diff: "diff --git a/migrations/003.sql b/migrations/003.sql\nnew file mode 100644\n--- /dev/null\n+++ b/migrations/003.sql\n@@ -0,0 +1,2 @@\n+CREATE TABLE t;\n+-- cut here\n",
  },
  {
    analysis_id: A_NEW,
    ordinal: 2,
    path: "tests/schema.rs",
    old_path: "tests/schema.rs",
    change: "M",
    binary: false,
    additions: 3,
    deletions: 0,
    diff_bytes: 10,
    truncated: false,
    diff: "",
  },
  {
    analysis_id: A_NEW,
    ordinal: 3,
    path: "README.md",
    old_path: "README.md",
    change: "M",
    binary: false,
    additions: 1,
    deletions: 1,
    diff_bytes: 10,
    truncated: false,
    diff: "",
  },
];
const units: Row[] = [
  /* ordinals out of list order: the page sorts */
  {
    analysis_id: A_NEW,
    unit_id: "tests",
    ordinal: 1,
    title: "Tests",
    summary: "Covers the wider field.",
    files: JSON.stringify(["tests/schema.rs", "src/schema.rs"]),
  },
  {
    analysis_id: A_NEW,
    unit_id: "schema",
    ordinal: 0,
    title: "Schema change <b>bold</b>",
    summary: "Widens b and adds a migration.",
    files: JSON.stringify(["src/schema.rs", "migrations/003.sql", "ghost.rs"]),
  },
];

function dataHost(calls: Call[], rows: { analyses: Row[] } = { analyses }) {
  return {
    call: async (
      listing: string,
      nodeDid: string,
      surface: string,
      tool: string,
      args: Record<string, unknown>,
    ) => {
      calls.push([listing, nodeDid, surface, tool, args]);
      const pick = (all: Row[]) =>
        all.filter((r) => r.analysis_id === args.analysis_id);
      let results: Row[] = [];
      if (tool === "pr_list_analyses") results = rows.analyses;
      if (tool === "pr_list_units") results = pick(units);
      if (tool === "pr_list_files") {
        results = pick(files).filter(
          (f) => args.path === undefined || f.path === args.path,
        );
        if (Array.isArray(args.fields)) {
          const keep = args.fields as string[];
          results = results.map((r) =>
            Object.fromEntries(Object.entries(r).filter(([k]) => keep.includes(k))),
          );
        }
      }
      return { kind: "query" as const, output: { results, count: results.length } };
    },
    surfaces: async () => [],
  };
}

const sent: string[] = [];
const navigated: unknown[] = [];
const opened: string[] = [];
function host(calls: Call[], rows?: { analyses: Row[] }): UxHost {
  return {
    shell: {
      route: () => ({ name: "sessions" }),
      navigate: (route) => void navigated.push(route),
      deployments: () => [],
      selectedDeployment: () => ({ nodeDid: "did:key:n" }) as never,
      selectedSessionId: () => "s1",
      send: async (content) => void sent.push(content),
    },
    plugin: async () => ({}),
    os: {
      openExternal: async (url) => {
        opened.push(url);
        return true;
      },
      revealPath: async () => true,
      pickDirectory: async () => null,
    },
    data: dataHost(calls, rows),
  };
}

async function load(declared = DECLARED) {
  const id = await loadRuntimeUxPlugin(
    {
      source: read(`${UX}/ux/pr_explorer/plugin.js`),
      css: read(`${UX}/ux/pr_explorer/plugin.css`),
    },
    {
      origin: LISTING,
      door: "pack",
      pack: "gents/pr_explorer_ux",
      listing: LISTING,
      declared,
    },
  );
  /* null: the pipeline refused it (its record says why) */
  if (id !== null) await setUxPluginEnabled(id, true);
  return id;
}

async function openPage() {
  const page = registry.getArea(PLUGIN_PAGE_AREA)[0]!;
  const view = render(createElement(Fragment, null, page.render!()));
  await settle(12);
  return { view, mount: frameMount(LISTING)! };
}

const text = (el: Element | null | undefined) => el?.textContent ?? "";

beforeEach(() => {
  sent.length = 0;
  opened.length = 0;
  navigated.length = 0;
  resetUxPluginStore();
  resetSdkImportMap();
  installInProcessSandbox();
});
afterEach(() => {
  for (const id of Object.keys(uxPluginRecords())) retireRuntimeUxPlugin(id);
  uninstallInProcessSandbox();
  vi.restoreAllMocks();
});

describe.skipIf(!present)("PR Explorer plugin (GENTS_PACKS_ROOT)", () => {
  it("imports only the allowlisted specifiers and declares only the surface's query tools", () => {
    expect(
      unsupportedImports(read(`${UX}/ux/pr_explorer/plugin.js`), ALLOWED_SPECIFIERS),
    ).toEqual([]);
    const manifest = JSON.parse(read(`${UX}/manifest.json`)) as {
      kind: string;
      ux: Array<{
        contributes: { areas: string[]; directives: string[] };
        data: Array<{ surface: string; tools: string[] }>;
      }>;
    };
    expect(manifest.kind).toBe("assets");
    expect(manifest.ux[0]!.contributes).toEqual(DECLARED);
    const declared = manifest.ux[0]!.data;
    expect(declared).toEqual([
      {
        surface: "pr_explorer",
        tools: ["pr_list_analyses", "pr_list_units", "pr_list_files"],
      },
    ]);
    /* each declared tool is a query tool of the documents pack's surface */
    const docs = JSON.parse(read(`${DOCS}/pack_config.json`)) as {
      datastore_tool_surfaces: Array<{
        surface_id: string;
        entries: Array<{ tool_name: string; kind?: string }>;
      }>;
    };
    const surface = docs.datastore_tool_surfaces.find(
      (s) => s.surface_id === "pr_explorer",
    )!;
    const kinds = Object.fromEntries(
      surface.entries.map((e) => [e.tool_name, e.kind ?? "create"]),
    );
    for (const tool of declared[0]!.tools) expect(kinds[tool], tool).toBe("query");
  });

  it("loads under its declared gate: a nav row to its page, the page, the ::pr-explorer chip", async () => {
    installUxHost(host([]));
    expect(await load()).toBe("pr_explorer");
    expect(uxPluginRecords().pr_explorer?.status).toBe("loaded");
    const nav = registry.getArea(NAV_AREA).find((c) => c.id === "pr_explorer:nav");
    const to = (nav?.data as { to: (ctx: unknown) => unknown }).to;
    expect(to({ mailboxCount: 0, selectedNodeDid: null })).toEqual({
      name: "plugin",
      pluginId: "pr_explorer",
    });
    expect(registry.getArea(PLUGIN_PAGE_AREA).map((c) => c.id)).toEqual([
      "pr_explorer:page",
    ]);
    const chip = registry.getArea(TRANSCRIPT_DIRECTIVE_AREA)[0]!
      .data as TranscriptDirectiveContribution;
    expect(chip.name).toBe("pr-explorer");
  });

  it("is refused when its pack does not declare the directive", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    installUxHost(host([]));
    await load({ areas: DECLARED.areas, directives: [] });
    expect(uxPluginRecords().pr_explorer?.status).toBe("error");
  });

  it("with no analysis, shows the empty state and sends the analyze prompt only on submit", async () => {
    const calls: Call[] = [];
    installUxHost(host(calls, { analyses: [] }));
    await load();
    const { view, mount } = await openPage();
    expect(text(mount)).toContain("No analysis yet");
    expect(calls.map((c) => c[3])).toEqual(["pr_list_analyses"]);
    expect(calls[0]!.slice(0, 3)).toEqual([LISTING, "did:key:n", "pr_explorer"]);
    /* nothing is sent by mounting or refreshing */
    expect(sent).toEqual([]);

    const input = (label: string) =>
      mount.querySelector(`input[aria-label='${label}']`)!;
    const submit = () =>
      mount.querySelector("form button[type=submit]") as HTMLButtonElement;
    /* a path that climbs out of the root never becomes a prompt */
    fireEvent.change(input("Repository"), { target: { value: "../etc" } });
    await settle();
    expect(submit().disabled).toBe(true);
    fireEvent.submit(mount.querySelector("form")!);
    await settle();
    expect(sent).toEqual([]);

    fireEvent.change(input("Repository"), { target: { value: "gents" } });
    fireEvent.change(input("Base"), { target: { value: "main" } });
    fireEvent.change(input("Head"), { target: { value: "feature/x" } });
    await settle();
    expect(submit().disabled).toBe(false);
    fireEvent.submit(mount.querySelector("form")!);
    await settle();
    expect(sent).toEqual(["Analyze main...feature/x in gents with PR Explorer."]);
    expect(text(mount)).toContain("Sent to the selected session.");
    view.unmount();
  });

  it("draws the newest analysis as units and the selected file's diff, as text", async () => {
    const calls: Call[] = [];
    installUxHost(host(calls));
    await load();
    const { view, mount } = await openPage();

    /* newest analysis of the first repository (gents), not packs' */
    const listCalls = calls.filter((c) => c[3] !== "pr_list_analyses");
    expect(listCalls.find((c) => c[3] === "pr_list_units")![4]).toEqual({
      analysis_id: A_NEW,
    });
    const fileList = listCalls.find(
      (c) => c[3] === "pr_list_files" && c[4].path === undefined,
    )!;
    expect(fileList[4].analysis_id).toBe(A_NEW);
    /* the file list never pulls diff text */
    expect(fileList[4].fields).not.toContain("diff");

    /* the page takes the app palette, not the frame's marketing fallback */
    expect(mount.querySelector(".gents-prx")?.getAttribute("data-context")).toBe("app");

    /* units in ordinal order, then the file no unit claims */
    const unitIds = [...mount.querySelectorAll("[data-unit]")].map((el) =>
      el.getAttribute("data-unit"),
    );
    expect(unitIds).toEqual(["schema", "tests", "__unassigned"]);
    /* model text is text: the <b> in a title is not markup */
    const schema = mount.querySelector("[data-unit=schema]")!;
    expect(schema.querySelector("b")).toBeNull();
    expect(text(schema)).toContain("Schema change <b>bold</b>");
    expect(text(mount.querySelector("[data-unit=__unassigned]"))).toContain(
      "README.md",
    );

    /* the first unit opens on its first file; a path no file has is marked */
    expect(schema.querySelector("[aria-expanded=true]")).not.toBeNull();
    expect(text(schema)).toContain("missing");
    const diffCall = calls.find(
      (c) => c[3] === "pr_list_files" && c[4].path === "src/schema.rs",
    );
    expect(diffCall?.[4]).toEqual({ analysis_id: A_NEW, path: "src/schema.rs" });

    const rows = [...mount.querySelectorAll(".gents-prx-diff tr")];
    const kinds = rows.map((r) => r.className.replace("gents-prx-", ""));
    expect(kinds).toEqual([
      "meta",
      "meta",
      "meta",
      "meta",
      "hunk",
      "ctx",
      "del",
      "add",
      "ctx",
    ]);
    const cells = (r: Element) =>
      [...r.querySelectorAll("td")].map((td) => td.textContent);
    expect(cells(rows[5]!)).toEqual(["10", "10", "     a: u8,"]);
    expect(cells(rows[6]!)).toEqual(["11", "", "-    b: u8,"]);
    expect(cells(rows[7]!)).toEqual(["", "11", "+    b: u16,"]);
    expect(cells(rows[8]!)).toEqual([
      "12",
      "12",
      "     c: <img src=x onerror=alert(1)>,",
    ]);
    /* diff text is text: no element came out of it */
    expect(mount.querySelector(".gents-prx-diff img")).toBeNull();

    /* the second unit: src/schema.rs again is flagged, not drawn twice as live */
    fireEvent.click(mount.querySelector("[data-unit=tests] .gents-prx-unit-head")!);
    await settle();
    const dup = mount.querySelector(
      "[data-unit=tests] [data-path='src/schema.rs']",
    ) as HTMLButtonElement;
    expect(dup.disabled).toBe(true);
    expect(text(dup)).toContain("dup");

    /* a path with no stored diff stays clickable and says why */
    const ghost = mount.querySelector(
      "[data-unit=schema] [data-path='ghost.rs']",
    ) as HTMLButtonElement;
    expect(ghost.disabled).toBe(false);
    fireEvent.click(ghost);
    await settle(12);
    expect(text(mount.querySelector(".gents-prx-pane"))).toContain(
      "No diff is stored for this file",
    );
    expect(ghost.getAttribute("aria-current")).toBe("true");

    /* a truncated file says so */
    fireEvent.click(
      mount.querySelector("[data-unit=schema] [data-path='migrations/003.sql']")!,
    );
    await settle(12);
    expect(calls.some((c) => c[4].path === "migrations/003.sql")).toBe(true);
    expect(text(mount.querySelector(".gents-prx-pane .gents-prx-notice"))).toMatch(
      /Truncated: this file's diff is 879 KiB/,
    );
    /* every stored file is listed: no missing-diffs notice */
    expect(mount.querySelector(".gents-prx-notice-warn")).toBeNull();
    expect(sent).toEqual([]);
    view.unmount();
  });

  it("an analysis recorded without its file diffs says so, and its files stay clickable", async () => {
    /* the first live run: units and an analysis, but every plugin write refused */
    const A_BARE = "dddddddddddd-20261027222830";
    const calls: Call[] = [];
    const bare = {
      analysis_id: A_BARE,
      repository: "packs",
      pr_ref: "master...e901",
      base_sha: "4d23967",
      head_sha: "dddddddddddd",
      file_count: 468,
      unit_count: 1,
      created_at: "2026-10-27T22:31:56Z",
    };
    units.push({
      analysis_id: A_BARE,
      unit_id: "ci",
      ordinal: 0,
      title: "CI",
      summary: "s",
      files: JSON.stringify(["Makefile", "scripts/test-pack.sh"]),
    });
    try {
      installUxHost(host(calls, { analyses: [bare] }));
      await load();
      const { view, mount } = await openPage();
      expect(text(mount.querySelector(".gents-prx-notice-warn"))).toContain(
        "Only 0 of 468 changed files have a stored diff",
      );
      const file = mount.querySelector(
        "[data-unit=ci] [data-path='scripts/test-pack.sh']",
      ) as HTMLButtonElement;
      expect(file.disabled).toBe(false);
      fireEvent.click(file);
      await settle(12);
      expect(file.getAttribute("aria-current")).toBe("true");
      expect(text(mount.querySelector(".gents-prx-pane"))).toContain(
        "scripts/test-pack.sh",
      );
      expect(sent).toEqual([]);
      view.unmount();
    } finally {
      units.pop();
    }
  });

  it("styles only with tokens the app defines, and pairs every hover background with its foreground", () => {
    const css = read(`${UX}/ux/pr_explorer/plugin.css`);
    /* the app's raw names are --text and --bg; these two do not exist */
    expect(css).not.toMatch(/var\(--foreground\)|var\(--background\)/);
    for (const rule of css.match(/[^{}]*:hover[^{}]*\{[^}]*\}/g) ?? []) {
      if (/background:\s*var\(--accent\)/.test(rule))
        expect(rule, rule).toMatch(/color:\s*var\(--accent-foreground\)/);
    }
    /* diffs use the design system's functional diff tints */
    for (const token of [
      "--diff-added",
      "--diff-added-fg",
      "--diff-removed",
      "--diff-removed-fg",
    ]) {
      expect(css).toContain(`var(${token})`);
    }
    /* the page fills its frame and scrolls inside, instead of a fixed short block */
    expect(css).toMatch(/\.gents-prx \{[^}]*height: 100vh/);
    expect(css).not.toMatch(/calc\(100vh/);
    /* sized for the full-width pane: a unit list that scales with the window,
       1rem unit titles, and diff text no smaller than .875rem */
    expect(css).toMatch(/grid-template-columns: clamp\(20rem, 28%, 32rem\)/);
    expect(css).toMatch(/\.gents-prx-unit-title \{[^}]*font-size: 1rem/);
    expect(css).toMatch(/\.gents-prx-diff \{[^}]*font-size: \.875rem/);
  });

  it("sends a GitHub PR link as a canonical review request, and refuses other links", async () => {
    installUxHost(host([], { analyses: [] }));
    await load();
    const { view, mount } = await openPage();
    const link = mount.querySelector("input[aria-label='PR link']") as HTMLInputElement;
    const submit = () =>
      mount.querySelector("form button[type=submit]") as HTMLButtonElement;
    const repoInput = () =>
      mount.querySelector("input[aria-label=Repository]") as HTMLInputElement;

    /* the base no longer defaults to main: a repo without one never gets a bad default */
    expect(
      (mount.querySelector("input[aria-label=Base]") as HTMLInputElement).value,
    ).toBe("");

    for (const bad of [
      "https://gitlab.com/gents-ai/gents/pull/3189",
      "http://github.com/gents-ai/gents/pull/3189",
      "https://github.com/gents-ai/gents/issues/3189",
      "https://github.com/gents-ai/../pull/1",
      "javascript:alert(1)//github.com/a/b/pull/1",
      "gents-ai/gents#0",
    ]) {
      fireEvent.change(link, { target: { value: bad } });
      await settle();
      expect(submit().disabled, bad).toBe(true);
      expect(link.getAttribute("aria-invalid"), bad).toBe("true");
      expect(text(mount)).toContain("Not a github.com pull request link.");
    }
    fireEvent.submit(mount.querySelector("form")!);
    await settle();
    expect(sent).toEqual([]);

    /* anything after the number is dropped: the prompt carries the canonical URL */
    fireEvent.change(link, {
      target: {
        value: "  https://github.com/gents-ai/gents/pull/3189/files?diff=split#x  ",
      },
    });
    await settle();
    expect(submit().disabled).toBe(false);
    expect(repoInput().disabled).toBe(true);
    expect(text(mount)).toContain("gents-ai/gents #3189");
    fireEvent.submit(mount.querySelector("form")!);
    await settle();

    fireEvent.change(link, { target: { value: "gents-ai/packs#42" } });
    await settle();
    fireEvent.submit(mount.querySelector("form")!);
    await settle();
    expect(sent).toEqual([
      "Review https://github.com/gents-ai/gents/pull/3189 with PR Explorer.",
      "Review https://github.com/gents-ai/packs/pull/42 with PR Explorer.",
    ]);

    /* clearing the link hands the form back to the range */
    fireEvent.change(link, { target: { value: "" } });
    await settle();
    expect(repoInput().disabled).toBe(false);
    view.unmount();
  });

  it("labels a PR analysis owner/repo #n and opens only its canonical GitHub URL", async () => {
    const A_PR = "fc7bae9e1c2d-20261028090000";
    const A_ODD = "eeeeeeeeeeee-20261028080000";
    const rows = [
      {
        analysis_id: A_PR,
        repository: "gents",
        pr_ref: "https://github.com/gents-ai/gents/pull/3189",
        base_sha: "e6bbdedb78",
        head_sha: "fc7bae9e1c2d",
        file_count: 0,
        unit_count: 0,
        created_at: "2026-10-28T09:00:00Z",
      },
      /* a model-written pr_ref that only looks like a link: no button, shown as text */
      {
        analysis_id: A_ODD,
        repository: "gents",
        pr_ref: "https://evil.example/gents-ai/gents/pull/1",
        base_sha: "1",
        head_sha: "eeeeeeeeeeee",
        file_count: 0,
        unit_count: 0,
        created_at: "2026-10-28T08:00:00Z",
      },
    ];
    installUxHost(host([], { analyses: rows }));
    await load();
    const { view, mount } = await openPage();
    const options = [
      ...(mount.querySelector("select[aria-label=Analysis]") as HTMLSelectElement)
        .options,
    ].map((o) => o.textContent);
    expect(options).toEqual([
      "gents-ai/gents #3189 · 2026-10-28T09:00:00Z",
      "https://evil.example/gents-ai/gents/pull/1 · 2026-10-28T08:00:00Z",
    ]);
    const openButton = () =>
      [...mount.querySelectorAll("button")].find(
        (b) => b.textContent === "Open on GitHub",
      );
    fireEvent.click(openButton()!);
    await settle();
    expect(opened).toEqual(["https://github.com/gents-ai/gents/pull/3189"]);

    fireEvent.change(mount.querySelector("select[aria-label=Analysis]")!, {
      target: { value: A_ODD },
    });
    await settle(12);
    expect(openButton()).toBeUndefined();
    expect(opened).toHaveLength(1);
    expect(sent).toEqual([]);
    view.unmount();
  });

  it("switches repository and analysis from the pickers", async () => {
    const calls: Call[] = [];
    installUxHost(host(calls));
    await load();
    const { view, mount } = await openPage();
    const repoSelect = mount.querySelector(
      "select[aria-label=Repository]",
    ) as HTMLSelectElement;
    expect([...repoSelect.options].map((o) => o.value)).toEqual(["gents", "packs"]);
    const analysisSelect = () =>
      mount.querySelector("select[aria-label=Analysis]") as HTMLSelectElement;
    expect([...analysisSelect().options].map((o) => o.value)).toEqual([A_NEW, A_OLD]);

    fireEvent.change(analysisSelect(), { target: { value: A_OLD } });
    await settle(12);
    expect(
      calls.some((c) => c[3] === "pr_list_units" && c[4].analysis_id === A_OLD),
    ).toBe(true);

    fireEvent.change(repoSelect, { target: { value: "packs" } });
    await settle(12);
    expect([...analysisSelect().options].map((o) => o.value)).toEqual([A_PACKS]);
    expect(
      calls.some((c) => c[3] === "pr_list_units" && c[4].analysis_id === A_PACKS),
    ).toBe(true);
    view.unmount();
  });

  it("the chip opens the page on the analysis it names, and ignores a malformed id", async () => {
    const calls: Call[] = [];
    installUxHost(host(calls));
    await load();
    const chip = registry.getArea(TRANSCRIPT_DIRECTIVE_AREA)[0]!
      .data as TranscriptDirectiveContribution;

    const chipView = render(
      createElement(
        Fragment,
        null,
        chip.render({
          attrs: { analysis: A_OLD },
          source: `::pr-explorer{analysis="${A_OLD}"}`,
          streaming: false,
        }),
      ),
    );
    expect(screen.getByTestId("ux-directive-chip")).toHaveTextContent(
      `PR Explorer · ${A_OLD}`,
    );
    fireEvent.click(screen.getByTestId("ux-directive-chip"));
    await settle();
    expect(navigated).toEqual([{ name: "plugin", pluginId: "pr_explorer" }]);
    chipView.unmount();
    const first = await openPage();
    expect(calls.find((c) => c[3] === "pr_list_units")![4]).toEqual({
      analysis_id: A_OLD,
    });
    first.view.unmount();

    calls.length = 0;
    const bad = render(
      createElement(
        Fragment,
        null,
        chip.render({
          attrs: { analysis: "../../x" },
          source: '::pr-explorer{analysis="../../x"}',
          streaming: false,
        }),
      ),
    );
    fireEvent.click(screen.getByTestId("ux-directive-chip"));
    await settle();
    bad.unmount();
    const second = await openPage();
    expect(calls.find((c) => c[3] === "pr_list_units")![4]).toEqual({
      analysis_id: A_NEW,
    });
    second.view.unmount();
  });
});
