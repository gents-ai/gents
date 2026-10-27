import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

/* Tauri applies `app.security.csp` only to the pages it serves itself (the
   built dist); under `devUrl` the webview takes whatever Vite sends. So when
   the Tauri CLI spawned this server, Vite sends the same `devCsp` header,
   and a plugin that `tauri dev` admits is one the built app admits too. The
   Playwright harness runs Vite on its own and gets no header. */
const underTauri = Boolean(process.env.TAURI_ENV_PLATFORM);
const devCsp = (): string | undefined => {
  const conf = JSON.parse(
    readFileSync(
      fileURLToPath(new URL("./src-tauri/tauri.conf.json", import.meta.url)),
      "utf8",
    ),
  ) as { app?: { security?: { csp?: string | null; devCsp?: string | null } } };
  return conf.app?.security?.devCsp ?? conf.app?.security?.csp ?? undefined;
};

export default defineConfig(async ({ mode }) => ({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src/ui", import.meta.url)),
      "@gents/ux-sdk": fileURLToPath(new URL("./src/ui/sdk/index.ts", import.meta.url)),
    },
  },
  optimizeDeps: {
    include: ["@gents/shell"],
  },
  /* the diff view's worker loads its highlighter in pieces, which only
     module workers can split */
  worker: { format: "es" as const },

  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr:
      mode === "browser-test"
        ? false
        : host
          ? {
              protocol: "ws",
              host,
              port: 1421,
            }
          : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
    headers: underTauri ? { "Content-Security-Policy": devCsp() } : undefined,
  },
}));
