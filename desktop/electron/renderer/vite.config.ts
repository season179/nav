import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const rendererRoot = fileURLToPath(new URL(".", import.meta.url));

export default defineConfig({
  root: rendererRoot,
  base: "./",
  plugins: [react()],
  build: {
    emptyOutDir: true,
    outDir: "dist",
    rolldownOptions: {
      output: {
        codeSplitting: true,
      },
    },
    sourcemap: true,
  },
});
