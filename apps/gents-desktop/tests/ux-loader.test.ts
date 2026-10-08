import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { isUxPluginLive } from "@/contrib/activate";
import {
  codeRanges,
  loadRuntimeUxPlugin,
  retireRuntimeUxPlugin,
  rewriteSpecifiers,
  unsupportedImports,
} from "@/contrib/loader";
import { installUxHost, type UxHost } from "@/contrib/plugin";
import { resetUxPluginStore, uxPluginRecords } from "@/contrib/plugins-store";
import { registry } from "@/contrib/registry";
import { SESSION_HEADER_ACTIONS_AREA } from "@/contrib/types";
import { resetSdkImportMap } from "@/sdk/runtime";

const ALLOWED = ["@gents/ux-sdk", "react", "react/jsx-runtime"];

const host: UxHost = {
  shell: {
    route: () => ({ name: "sessions" }),
    navigate: () => {},
    deployments: () => [],
    selectedDeployment: () => null,
    selectedSessionId: () => null,
    send: async () => {},
  },
  bridge: async () => ({}) as never,
  os: {
    openExternal: async () => true,
    revealPath: async () => true,
    pickDirectory: async () => null,
  },
};

/* the loader evaluates a plugin through a blob-URL import(), which the test
   module runner cannot resolve; reroute to a data: URL node's native ESM
   loader handles (the same shape hermes-agent's loader tests use). The SDK
   shims take the same path, so a plugin's `@gents/ux-sdk` import resolves
   to the live namespace end to end. */
const RealBlob = globalThis.Blob;
class FakeBlob {
  parts: string[];
  constructor(parts: string[]) {
    this.parts = parts;
  }
}

beforeEach(() => {
  installUxHost(host);
  resetUxPluginStore();
  resetSdkImportMap();
  vi.spyOn(console, "error").mockImplementation(() => {});
  vi.stubGlobal("Blob", FakeBlob);
  URL.createObjectURL = (blob: unknown) =>
    `data:text/javascript;base64,${Buffer.from((blob as FakeBlob).parts.join("")).toString("base64")}`;
  URL.revokeObjectURL = () => undefined;
});
afterEach(() => {
  for (const id of Object.keys(uxPluginRecords())) retireRuntimeUxPlugin(id);
  vi.stubGlobal("Blob", RealBlob);
  vi.restoreAllMocks();
});

describe("import lexing", () => {
  it("finds imports only in code, not in strings or comments", () => {
    const src = [
      `import { a } from '@gents/ux-sdk'`,
      `// import 'lodash'`,
      `const s = "from 'left-pad'"`,
      `/* import('evil') */`,
      "const t = `import 'x' ${ import('react') }`",
      `const re = /from 'nope'/`,
    ].join("\n");
    expect(unsupportedImports(src, ALLOWED)).toEqual([]);
  });

  it("reports every unsupported specifier once", () => {
    const src = `import x from 'lodash'\nimport y from 'lodash'\nimport('https://evil/x.js')\nimport './rel.js'`;
    expect(unsupportedImports(src, ALLOWED).sort()).toEqual(
      ["./rel.js", "https://evil/x.js", "lodash"].sort(),
    );
  });

  it("rewrites mapped specifiers in code and leaves the same text in a string alone", () => {
    const map = { react: "blob:r", "@gents/ux-sdk": "blob:s" };
    const src = `import r from 'react'\nconst msg = "import r from 'react'"`;
    const out = rewriteSpecifiers(src, map);
    expect(out).toBe(`import r from 'blob:r'\nconst msg = "import r from 'react'"`);
  });

  it("treats a division as code and a regex as not-code", () => {
    const ranges = codeRanges(`const a = b / c; const d = /x'y/g; e`);
    const text = (r: [number, number]) =>
      `const a = b / c; const d = /x'y/g; e`.slice(r[0], r[1]);
    const joined = ranges.map(text).join("|");
    expect(joined).toContain("b / c");
    expect(joined).not.toContain("x'y");
  });
});

describe("runtime loader", () => {
  const sdkModule = (body: string) =>
    `import { SESSION_HEADER_ACTIONS_AREA } from '@gents/ux-sdk'\n${body}`;

  it("loads a plain ESM plugin, registers it and lands a loaded row", async () => {
    const source = sdkModule(`export default {
      id: 'rt', name: 'Runtime', register(ctx) {
        ctx.register({ id: 'chip', area: SESSION_HEADER_ACTIONS_AREA, render: () => null })
      } }`);
    const id = await loadRuntimeUxPlugin(
      { source },
      { origin: "rt", door: "dev", file: "/x/plugin.js" },
    );
    expect(id).toBe("rt");
    expect(uxPluginRecords().rt?.status).toBe("loaded");
    expect(uxPluginRecords().rt?.door).toBe("dev");
    expect(registry.getArea(SESSION_HEADER_ACTIONS_AREA).map((c) => c.id)).toEqual([
      "rt:chip",
    ]);
    expect(isUxPluginLive("rt")).toBe(true);
  });

  it("refuses a module with an unsupported import before evaluating it", async () => {
    const source = `import _ from 'lodash'\nexport default { id: 'bad', register() {} }`;
    const id = await loadRuntimeUxPlugin(
      { source },
      { origin: "bad-folder", door: "dev" },
    );
    expect(id).toBeNull();
    const row = uxPluginRecords()["bad-folder"];
    expect(row?.status).toBe("error");
    expect(row?.error).toMatch(/unsupported import.*lodash/);
  });

  it("lands a register() throw on the plugin's own row with nothing left registered", async () => {
    const source = sdkModule(`export default { id: 'thrower', register(ctx) {
      ctx.register({ id: 'a', area: SESSION_HEADER_ACTIONS_AREA, render: () => null })
      throw new Error('mid-way')
    } }`);
    await loadRuntimeUxPlugin({ source }, { origin: "thrower", door: "dev" });
    expect(uxPluginRecords().thrower?.status).toBe("error");
    expect(uxPluginRecords().thrower?.error).toBe("mid-way");
    expect(registry.getArea(SESSION_HEADER_ACTIONS_AREA)).toHaveLength(0);
    expect(isUxPluginLive("thrower")).toBe(false);
  });

  it("rejects a module without a default GentsUxPlugin", async () => {
    await loadRuntimeUxPlugin(
      { source: `export const x = 1` },
      { origin: "nope", door: "dev" },
    );
    expect(uxPluginRecords().nope?.error).toMatch(/no valid default/);
  });

  it("enforces the declared-contributions gate from the listing", async () => {
    const source = sdkModule(`export default { id: 'gated', register(ctx) {
      ctx.register({ id: 'chip', area: SESSION_HEADER_ACTIONS_AREA, render: () => null })
    } }`);
    await loadRuntimeUxPlugin(
      { source },
      {
        origin: "gated",
        door: "pack",
        pack: "ns/p",
        declared: { areas: ["nav"], directives: [] },
      },
    );
    expect(uxPluginRecords().gated?.status).toBe("error");
    expect(uxPluginRecords().gated?.error).toMatch(/does not declare/);
  });

  it("reloading the same id disposes the previous incarnation first", async () => {
    const v = (label: string) =>
      sdkModule(`export default { id: 'twice', register(ctx) {
        ctx.register({ id: 'chip', area: SESSION_HEADER_ACTIONS_AREA, title: '${label}', render: () => null })
      } }`);
    await loadRuntimeUxPlugin(
      { source: v("one") },
      { origin: "twice", door: "dev", file: "/t" },
    );
    await loadRuntimeUxPlugin(
      { source: v("two") },
      { origin: "twice", door: "dev", file: "/t" },
    );
    const area = registry.getArea(SESSION_HEADER_ACTIONS_AREA);
    expect(area).toHaveLength(1);
    expect(area[0]?.title).toBe("two");
  });

  it("installs and removes a plugin's css with its lifetime", async () => {
    const source = sdkModule(`export default { id: 'styled', register() {} }`);
    await loadRuntimeUxPlugin(
      { source, css: ".x{color:red}" },
      { origin: "styled", door: "dev" },
    );
    expect(document.querySelector('style[data-gents-ux="styled"]')?.textContent).toBe(
      ".x{color:red}",
    );
    retireRuntimeUxPlugin("styled");
    expect(document.querySelector('style[data-gents-ux="styled"]')).toBeNull();
  });

  it("ships opt-in when the root caps defaultEnabled, without losing the row", async () => {
    const source = sdkModule(`export default { id: 'optin', register(ctx) {
      ctx.register({ id: 'c', area: SESSION_HEADER_ACTIONS_AREA, render: () => null }) } }`);
    await loadRuntimeUxPlugin(
      { source },
      { origin: "optin", door: "pack", defaultEnabled: false },
    );
    expect(uxPluginRecords().optin?.status).toBe("disabled");
    expect(registry.getArea(SESSION_HEADER_ACTIONS_AREA)).toHaveLength(0);
  });
});
