import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// Tauri serves the frontend from a fixed port in development and from `dist/` in a bundle.
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    // The WebView is Edge on Windows and WebKit elsewhere; both are current enough for ES2021.
    target: "es2021",
    sourcemap: false,
    // Nothing is fetched at runtime, so everything must be inlined or emitted locally: the
    // CSP forbids remote origins entirely.
    assetsInlineLimit: 8192,
  },
  test: {
    environment: "node",
    include: ["src/**/*.spec.ts"],
  },
});
