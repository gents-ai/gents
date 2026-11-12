import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

const here = (path: string) => fileURLToPath(new URL(path, import.meta.url));

/* The page `gents ux preview` drives: the real frame pipeline and plugin
   screen under the shell's own policy, so what it shows is what the app
   would. Output: dist-ux-preview (gitignored), which also carries the
   sandbox runtime from public/. `npm run build:ux-preview`. */
function shellPolicy(): Plugin {
  const { app } = JSON.parse(
    readFileSync(here("./src-tauri/tauri.conf.json"), "utf8"),
  ) as {
    app: { security: { csp: string } };
  };
  return {
    name: "gents-shell-policy",
    transformIndexHtml: {
      order: "pre",
      handler: (html) =>
        html.replace(
          "<!--gents:csp-->",
          `<meta http-equiv="Content-Security-Policy" content="${app.security.csp}" />`,
        ),
    },
  };
}

export default defineConfig({
  plugins: [react(), tailwindcss(), shellPolicy()],
  resolve: {
    alias: {
      "@": here("./src/ui"),
      "@gents/ux-sdk": here("./src/ui/sdk/index.ts"),
    },
  },
  worker: { format: "es" as const },
  build: {
    outDir: here("./dist-ux-preview"),
    emptyOutDir: true,
    rollupOptions: { input: here("./ux-preview.html") },
  },
});
