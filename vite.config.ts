import { defineConfig } from 'vite';
import solid from 'vite-plugin-solid';

// Tauri expects a fixed dev server port and will fail if it's occupied.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [solid()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    watch: {
      // don't watch the Rust side, tauri's own watcher handles that
      ignored: ['**/src-tauri/**'],
    },
  },
  // keep bundles lean; no need to split vendor chunks for an app this size
  build: {
    target: process.env.TAURI_ENV_PLATFORM === 'windows' ? 'chrome105' : 'safari13',
    minify: !process.env.TAURI_ENV_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
});
