import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

/* The plugin sandbox runtime as one classic script. A plugin's frame has an
   opaque origin, which can neither pass CORS for a module request nor match
   'self' in a policy, so the shell inlines this file into the frame
   document (src/ui/contrib/frame-document.ts) instead of letting the frame
   fetch it. Output: public/ux-sandbox/runtime.js (+ runtime.css), served
   from the shell's own origin in dev and copied into dist by the main
   build; gitignored. `npm run build:sandbox`, run before dev and build. */
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src/ui", import.meta.url)),
      "@gents/ux-sdk": fileURLToPath(new URL("./src/ui/sdk/index.ts", import.meta.url)),
    },
  },
  define: {
    "process.env.NODE_ENV": JSON.stringify("production"),
  },
  /* the output lands inside public/, so public/ must not be copied into it */
  publicDir: false,
  build: {
    outDir: fileURLToPath(new URL("./public/ux-sandbox", import.meta.url)),
    emptyOutDir: true,
    sourcemap: false,
    cssCodeSplit: false,
    lib: {
      entry: fileURLToPath(new URL("./src/ui/sandbox/main.tsx", import.meta.url)),
      name: "GentsUxSandbox",
      formats: ["iife"],
      fileName: () => "runtime.js",
      cssFileName: "runtime",
    },
    rollupOptions: {
      output: {
        inlineDynamicImports: true,
      },
    },
  },
});
