import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { viteSingleFile } from "vite-plugin-singlefile";
import fs from "node:fs";

// Backend (Rust gml-server) that vite proxies API/SSE calls to during `npm run dev`.
// In production everything is inlined into one index.html served by gml-server,
// so no proxy is involved there. Override with GM_BACKEND_URL if needed.
const BACKEND = process.env.GM_BACKEND_URL || "http://127.0.0.1:8000";
const API_PATHS = [
  "/state",
  "/transcript",
  "/export",
  "/models",
  "/settings",
  "/turn",
  "/tts",
  "/transcribe",
  "/cmd",
  "/model",
  "/codex",
  "/debug",
  "/chats",
  "/stories",
  "/world-architect",
  "/story-architect",
  "/sidecar",
];

// Optional LAN + HTTPS dev so a phone can connect WITH hot-reload (and mic,
// which the browser only grants over a secure context). Opt-in via env, so the
// plain `npm run dev` stays localhost/HTTP for desktop work:
//   GM_DEV_HOST=1             expose the dev server on the LAN (binds 0.0.0.0)
//   GM_DEV_CERT / GM_DEV_KEY  PEM paths -> serve HTTPS (reuse gml-server's .tls,
//                             whose SAN already includes the machine's LAN IP)
const lan = !!process.env.GM_DEV_HOST;
const certPath = process.env.GM_DEV_CERT;
const keyPath = process.env.GM_DEV_KEY;
const httpsCfg =
  certPath && keyPath && fs.existsSync(certPath) && fs.existsSync(keyPath)
    ? { cert: fs.readFileSync(certPath), key: fs.readFileSync(keyPath) }
    : undefined;

export default defineConfig({
  plugins: [react(), viteSingleFile()],
  server: {
    host: lan ? true : "localhost",
    https: httpsCfg,
    // Accessing the dev server by LAN IP/hostname trips vite's host check.
    allowedHosts: lan ? true : undefined,
    port: 5173,
    proxy: Object.fromEntries(
      API_PATHS.map((p) => [
        p,
        { target: BACKEND, changeOrigin: true, ws: false, secure: false },
      ])
    ),
  },
  build: {
    // Emit a single, fully-inlined index.html into web/dist/. gml-server serves
    // this file (and Tauri's webview loads it via the embedded loopback server).
    outDir: "dist",
    emptyOutDir: true,
    assetsInlineLimit: 100_000_000,
    chunkSizeWarningLimit: 100_000,
  },
});
