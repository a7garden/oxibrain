import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // The console bundle ships inside the oxibrain-mcp crate (ADR-008):
  // `cargo install oxibrain-cli` serves it with no Node toolchain, and cargo
  // package must find it inside the crate so the published tarball embeds it.
  build: {
    outDir: "../../crates/oxibrain-mcp/assets/dist",
    emptyOutDir: true,
  },
  server: {
    proxy: {
      "/api": {
        target: "http://127.0.0.1:18080",
        changeOrigin: true,
      },
    },
  },
});
