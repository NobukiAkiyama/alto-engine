import { defineConfig } from "vite";

export default defineConfig({
  // The build is served from a subfolder.
  base: "./",
  // The engine's Workers import modules and start Workers of their own.
  worker: { format: "es" },
});
