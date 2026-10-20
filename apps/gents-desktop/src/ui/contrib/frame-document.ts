/* The frame document a plugin runs in, composed by the shell. The frame's
   origin is opaque (sandbox="allow-scripts" and nothing else), and an
   opaque origin can neither pass CORS for a module request nor match
   'self' in a policy, so nothing is fetched from inside it: the document
   carries the runtime's stylesheet and one inline bootstrap, the runtime's
   text travels in the connect message, and the bootstrap mints an in-frame
   blob for it. A srcdoc frame inherits the shell's policy as well as its
   own; the bootstrap passes the shell's by hash, the blob by scheme. */
import bootstrap from "../sandbox/bootstrap.js?raw";

export const RUNTIME_JS_URL = "/ux-sandbox/runtime.js";
export const RUNTIME_CSS_URL = "/ux-sandbox/runtime.css";

/** the inline bootstrap, byte for byte what the shell's policy hashes */
export const FRAME_BOOTSTRAP: string = bootstrap;

export interface RuntimeAssets {
  js: string;
  css: string;
}

/** the frame's own policy: no network at all, script only the bootstrap
    (by nonce) and in-frame blobs, styles inline (the runtime's stylesheet
    is inlined) */
export function sandboxCsp(nonce: string): string {
  return [
    "default-src 'none'",
    `script-src 'nonce-${nonce}' blob:`,
    "style-src 'unsafe-inline'",
    "img-src data: blob:",
    "font-src data:",
    "connect-src 'none'",
    "frame-src 'none'",
    "object-src 'none'",
    "base-uri 'none'",
    "form-action 'none'",
  ].join("; ");
}

/* a closing tag inside inlined text would end the element early; the
   escaped form reads the same to the CSS and JavaScript parsers */
const escapeClose = (text: string, tag: string) =>
  text.replace(new RegExp(`</(${tag})`, "gi"), "<\\/$1");

export function sandboxDocument(nonce: string, css: string): string {
  return [
    "<!doctype html>",
    '<html lang="en" data-context="ux-sandbox">',
    "<head>",
    '<meta charset="UTF-8" />',
    `<meta http-equiv="Content-Security-Policy" content="${sandboxCsp(nonce)}" />`,
    '<meta name="viewport" content="width=device-width, initial-scale=1.0" />',
    "<title>Gents plugin</title>",
    `<style>${escapeClose(css, "style")}</style>`,
    "</head>",
    "<body>",
    '<div id="root"></div>',
    `<script nonce="${nonce}">${FRAME_BOOTSTRAP}</script>`,
    "</body>",
    "</html>",
  ].join("\n");
}

export function frameNonce(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return btoa(String.fromCharCode(...bytes)).replace(/[+/=]/g, "");
}

let assets: Promise<RuntimeAssets> | null = null;

/** the runtime, fetched from the shell's own origin once per session */
export function loadRuntimeAssets(): Promise<RuntimeAssets> {
  assets ??= (async () => {
    const [js, css] = await Promise.all([
      fetch(RUNTIME_JS_URL).then((response) => {
        if (!response.ok) {
          throw new Error(
            `the sandbox runtime is not served at ${RUNTIME_JS_URL} (${response.status}); run npm run build:sandbox`,
          );
        }
        return response.text();
      }),
      fetch(RUNTIME_CSS_URL).then((response) => (response.ok ? response.text() : "")),
    ]);
    return { js, css };
  })().catch((error: unknown) => {
    /* a failed fetch is not cached: the next frame tries again */
    assets = null;
    throw error;
  });
  return assets;
}

/** test seam */
export function resetRuntimeAssets(): void {
  assets = null;
}
