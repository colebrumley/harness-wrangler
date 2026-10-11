import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In `npm run dev` outside Tauri, API calls go to a running `wrangler serve`.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    proxy: { "/api": { target: "http://127.0.0.1:4477", changeOrigin: false } },
  },
  build: { target: "es2021", outDir: "dist" },
});
