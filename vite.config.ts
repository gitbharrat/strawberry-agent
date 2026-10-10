import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import { fileURLToPath, URL } from "node:url";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [solid()],
  resolve: {
    alias: { "~": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  // Tauri expects a fixed port and fails if it is not available.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    // The system webview is evergreen on every platform we ship to, so we can
    // emit modern output and skip the transpile/polyfill tax entirely.
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome110" : "safari15",
    minify: !process.env.TAURI_ENV_DEBUG,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    cssMinify: true,
    reportCompressedSize: false,
    rollupOptions: {
      output: {
        // The three heavy optional dependencies get their own chunks, so a
        // user who never opens a terminal, a diagram or a formula never
        // downloads or parses them.
        manualChunks(id: string) {
          if (id.includes("@xterm")) return "xterm";
          if (id.includes("mermaid") || id.includes("dagre") || id.includes("cytoscape")) return "mermaid";
          if (id.includes("katex")) return "katex";
          return undefined;
        },
      },
    },
  },
});
