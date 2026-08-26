import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The development proxy preserves the same-origin API contract used by the packaged desktop app.
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8787",
        ws: true,
      },
    },
  },
  build: {
    sourcemap: true,
  },
});
