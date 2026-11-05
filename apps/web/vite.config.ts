import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  server: {
    // Allow Vite to serve the WASM package, which lives outside apps/web.
    fs: { allow: ["../.."] },
  },
  build: {
    target: "es2023",
  },
});
