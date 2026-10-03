/// <reference types="vitest/config" />
import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// `bun run dev` serves the UI with hot reload and proxies the API to a
// running `isb serve` (ISB_URL, default http://127.0.0.1:8092). For
// passkeys, start that daemon with ISB_PUBLIC_URL=http://localhost:5173 so
// the relying party matches the page's origin.
const target = process.env.ISB_URL ?? "http://127.0.0.1:8092";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  build: {
    // No source maps in the binary; the build is small enough to read.
    sourcemap: false,
    chunkSizeWarningLimit: 800,
  },
  server: {
    host: "127.0.0.1",
    port: 5173,
    strictPort: true,
    proxy: {
      "/api": { target },
      "/healthz": { target },
      // Org-bound tools, and the terminal's websocket.
      "^/orgs/[^/]+/api/": { target, ws: true },
    },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
