/* Two policies hold the plugin boundary: the shell's `app.security.csp`
   (script from the app and in-realm blobs only; frames from the app only)
   and the frame's own, composed by the shell with nothing an opaque origin
   could not satisfy. These pin both shapes so an edit cannot quietly reopen
   either. */
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import {
  FRAME_BOOTSTRAP,
  frameNonce,
  RUNTIME_JS_URL,
  sandboxCsp,
  sandboxDocument,
} from "@/contrib/frame-document";

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

const csp = conf.app.security.csp ?? "";
const prod = parse(csp);
const directives = parse;
const dev = parse(conf.app.security.devCsp ?? conf.app.security.csp ?? "");

describe("the webview CSP", () => {
  it("is set, and admits script only from the app, in-realm blobs and the frame bootstrap, and WebAssembly", () => {
    expect(conf.app.security.csp).toBeTruthy();
    /* 'wasm-unsafe-eval' lets the shell compile WebAssembly, for the diff
       view's highlighter; it admits no eval and no inline script */
    expect(prod["script-src"]).toEqual([
      "'self'",
      "blob:",
      "'wasm-unsafe-eval'",
      expect.stringMatching(/^'sha256-[A-Za-z0-9+/=]+'$/),
    ]);
    expect(prod["default-src"]).toEqual(["'self'"]);
    expect(prod["object-src"]).toEqual(["'none'"]);
    /* the plugin frames are the shell's own srcdoc documents and nothing else */
    expect(prod["frame-src"]).toEqual(["'self'"]);
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
    expect(dev["script-src"]).toEqual([
      "'self'",
      "blob:",
      "'wasm-unsafe-eval'",
      "'unsafe-inline'",
    ]);
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

describe("the sandbox document", () => {
  /* the frame's origin is opaque: 'self' would match nothing, and a module
     request from it fails CORS — so the frame's own policy names no origin
     at all. A srcdoc frame also inherits the shell's policy, which admits
     the one inline bootstrap by hash and the runtime's blob by scheme. */
  const nonce = frameNonce();
  const policy = directives(sandboxCsp(nonce));

  it("carries a policy of its own that reaches no origin, no network and no frame", () => {
    expect(policy["default-src"]).toEqual(["'none'"]);
    /* no 'wasm-unsafe-eval': a frame obeys its own policy and the shell's,
       so plugins compile no WebAssembly though the shell may */
    expect(policy["script-src"]).toEqual([`'nonce-${nonce}'`, "blob:"]);
    expect(policy["connect-src"]).toEqual(["'none'"]);
    expect(policy["frame-src"]).toEqual(["'none'"]);
    expect(policy["form-action"]).toEqual(["'none'"]);
    for (const sources of Object.values(policy)) {
      expect(sources).not.toContain("'self'");
      expect(sources).not.toContain("'unsafe-eval'");
      expect(sources.some((s) => /^https?:|^tauri:/.test(s))).toBe(false);
    }
  });

  it("inlines only the bootstrap and the stylesheet, and fetches nothing", () => {
    const html = sandboxDocument(nonce, "body{}</style><style>");
    expect(html).toContain(
      `<meta http-equiv="Content-Security-Policy" content="${sandboxCsp(nonce)}" />`,
    );
    expect(html).toContain(`<script nonce="${nonce}">${FRAME_BOOTSTRAP}</script>`);
    expect(html).not.toMatch(/<script[^>]*\ssrc=/);
    expect(html).not.toMatch(/<link\s/);
    /* a closing tag inside the inlined stylesheet cannot end the element early */
    expect(html.match(/<\/style>/g)).toHaveLength(1);
    expect(html.match(/<\/script>/g)).toHaveLength(1);
  });

  it("mints a fresh nonce per frame", () => {
    expect(frameNonce()).not.toBe(frameNonce());
    expect(frameNonce()).toMatch(/^[A-Za-z0-9]{16,}$/);
  });

  it("is admitted by the shell's built policy by the hash of its exact text", () => {
    const hash = `'sha256-${createHash("sha256").update(FRAME_BOOTSTRAP).digest("base64")}'`;
    expect(prod["script-src"]).toEqual(["'self'", "blob:", "'wasm-unsafe-eval'", hash]);
    /* the dev policy keeps 'unsafe-inline' for Vite; a hash beside it would
       switch that off and break the dev server's own inline scripts */
    expect(dev["script-src"]).toEqual([
      "'self'",
      "blob:",
      "'wasm-unsafe-eval'",
      "'unsafe-inline'",
    ]);
    expect(FRAME_BOOTSTRAP).toBe(
      readFileSync(join(__dirname, "../src/ui/sandbox/bootstrap.js"), "utf8"),
    );
    /* the bootstrap stays what the hash says: no fetch, no import, a blob
       from the message, the parent as the only sender */
    expect(FRAME_BOOTSTRAP).not.toMatch(/fetch\(|import\(|XMLHttpRequest/);
    expect(FRAME_BOOTSTRAP).toContain("event.source !== window.parent");
    expect(FRAME_BOOTSTRAP).toContain(
      'new Blob([data.runtime], { type: "text/javascript" })',
    );
  });

  it("is proven in a browser under the built policy, not a looser one", () => {
    const harness = readFileSync(join(__dirname, "ui-harness/sandbox.html"), "utf8");
    const meta = /http-equiv="Content-Security-Policy"\s+content="([^"]+)"/.exec(
      harness,
    );
    expect(meta?.[1]).toBe(csp);
  });

  it("is built as one classic script the shell serves from its own origin", () => {
    const config = readFileSync(join(__dirname, "../vite.sandbox.config.ts"), "utf8");
    expect(config).toContain('formats: ["iife"]');
    expect(config).toContain("public/ux-sandbox");
    const pkg = JSON.parse(
      readFileSync(join(__dirname, "../package.json"), "utf8"),
    ) as {
      scripts: Record<string, string>;
    };
    expect(pkg.scripts["build:sandbox"]).toContain("vite.sandbox.config.ts");
    expect(pkg.scripts.predev).toContain("build:sandbox");
    expect(pkg.scripts.build).toContain("build:sandbox");
    expect(RUNTIME_JS_URL).toBe("/ux-sandbox/runtime.js");
  });
});
