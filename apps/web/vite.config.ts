import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  server: {
    // Allow Vite to serve the WASM package, which lives outside apps/web.
    fs: { allow: ["../.."] },
    // The API runs on port 8080 (cargo run -p api).
    proxy: { "/v1": "http://127.0.0.1:8080" },
  },
  build: {
    target: "es2023",
  },
});
