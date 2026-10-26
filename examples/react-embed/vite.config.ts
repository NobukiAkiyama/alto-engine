import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  base: "./",
  plugins: [react()],
  // The engine's Workers import modules and start Workers of their own.
  worker: { format: "es" },
});
