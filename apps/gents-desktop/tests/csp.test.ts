/* The webview's Content Security Policy is the control the UX-plugin lint
   stands in front of: a loaded module runs with the app's authority, so what
   keeps it from pulling a second stage is that the policy admits script only
   from the app itself and from the blob: URLs the loader mints. These pin the
   shape of `app.security.csp` and `devCsp` in tauri.conf.json so an edit
   cannot quietly reopen it. */
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

type Policy = Record<string, string[]>;

function parse(csp: string): Policy {
  const out: Policy = {};
  for (const directive of csp.split(";")) {
    const [name, ...sources] = directive.trim().split(/\s+/);
    if (name) out[name] = sources;
  }
  return out;
}

const conf = JSON.parse(
  readFileSync(join(__dirname, "../src-tauri/tauri.conf.json"), "utf8"),
) as { app: { security: { csp: string | null; devCsp?: string | null } } };

const prod = parse(conf.app.security.csp ?? "");
const dev = parse(conf.app.security.devCsp ?? conf.app.security.csp ?? "");

describe("the webview CSP", () => {
  it("is set, and admits script only from the app and from the loader's blob: modules", () => {
    expect(conf.app.security.csp).toBeTruthy();
    expect(prod["script-src"]).toEqual(["'self'", "blob:"]);
    expect(prod["default-src"]).toEqual(["'self'"]);
    expect(prod["object-src"]).toEqual(["'none'"]);
    expect(prod["frame-src"]).toEqual(["'none'"]);
    expect(prod["base-uri"]).toEqual(["'self'"]);
  });

  it("lets the Tauri IPC through and nothing else over the network", () => {
    expect(prod["connect-src"]).toEqual(["'self'", "ipc:", "http://ipc.localhost"]);
    for (const [name, sources] of Object.entries(prod)) {
      for (const source of sources) {
        expect(source, `${name} ${source}`).not.toMatch(/^(\*|https?:|wss?:)$/);
        expect(source, `${name} ${source}`).not.toMatch(
          /^https?:\/\/(?!ipc\.localhost)/,
        );
      }
    }
  });

  it("keeps 'unsafe-eval' out of both policies", () => {
    for (const policy of [prod, dev]) {
      for (const sources of Object.values(policy)) {
        expect(sources).not.toContain("'unsafe-eval'");
      }
    }
  });

  it("relaxes the dev policy only for Vite: inline scripts and its own server", () => {
    expect(dev["script-src"]).toEqual(["'self'", "blob:", "'unsafe-inline'"]);
    const extra = (dev["connect-src"] ?? []).filter(
      (s) => !(prod["connect-src"] ?? []).includes(s),
    );
    expect(extra).toEqual(["ws://localhost:1420", "http://localhost:1420"]);
    for (const name of Object.keys(prod)) {
      if (name === "script-src" || name === "connect-src") continue;
      expect(dev[name], name).toEqual(prod[name]);
    }
  });
});
