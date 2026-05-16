import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// PRD §3 locks Vite 5+ and SolidJS 1.9+ as the frontend build/runtime stack.
// `clearScreen=false` and the `1420` dev port match the Tauri 2 conventions so
// `cargo tauri dev` can pipe logs through cleanly later (F-001 / F-018).
export default defineConfig({
  plugins: [solid()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "es2022",
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: true,
  },
});
