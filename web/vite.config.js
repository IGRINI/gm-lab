import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { viteSingleFile } from "vite-plugin-singlefile";

// Backend endpoints proxied to the Python server during `npm run dev`.
// In production everything is inlined into one index.html served by server.py,
// so no proxy is involved there.
const BACKEND = "http://127.0.0.1:8000";
const API_PATHS = [
  "/state",
  "/transcript",
  "/export",
  "/models",
  "/settings",
  "/turn",
  "/cmd",
  "/model",
  "/codex",
  "/debug",
];

export default defineConfig({
  plugins: [react(), viteSingleFile()],
  server: {
    port: 5173,
    proxy: Object.fromEntries(
      API_PATHS.map((p) => [
        p,
        { target: BACKEND, changeOrigin: true, ws: false },
      ])
    ),
  },
  build: {
    // Emit a single, fully-inlined index.html into the repo root so that
    // server.py keeps serving `HERE/index.html` with zero changes.
    outDir: "..",
    emptyOutDir: false,
    assetsInlineLimit: 100_000_000,
    chunkSizeWarningLimit: 100_000,
  },
});
