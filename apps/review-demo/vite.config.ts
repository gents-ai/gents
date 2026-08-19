import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const runtimePort = process.env.REVIEW_PORT || "19191";
const runtime = `http://127.0.0.1:${runtimePort}`;

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: Number(process.env.REVIEW_PAGE_PORT || 19190),
    strictPort: true,
    host: "127.0.0.1",
    open: true,
    proxy: {
      "/api": { target: runtime, changeOrigin: true },
      "/healthz": { target: runtime, changeOrigin: true },
    },
  },
  preview: {
    port: Number(process.env.REVIEW_PAGE_PORT || 19190),
    strictPort: true,
  },
  build: {
    target: "esnext",
    outDir: "dist",
  },
});
