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
    // WebView2 on Windows; WebKit on macOS/Linux. Not lower than safari15:
    // esbuild can't lower the destructuring in Monaco's workers to safari13,
    // which failed every macOS/Linux build. Safari 15 runs on macOS 10.15+.
    target: process.env.TAURI_ENV_PLATFORM === 'windows' ? 'chrome105' : 'safari15',
    minify: !process.env.TAURI_ENV_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    // The bundle (Monaco and the editor's languages) ships inside the app
    // and loads from disk, so the 500 kB advice for pages served over the
    // network doesn't apply; the limit is raised above what it is today.
    chunkSizeWarningLimit: 6000,
  },
});
